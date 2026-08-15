//! One machine record: heap, frames, arenas, fuel, policy table,
//! pending perform, and terminal storage.
//!
//! `exec_instr` retires exactly one instruction of this machine. It
//! never runs another machine and never recurses: every operation
//! that reaches outside the machine returns an `ExecOutcome` for the
//! world driver.

use crate::heap::{Heap, Object};
use crate::{FaultCode, VmConfig};
use lm_bytecode::{Instr, Module};
use lm_value::{ObjRef, Value};

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
    Done,
    Faulted,
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
    /// The host completion token while `waiting`.
    pub wait_token: Option<u64>,
}

/// A stored machine fault.
#[derive(Debug, Clone)]
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
    /// `request.as_call(op)`.
    AsCall { request: ObjRef, op: u32 },
    /// `call.args()`.
    CallArgs { call: ObjRef },
}

/// One machine.
pub struct Machine {
    pub heap: Heap,
    pub frames: Vec<Frame>,
    pub locals: Vec<Value>,
    pub operands: Vec<Value>,
    pub fuel: u64,
    pub config: VmConfig,
    pub state: MachineState,
    pub pending: Option<Pending>,
    pub terminal: Option<Terminal>,
    pub table: PolicyTable,
    pub parent: Option<VmId>,
    pub next_ordinal: u64,
    /// The number of live activation references to this machine on
    /// the driver stack. A machine with references rejects control
    /// methods.
    pub active: u32,
}

impl Machine {
    /// A machine without a loaded entry.
    pub fn empty(config: VmConfig, parent: Option<VmId>) -> Machine {
        Machine {
            heap: Heap::new(config.heap_bytes),
            frames: Vec::new(),
            locals: Vec::new(),
            operands: Vec::new(),
            fuel: config.fuel,
            config,
            state: MachineState::Empty,
            pending: None,
            terminal: None,
            table: PolicyTable::default(),
            parent,
            next_ordinal: 1,
            active: 0,
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
    ) {
        let local_count = module.funcs[func as usize].local_count() as usize;
        if local_count > self.config.max_stack_values as usize {
            self.set_fault(
                FaultCode::StackLimit,
                "the initial frame exceeds the arena",
                None,
            );
            return;
        }
        self.locals = args;
        self.locals.resize(local_count, Value::Unit);
        self.operands.clear();
        self.frames.push(Frame {
            func,
            block: 0,
            ip: 0,
            base_local: 0,
            base_operand: 0,
            closure,
        });
        self.state = MachineState::Ready;
    }

    pub fn set_done(&mut self, value: Value) {
        self.terminal = Some(Terminal::Done(value));
        self.state = MachineState::Done;
        self.pending = None;
    }

    pub fn set_fault(&mut self, code: FaultCode, message: impl Into<String>, op: Option<u32>) {
        self.terminal = Some(Terminal::Fault(FaultRec {
            code,
            message: message.into(),
            op,
        }));
        self.state = MachineState::Faulted;
        self.pending = None;
    }

    /// Collect garbage now. `extra` holds additional roots that are
    /// not yet stored in the arenas.
    pub fn collect_garbage(&mut self, extra: &[ObjRef]) {
        let mut roots: Vec<ObjRef> = Vec::new();
        for value in self.locals.iter().chain(self.operands.iter()) {
            if let Value::Obj(r) = value {
                roots.push(*r);
            }
        }
        for frame in &self.frames {
            if let Some(r) = frame.closure {
                roots.push(r);
            }
        }
        if let Some(pending) = &self.pending {
            for value in &pending.args {
                if let Value::Obj(r) = value {
                    roots.push(*r);
                }
            }
        }
        if let Some(Terminal::Done(Value::Obj(r))) = &self.terminal {
            roots.push(*r);
        }
        for action in self.table.exact.iter().chain(self.table.group.iter()) {
            if let Some(Action::Mock(r)) = action {
                roots.push(*r);
            }
        }
        roots.extend_from_slice(extra);
        self.heap.collect(roots);
    }

    /// Allocate one object. When the cap would be exceeded, collect
    /// first. The children of the new object are roots during the
    /// collection because they are not yet reachable from the arenas.
    pub fn alloc(&mut self, object: Object) -> Result<Value, FaultCode> {
        let cost = object.cost();
        if self.heap.would_exceed(cost) {
            let mut extra = Vec::new();
            object.trace_children(&mut extra);
            self.collect_garbage(&extra);
            if self.heap.would_exceed(cost) {
                return Err(FaultCode::HeapLimit);
            }
        }
        Ok(Value::Obj(self.heap.alloc(object)))
    }

    /// Make room for `delta` more bytes of growth on an existing
    /// object. `temps` holds values already popped from the arenas.
    fn reserve(&mut self, delta: usize, temps: &[Value]) -> Result<(), FaultCode> {
        if self.heap.would_exceed(delta) {
            let extra: Vec<ObjRef> = temps.iter().filter_map(|v| v.as_obj()).collect();
            self.collect_garbage(&extra);
            if self.heap.would_exceed(delta) {
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
                match (self.heap.get(x), self.heap.get(y)) {
                    (Object::Str(s1), Object::Str(s2)) => s1 == s2,
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Find the entry position of a key in a map.
    fn map_find(&self, entries: &[(Value, Value)], key: Value) -> Option<usize> {
        entries.iter().position(|(k, _)| self.key_eq(*k, key))
    }

    fn frozen_guard(&self, r: ObjRef) -> Result<(), FaultCode> {
        if self.heap.is_frozen(r) {
            Err(FaultCode::FrozenWrite)
        } else {
            Ok(())
        }
    }

    /// Execute exactly one instruction of the current frame.
    pub fn exec_instr(
        &mut self,
        module: &Module,
        dispatch: &[Vec<u32>],
    ) -> Result<ExecOutcome, FaultCode> {
        if self.fuel == 0 {
            return Err(FaultCode::OutOfFuel);
        }
        self.fuel -= 1;
        let frame = self.frames.last().expect("an active frame exists");
        let func = &module.funcs[frame.func as usize];
        let instr = func.blocks[frame.block as usize][frame.ip as usize];
        self.frames.last_mut().expect("an active frame exists").ip += 1;
        match instr {
            Instr::ConstUnit => self.push(Value::Unit)?,
            Instr::ConstBool(v) => self.push(Value::Bool(v))?,
            Instr::ConstInt(v) => self.push(Value::Int(v))?,
            Instr::ConstStr(idx) => {
                let text = module.strings[idx as usize].clone();
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::LoadLocal(slot) => {
                let base = self.frames.last().expect("frame").base_local;
                let value = self.locals[(base + slot) as usize];
                self.push(value)?;
            }
            Instr::StoreLocal(slot) => {
                let value = self.pop();
                let base = self.frames.last().expect("frame").base_local;
                self.locals[(base + slot) as usize] = value;
            }
            Instr::Pop => {
                self.pop();
            }
            Instr::Add => self.int_binary(i64::checked_add)?,
            Instr::Sub => self.int_binary(i64::checked_sub)?,
            Instr::Mul => self.int_binary(i64::checked_mul)?,
            Instr::Div => {
                let b = self.pop_int();
                let a = self.pop_int();
                if b == 0 {
                    return Err(FaultCode::DivideByZero);
                }
                let value = a.checked_div(b).ok_or(FaultCode::IntegerOverflow)?;
                self.push(Value::Int(value))?;
            }
            Instr::Rem => {
                let b = self.pop_int();
                let a = self.pop_int();
                if b == 0 {
                    return Err(FaultCode::DivideByZero);
                }
                let value = a.checked_rem(b).ok_or(FaultCode::IntegerOverflow)?;
                self.push(Value::Int(value))?;
            }
            Instr::Neg => {
                let a = self.pop_int();
                let value = a.checked_neg().ok_or(FaultCode::IntegerOverflow)?;
                self.push(Value::Int(value))?;
            }
            Instr::Not => {
                let a = self.pop_bool();
                self.push(Value::Bool(!a))?;
            }
            Instr::LtInt => self.int_compare(|a, b| a < b)?,
            Instr::LeInt => self.int_compare(|a, b| a <= b)?,
            Instr::GtInt => self.int_compare(|a, b| a > b)?,
            Instr::GeInt => self.int_compare(|a, b| a >= b)?,
            Instr::EqInt => self.int_compare(|a, b| a == b)?,
            Instr::NeInt => self.int_compare(|a, b| a != b)?,
            Instr::EqBool => {
                let b = self.pop_bool();
                let a = self.pop_bool();
                self.push(Value::Bool(a == b))?;
            }
            Instr::NeBool => {
                let b = self.pop_bool();
                let a = self.pop_bool();
                self.push(Value::Bool(a != b))?;
            }
            Instr::EqStr => self.str_compare(true)?,
            Instr::NeStr => self.str_compare(false)?,
            Instr::EqRef => {
                let b = self.pop_obj();
                let a = self.pop_obj();
                self.push(Value::Bool(a == b))?;
            }
            Instr::NeRef => {
                let b = self.pop_obj();
                let a = self.pop_obj();
                self.push(Value::Bool(a != b))?;
            }
            Instr::Call(callee) | Instr::CallG { func: callee, .. } => {
                let argc = module.funcs[callee as usize].params.len();
                self.push_frame(module, callee, argc, None)?;
            }
            Instr::CallVirtual { selector, argc } | Instr::CallVirtualG { selector, argc, .. } => {
                let argc = argc as usize;
                let recv = self.operands[self.operands.len() - 1 - argc];
                let class = match self.heap.get(recv.as_obj().expect("verified receiver")) {
                    Object::Instance { class, .. } => *class,
                    _ => unreachable!("verified receiver shape"),
                };
                let target = dispatch[class as usize][selector as usize];
                self.push_frame(module, target, argc + 1, None)?;
            }
            Instr::CallValue { argc } => {
                let argc = argc as usize;
                let callee_pos = self.operands.len() - 1 - argc;
                let callee = self.operands.remove(callee_pos);
                let r = callee.as_obj().expect("verified closure value");
                let target = match self.heap.get(r) {
                    Object::Closure { func, .. } => *func,
                    _ => unreachable!("verified closure shape"),
                };
                self.push_frame(module, target, argc, Some(r))?;
            }
            Instr::MakeClosure { func, captures } => {
                let split = self.operands.len() - captures as usize;
                let captured: Vec<Value> = self.operands.split_off(split);
                let value = self.alloc(Object::Closure {
                    func,
                    captures: captured,
                })?;
                self.push(value)?;
            }
            Instr::LoadCapture(idx) => {
                let frame = self.frames.last().expect("frame");
                let closure = frame.closure.expect("verified capture context");
                let value = match self.heap.get(closure) {
                    Object::Closure { captures, .. } => captures[idx as usize],
                    _ => unreachable!("verified closure shape"),
                };
                self.push(value)?;
            }
            Instr::New(class) | Instr::NewG { class, .. } => {
                let field_count = module.classes[class as usize].fields.len();
                let value = self.alloc(Object::Instance {
                    class,
                    fields: vec![Value::Uninit; field_count],
                })?;
                self.push(value)?;
            }
            Instr::TupleNew { count, .. } => {
                let split = self.operands.len() - count as usize;
                let items: Vec<Value> = self.operands.split_off(split);
                let value = self.alloc(Object::Tuple { items })?;
                self.push(value)?;
            }
            Instr::TupleGet(index) => {
                let r = self.pop_obj();
                let value = match self.heap.get(r) {
                    Object::Tuple { items } => items[index as usize],
                    _ => unreachable!("verified tuple shape"),
                };
                self.push(value)?;
            }
            Instr::IsType(ty) => {
                let r = self.pop_obj();
                let matches = self.instance_matches(module, r, ty);
                self.push(Value::Bool(matches))?;
            }
            Instr::CastType(ty) => {
                let r = self.pop_obj();
                if !self.instance_matches(module, r, ty) {
                    return Err(FaultCode::BadCast);
                }
                self.push(Value::Obj(r))?;
            }
            Instr::LoadField(field) => {
                let r = self.pop_obj();
                let value = match self.heap.get(r) {
                    Object::Instance { fields, .. } => fields[field as usize],
                    _ => unreachable!("verified instance shape"),
                };
                if value == Value::Uninit {
                    return Err(FaultCode::UninitializedField);
                }
                self.push(value)?;
            }
            Instr::StoreField(field) => {
                let value = self.pop();
                let r = self.pop_obj();
                self.frozen_guard(r)?;
                match self.heap.get_mut(r) {
                    Object::Instance { fields, .. } => fields[field as usize] = value,
                    _ => unreachable!("verified instance shape"),
                }
            }
            Instr::ListNew { count, .. } => {
                let split = self.operands.len() - count as usize;
                let items: Vec<Value> = self.operands.split_off(split);
                let value = self.alloc(Object::List { items })?;
                self.push(value)?;
            }
            Instr::ListLen => {
                let r = self.pop_obj();
                let len = match self.heap.get(r) {
                    Object::List { items } => items.len(),
                    _ => unreachable!("verified list shape"),
                };
                self.push(Value::Int(len as i64))?;
            }
            Instr::ListAt => {
                let idx = self.pop_int();
                let r = self.pop_obj();
                let value = match self.heap.get(r) {
                    Object::List { items } => {
                        if idx < 0 || idx as usize >= items.len() {
                            return Err(FaultCode::IndexOutOfBounds);
                        }
                        items[idx as usize]
                    }
                    _ => unreachable!("verified list shape"),
                };
                self.push(value)?;
            }
            Instr::ListPush => {
                let value = self.pop();
                let r = self.pop_obj();
                self.frozen_guard(r)?;
                self.reserve(16, &[Value::Obj(r), value])?;
                match self.heap.get_mut(r) {
                    Object::List { items } => items.push(value),
                    _ => unreachable!("verified list shape"),
                }
                self.heap.recharge(r);
                self.push(Value::Unit)?;
            }
            Instr::MapNew { count, .. } => {
                let split = self.operands.len() - 2 * count as usize;
                let flat: Vec<Value> = self.operands.split_off(split);
                let mut entries: Vec<(Value, Value)> = Vec::new();
                for pair in flat.chunks(2) {
                    let (key, value) = (pair[0], pair[1]);
                    match self.map_find(&entries, key) {
                        Some(pos) => entries[pos].1 = value,
                        None => entries.push((key, value)),
                    }
                }
                let value = self.alloc(Object::Map { entries })?;
                self.push(value)?;
            }
            Instr::MapLen => {
                let r = self.pop_obj();
                let len = match self.heap.get(r) {
                    Object::Map { entries } => entries.len(),
                    _ => unreachable!("verified map shape"),
                };
                self.push(Value::Int(len as i64))?;
            }
            Instr::MapHas => {
                let key = self.pop();
                let r = self.pop_obj();
                let found = match self.heap.get(r) {
                    Object::Map { entries } => self.map_find(entries, key).is_some(),
                    _ => unreachable!("verified map shape"),
                };
                self.push(Value::Bool(found))?;
            }
            Instr::MapAt => {
                let key = self.pop();
                let r = self.pop_obj();
                let value = match self.heap.get(r) {
                    Object::Map { entries } => match self.map_find(entries, key) {
                        Some(pos) => entries[pos].1,
                        None => return Err(FaultCode::MissingKey),
                    },
                    _ => unreachable!("verified map shape"),
                };
                self.push(value)?;
            }
            Instr::MapPut => {
                let value = self.pop();
                let key = self.pop();
                let r = self.pop_obj();
                self.frozen_guard(r)?;
                let pos = match self.heap.get(r) {
                    Object::Map { entries } => self.map_find(entries, key),
                    _ => unreachable!("verified map shape"),
                };
                match pos {
                    Some(pos) => match self.heap.get_mut(r) {
                        Object::Map { entries } => entries[pos].1 = value,
                        _ => unreachable!(),
                    },
                    None => {
                        self.reserve(32, &[Value::Obj(r), key, value])?;
                        match self.heap.get_mut(r) {
                            Object::Map { entries } => entries.push((key, value)),
                            _ => unreachable!(),
                        }
                        self.heap.recharge(r);
                    }
                }
                self.push(Value::Unit)?;
            }
            Instr::SbNew => {
                let value = self.alloc(Object::StrBuilder(String::new()))?;
                self.push(value)?;
            }
            Instr::SbAppendStr => {
                let s = self.pop_obj();
                let sb = self.pop_obj();
                self.frozen_guard(sb)?;
                let text = match self.heap.get(s) {
                    Object::Str(text) => text.clone(),
                    _ => unreachable!("verified string shape"),
                };
                self.sb_append(sb, &text)?;
            }
            Instr::SbAppendInt => {
                let v = self.pop_int();
                let sb = self.pop_obj();
                self.frozen_guard(sb)?;
                self.sb_append(sb, &v.to_string())?;
            }
            Instr::SbAppendBool => {
                let v = self.pop_bool();
                let sb = self.pop_obj();
                self.frozen_guard(sb)?;
                let text = if v { "true" } else { "false" };
                self.sb_append(sb, text)?;
            }
            Instr::SbBuild => {
                let sb = self.pop_obj();
                let text = match self.heap.get(sb) {
                    Object::StrBuilder(text) => text.clone(),
                    _ => unreachable!("verified builder shape"),
                };
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::BbNew => {
                let value = self.alloc(Object::ByteBuf(Vec::new()))?;
                self.push(value)?;
            }
            Instr::BbAppend => {
                let v = self.pop_int();
                let bb = self.pop_obj();
                self.frozen_guard(bb)?;
                let byte = u8::try_from(v).map_err(|_| FaultCode::IntegerOverflow)?;
                self.reserve(1, &[Value::Obj(bb)])?;
                match self.heap.get_mut(bb) {
                    Object::ByteBuf(bytes) => bytes.push(byte),
                    _ => unreachable!("verified buffer shape"),
                }
                self.heap.recharge(bb);
                self.push(Value::Obj(bb))?;
            }
            Instr::BbLen => {
                let bb = self.pop_obj();
                let len = match self.heap.get(bb) {
                    Object::ByteBuf(bytes) => bytes.len(),
                    _ => unreachable!("verified buffer shape"),
                };
                self.push(Value::Int(len as i64))?;
            }
            Instr::BbBuild => {
                let bb = self.pop_obj();
                let bytes = match self.heap.get(bb) {
                    Object::ByteBuf(bytes) => bytes.clone(),
                    _ => unreachable!("verified buffer shape"),
                };
                let text = String::from_utf8(bytes).map_err(|_| FaultCode::BadCast)?;
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::Freeze => {
                let r = self.pop_obj();
                self.heap.freeze(r);
                self.push(Value::Obj(r))?;
            }
            Instr::Jump(target) => {
                let frame = self.frames.last_mut().expect("frame");
                frame.block = target;
                frame.ip = 0;
            }
            Instr::JumpIfFalse(target) => {
                if !self.pop_bool() {
                    let frame = self.frames.last_mut().expect("frame");
                    frame.block = target;
                    frame.ip = 0;
                }
            }
            Instr::JumpIfTrue(target) => {
                if self.pop_bool() {
                    let frame = self.frames.last_mut().expect("frame");
                    frame.block = target;
                    frame.ip = 0;
                }
            }
            Instr::Return => {
                let value = self.pop();
                let frame = self.frames.pop().expect("frame");
                self.operands.truncate(frame.base_operand as usize);
                self.locals.truncate(frame.base_local as usize);
                if self.frames.is_empty() {
                    return Ok(ExecOutcome::Terminal(value));
                }
                self.push(value)?;
            }
            Instr::Unreachable => {
                return Err(FaultCode::UnreachableCode);
            }
            Instr::Perform { op, argc } => {
                let split = self.operands.len() - argc as usize;
                let args = self.operands.split_off(split);
                return Ok(ExecOutcome::Perform { op, args });
            }
            Instr::PerformValue { argc } => {
                let split = self.operands.len() - argc as usize;
                let args = self.operands.split_off(split);
                let callee = self.pop();
                let op = match callee {
                    Value::Op(op) => op,
                    _ => unreachable!("verified operation value"),
                };
                return Ok(ExecOutcome::Perform { op, args });
            }
            Instr::OpConst(op) => {
                self.push(Value::Op(op))?;
            }
            Instr::TableEdit { action, kind, slot } => {
                let mock = if action == 2 { Some(self.pop()) } else { None };
                let table = self.pop_obj();
                return Ok(ExecOutcome::TableEdit {
                    table,
                    action,
                    kind,
                    slot,
                    mock,
                });
            }
            Instr::AsCall(op) => {
                let request = self.pop_obj();
                return Ok(ExecOutcome::AsCall { request, op });
            }
            Instr::CallArgs => {
                let call = self.pop_obj();
                return Ok(ExecOutcome::CallArgs { call });
            }
            Instr::FaultCode => {
                let r = self.pop_obj();
                let code = match self.heap.get(r) {
                    Object::NativeFault { code, .. } => *code,
                    _ => unreachable!("verified fault shape"),
                };
                let value = self.alloc(Object::Str(code.to_string()))?;
                self.push(value)?;
            }
        }
        Ok(ExecOutcome::Continue)
    }

    /// Return true when the instance class equals or extends the
    /// class named by the target type index.
    fn instance_matches(&self, module: &Module, r: ObjRef, ty: u32) -> bool {
        let target = match &module.types[ty as usize] {
            lm_bytecode::BcType::Class(c) | lm_bytecode::BcType::Inst(c, _) => *c,
            _ => unreachable!("verified type-test target"),
        };
        let mut class = match self.heap.get(r) {
            Object::Instance { class, .. } => *class,
            _ => unreachable!("verified type-test operand"),
        };
        loop {
            if class == target {
                return true;
            }
            match module.classes[class as usize].parent() {
                Some(p) => class = p,
                None => return false,
            }
        }
    }

    /// Append text to a string builder with a growth reservation.
    fn sb_append(&mut self, sb: ObjRef, text: &str) -> Result<(), FaultCode> {
        self.reserve(text.len(), &[Value::Obj(sb)])?;
        match self.heap.get_mut(sb) {
            Object::StrBuilder(buf) => buf.push_str(text),
            _ => unreachable!("verified builder shape"),
        }
        self.heap.recharge(sb);
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
    ) -> Result<(), FaultCode> {
        if self.frames.len() as u32 >= self.config.max_frames {
            return Err(FaultCode::StackLimit);
        }
        let func = &module.funcs[callee as usize];
        let base_local = self.locals.len() as u32;
        let arg_start = self.operands.len() - consume;
        let new_locals = self.locals.len() + func.local_count() as usize;
        if new_locals + self.operands.len() > self.config.max_stack_values as usize {
            return Err(FaultCode::StackLimit);
        }
        self.locals.extend_from_slice(&self.operands[arg_start..]);
        self.operands.truncate(arg_start);
        // The slots after the parameters start without a value.
        self.locals.resize(new_locals, Value::Unit);
        let base_operand = self.operands.len() as u32;
        self.frames.push(Frame {
            func: callee,
            block: 0,
            ip: 0,
            base_local,
            base_operand,
            closure,
        });
        Ok(())
    }

    pub fn push(&mut self, value: Value) -> Result<(), FaultCode> {
        if self.operands.len() + self.locals.len() >= self.config.max_stack_values as usize {
            return Err(FaultCode::StackLimit);
        }
        self.operands.push(value);
        Ok(())
    }

    fn pop(&mut self) -> Value {
        self.operands.pop().expect("verified stack shape")
    }

    fn pop_int(&mut self) -> i64 {
        match self.pop() {
            Value::Int(v) => v,
            _ => unreachable!("verified operand type"),
        }
    }

    fn pop_bool(&mut self) -> bool {
        match self.pop() {
            Value::Bool(v) => v,
            _ => unreachable!("verified operand type"),
        }
    }

    fn pop_obj(&mut self) -> ObjRef {
        match self.pop() {
            Value::Obj(r) => r,
            _ => unreachable!("verified operand type"),
        }
    }

    fn int_binary(&mut self, op: impl Fn(i64, i64) -> Option<i64>) -> Result<(), FaultCode> {
        let b = self.pop_int();
        let a = self.pop_int();
        let value = op(a, b).ok_or(FaultCode::IntegerOverflow)?;
        self.push(Value::Int(value))
    }

    fn int_compare(&mut self, op: impl Fn(i64, i64) -> bool) -> Result<(), FaultCode> {
        let b = self.pop_int();
        let a = self.pop_int();
        self.push(Value::Bool(op(a, b)))
    }

    fn str_compare(&mut self, want_equal: bool) -> Result<(), FaultCode> {
        let b = self.pop_obj();
        let a = self.pop_obj();
        let equal = match (self.heap.get(a), self.heap.get(b)) {
            (Object::Str(s1), Object::Str(s2)) => s1 == s2,
            _ => unreachable!("verified operand type"),
        };
        self.push(Value::Bool(equal == want_equal))
    }
}
