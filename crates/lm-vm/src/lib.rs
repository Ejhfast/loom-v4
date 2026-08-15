//! The bytecode virtual machine.
//!
//! The VM owns explicit frames and one operand arena. A guest call
//! pushes a VM frame and never grows the Rust stack. One interpreter
//! loop executes verified code with an instruction fuel budget and a
//! collected heap under a hard cap. The result is a terminal `Done`
//! value or a `Fault`.
//!
//! Allocation past the heap cap first runs a stop-the-VM mark/sweep
//! collection. The VM faults `HeapLimit` only when live data still
//! exceeds the cap after the collection.

mod heap;

pub use heap::{Heap, HeapStats, Object, ShapeDesc};

use lm_bytecode::{BcClassKind, BcType, DecodeError, Instr, Module};
use lm_value::{ObjRef, Value};
use lm_verify::VerifyError;
use std::fmt;

/// A stable machine-fault code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultCode {
    IntegerOverflow,
    DivideByZero,
    OutOfFuel,
    StackLimit,
    HeapLimit,
    FrozenWrite,
    IndexOutOfBounds,
    MissingKey,
    BadCast,
    /// Implementation subcode: a field was read before its first
    /// assignment. Checked source programs cannot reach this fault.
    UninitializedField,
}

impl fmt::Display for FaultCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            FaultCode::IntegerOverflow => "IntegerOverflow",
            FaultCode::DivideByZero => "DivideByZero",
            FaultCode::OutOfFuel => "OutOfFuel",
            FaultCode::StackLimit => "StackLimit",
            FaultCode::HeapLimit => "HeapLimit",
            FaultCode::FrozenWrite => "FrozenWrite",
            FaultCode::IndexOutOfBounds => "IndexOutOfBounds",
            FaultCode::MissingKey => "MissingKey",
            FaultCode::BadCast => "BadCast",
            FaultCode::UninitializedField => "UninitializedField",
        };
        f.write_str(name)
    }
}

/// A terminal execution result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done(Value),
    Fault(FaultCode),
}

/// Resource limits and the fuel budget for one run.
#[derive(Debug, Clone, Copy)]
pub struct VmConfig {
    /// Instruction budget. Each instruction costs one unit.
    pub fuel: u64,
    /// The largest frame-stack depth.
    pub max_frames: u32,
    /// The largest total operand-arena and local-arena size, in values.
    pub max_stack_values: u32,
    /// The hard heap cap in logical bytes.
    pub heap_bytes: usize,
}

impl Default for VmConfig {
    fn default() -> VmConfig {
        VmConfig {
            fuel: 1_000_000_000,
            max_frames: 65_536,
            max_stack_values: 4_194_304,
            heap_bytes: 64 << 20,
        }
    }
}

/// A load failure: a structural decode error or a verifier rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    Decode(DecodeError),
    Verify(VerifyError),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Decode(e) => write!(f, "decode error: {e}"),
            LoadError::Verify(e) => write!(f, "verify error: {e}"),
        }
    }
}

/// The sentinel for an empty dispatch-table entry.
const NO_METHOD: u32 = u32::MAX;

/// A module that passed the independent verifier, plus the resolved
/// dispatch tables.
///
/// Construction through `load` is the only path, so every executed
/// function has passed verification. The dispatch table maps
/// `(class slot, selector slot)` to a function index with two indexed
/// loads and no name lookup.
#[derive(Debug)]
pub struct LoadedModule {
    module: Module,
    dispatch: Vec<Vec<u32>>,
}

impl LoadedModule {
    pub fn module(&self) -> &Module {
        &self.module
    }
}

/// Verify a decoded module and admit it for execution.
pub fn load(module: Module) -> Result<LoadedModule, VerifyError> {
    lm_verify::verify_module(&module)?;
    // Build the sealed per-class selector tables. A child row starts
    // as a copy of the parent row; own methods override entries.
    // Parents precede children in the verified class table.
    let mut dispatch: Vec<Vec<u32>> = Vec::with_capacity(module.classes.len());
    for class in &module.classes {
        let mut row = match class.parent() {
            Some(p) => dispatch[p as usize].clone(),
            None => vec![NO_METHOD; module.selectors.len()],
        };
        for (sel, func) in &class.methods {
            row[*sel as usize] = *func;
        }
        dispatch.push(row);
    }
    Ok(LoadedModule { module, dispatch })
}

/// Decode serialized bytecode, verify it, and admit it for execution.
pub fn load_bytes(bytes: &[u8]) -> Result<LoadedModule, LoadError> {
    let module = lm_bytecode::decode(bytes).map_err(LoadError::Decode)?;
    load(module).map_err(LoadError::Verify)
}

/// One explicit VM frame.
struct Frame {
    func: u32,
    block: u32,
    ip: u32,
    base_local: u32,
    base_operand: u32,
    /// The active closure object for `LoadCapture`.
    closure: Option<ObjRef>,
}

/// The virtual machine for one loaded module.
pub struct Vm<'m> {
    module: &'m Module,
    dispatch: &'m [Vec<u32>],
    config: VmConfig,
    heap: Heap,
    frames: Vec<Frame>,
    locals: Vec<Value>,
    operands: Vec<Value>,
}

impl<'m> Vm<'m> {
    pub fn new(loaded: &'m LoadedModule, config: VmConfig) -> Vm<'m> {
        Vm {
            module: &loaded.module,
            dispatch: &loaded.dispatch,
            heap: Heap::new(config.heap_bytes),
            config,
            frames: Vec::new(),
            locals: Vec::new(),
            operands: Vec::new(),
        }
    }

    /// Read access to the heap, for inspection and tests.
    pub fn heap(&self) -> &Heap {
        &self.heap
    }

    /// Run the entry function to a terminal result.
    pub fn run(&mut self) -> Outcome {
        match self.run_inner() {
            Ok(value) => Outcome::Done(value),
            Err(fault) => Outcome::Fault(fault),
        }
    }

    /// Collect garbage now. `extra` holds additional roots that are
    /// not yet stored in the arenas.
    fn collect_garbage(&mut self, extra: &[ObjRef]) {
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
        roots.extend_from_slice(extra);
        self.heap.collect(roots);
    }

    /// Allocate one object. When the cap would be exceeded, collect
    /// first. The children of the new object are roots during the
    /// collection because they are not yet reachable from the arenas.
    fn alloc(&mut self, object: Object) -> Result<Value, FaultCode> {
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

    fn run_inner(&mut self) -> Result<Value, FaultCode> {
        let mut fuel = self.config.fuel;
        self.push_frame(self.module.entry, 0, None)?;
        loop {
            let frame = self.frames.last().expect("an active frame exists");
            let func = &self.module.funcs[frame.func as usize];
            let instr = func.blocks[frame.block as usize][frame.ip as usize];
            if fuel == 0 {
                return Err(FaultCode::OutOfFuel);
            }
            fuel -= 1;
            self.frames.last_mut().expect("an active frame exists").ip += 1;
            match instr {
                Instr::ConstUnit => self.push(Value::Unit)?,
                Instr::ConstBool(v) => self.push(Value::Bool(v))?,
                Instr::ConstInt(v) => self.push(Value::Int(v))?,
                Instr::ConstStr(idx) => {
                    let text = self.module.strings[idx as usize].clone();
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
                    let argc = self.module.funcs[callee as usize].params.len();
                    self.push_frame(callee, argc, None)?;
                }
                Instr::CallVirtual { selector, argc }
                | Instr::CallVirtualG { selector, argc, .. } => {
                    let argc = argc as usize;
                    let recv = self.operands[self.operands.len() - 1 - argc];
                    let class = match self.heap.get(recv.as_obj().expect("verified receiver")) {
                        Object::Instance { class, .. } => *class,
                        _ => unreachable!("verified receiver shape"),
                    };
                    let target = self.dispatch[class as usize][selector as usize];
                    debug_assert_ne!(target, NO_METHOD, "verified selector");
                    self.push_frame(target, argc + 1, None)?;
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
                    self.push_frame(target, argc, Some(r))?;
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
                    let field_count = self.module.classes[class as usize].fields.len();
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
                    let matches = self.instance_matches(r, ty);
                    self.push(Value::Bool(matches))?;
                }
                Instr::CastType(ty) => {
                    let r = self.pop_obj();
                    if !self.instance_matches(r, ty) {
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
                        return Ok(value);
                    }
                    self.push(value)?;
                }
            }
        }
    }

    /// Return true when the instance class equals or extends the
    /// class named by the target type index.
    fn instance_matches(&self, r: ObjRef, ty: u32) -> bool {
        let target = match &self.module.types[ty as usize] {
            BcType::Class(c) | BcType::Inst(c, _) => *c,
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
            match self.module.classes[class as usize].parent() {
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
        callee: u32,
        consume: usize,
        closure: Option<ObjRef>,
    ) -> Result<(), FaultCode> {
        if self.frames.len() as u32 >= self.config.max_frames {
            return Err(FaultCode::StackLimit);
        }
        let func = &self.module.funcs[callee as usize];
        let base_local = self.locals.len() as u32;
        let arg_start = self.operands.len() - consume;
        let new_locals = self.locals.len() + func.local_count as usize;
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

    fn push(&mut self, value: Value) -> Result<(), FaultCode> {
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

    /// Render a terminal outcome as stable text, for example
    /// `Done(3628800)` or `Fault(DivideByZero)`.
    pub fn show_outcome(&self, outcome: &Outcome) -> String {
        match outcome {
            Outcome::Done(value) => format!("Done({})", self.show_value(*value)),
            Outcome::Fault(code) => format!("Fault({code})"),
        }
    }

    /// Render one value in a stable readable form. Lists print as
    /// `[1, 2]`, maps in insertion order as `{"a": 1}`, and instances
    /// as `ClassName{field: value}`. Cycles print as `<cycle>`.
    pub fn show_value(&self, value: Value) -> String {
        let mut visited = Vec::new();
        self.show_value_inner(value, 0, &mut visited)
    }

    fn show_value_inner(&self, value: Value, depth: u32, visited: &mut Vec<ObjRef>) -> String {
        const MAX_SHOW_DEPTH: u32 = 32;
        match value {
            Value::Unit => "()".to_string(),
            Value::Bool(v) => v.to_string(),
            Value::Int(v) => v.to_string(),
            Value::Uninit => "<uninit>".to_string(),
            Value::Obj(r) => {
                if depth >= MAX_SHOW_DEPTH {
                    return "...".to_string();
                }
                if visited.contains(&r) {
                    return "<cycle>".to_string();
                }
                match self.heap.get(r) {
                    Object::Str(text) => render_string(text),
                    Object::List { items } => {
                        visited.push(r);
                        let parts: Vec<String> = items
                            .iter()
                            .map(|v| self.show_value_inner(*v, depth + 1, visited))
                            .collect();
                        visited.pop();
                        format!("[{}]", parts.join(", "))
                    }
                    Object::Map { entries } => {
                        visited.push(r);
                        let parts: Vec<String> = entries
                            .iter()
                            .map(|(k, v)| {
                                format!(
                                    "{}: {}",
                                    self.show_value_inner(*k, depth + 1, visited),
                                    self.show_value_inner(*v, depth + 1, visited)
                                )
                            })
                            .collect();
                        visited.pop();
                        format!("{{{}}}", parts.join(", "))
                    }
                    Object::Tuple { items } => {
                        visited.push(r);
                        let parts: Vec<String> = items
                            .iter()
                            .map(|v| self.show_value_inner(*v, depth + 1, visited))
                            .collect();
                        visited.pop();
                        if parts.len() == 1 {
                            format!("({},)", parts[0])
                        } else {
                            format!("({})", parts.join(", "))
                        }
                    }
                    Object::Instance { class, fields } => {
                        visited.push(r);
                        let bc = &self.module.classes[*class as usize];
                        let text = if bc.kind == BcClassKind::Case {
                            // A case instance prints in constructor
                            // form with its short arm name.
                            let short = bc.name.rsplit('.').next().unwrap_or(&bc.name);
                            if fields.is_empty() {
                                short.to_string()
                            } else {
                                let parts: Vec<String> = fields
                                    .iter()
                                    .map(|v| self.show_value_inner(*v, depth + 1, visited))
                                    .collect();
                                format!("{}({})", short, parts.join(", "))
                            }
                        } else {
                            let parts: Vec<String> = bc
                                .fields
                                .iter()
                                .zip(fields.iter())
                                .map(|((name, _), v)| {
                                    format!(
                                        "{}: {}",
                                        name,
                                        self.show_value_inner(*v, depth + 1, visited)
                                    )
                                })
                                .collect();
                            format!("{}{{{}}}", bc.name, parts.join(", "))
                        };
                        visited.pop();
                        text
                    }
                    Object::Closure { func, .. } => {
                        format!("<closure {}>", self.module.funcs[*func as usize].name)
                    }
                    Object::StrBuilder(buf) => format!("<StringBuilder len {}>", buf.len()),
                    Object::ByteBuf(bytes) => format!("<ByteBuffer len {}>", bytes.len()),
                }
            }
        }
    }

    /// Render the live machine state: outcome, heap statistics, frame
    /// count, and every live object in slot order. The format is
    /// deterministic.
    pub fn dump_live(&self, outcome: &Outcome) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "outcome: {}", self.show_outcome(outcome));
        let s = self.heap.stats();
        let _ = writeln!(
            out,
            "heap: live={} slots={} pages={} free={} used_bytes={} cap_bytes={} collections={}",
            s.live, s.slots, s.pages, s.free, s.used_bytes, s.cap_bytes, s.collections
        );
        let _ = writeln!(out, "frames: {} active", self.frames.len());
        for frame in &self.frames {
            let func = &self.module.funcs[frame.func as usize];
            let _ = writeln!(
                out,
                "  frame {} block {} ip {}",
                func.name, frame.block, frame.ip
            );
        }
        let _ = writeln!(out, "objects:");
        self.heap.for_each_live(|r, frozen, object| {
            let state = if frozen { "frozen" } else { "mutable" };
            let mut visited = Vec::new();
            let _ = writeln!(
                out,
                "  obj {} gen {} {} {} {}",
                r.slot,
                r.generation,
                object.shape().name,
                state,
                self.show_value_inner(Value::Obj(r), 0, &mut visited)
            );
        });
        out
    }
}

/// Render a string value with quotation marks and escapes.
fn render_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lm_bytecode::{BcType, Func, Instr::*, Module};

    fn int_module(blocks: Vec<Vec<Instr>>) -> LoadedModule {
        load(Module {
            strings: vec![],
            types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
            selectors: vec![],
            apps: vec![],
            classes: vec![],
            funcs: vec![Func {
                name: "main".to_string(),
                type_params: 0,
                effect_params: 0,
                params: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_count: 1,
                blocks,
            }],
            entry: 0,
        })
        .unwrap()
    }

    #[test]
    fn runs_addition() {
        let loaded = int_module(vec![vec![ConstInt(40), ConstInt(2), Add, Return]]);
        let mut vm = Vm::new(&loaded, VmConfig::default());
        assert_eq!(vm.run(), Outcome::Done(Value::Int(42)));
    }

    #[test]
    fn overflow_faults() {
        let loaded = int_module(vec![vec![ConstInt(i64::MAX), ConstInt(1), Add, Return]]);
        let mut vm = Vm::new(&loaded, VmConfig::default());
        assert_eq!(vm.run(), Outcome::Fault(FaultCode::IntegerOverflow));
    }

    #[test]
    fn divide_by_zero_faults() {
        let loaded = int_module(vec![vec![ConstInt(1), ConstInt(0), Div, Return]]);
        let mut vm = Vm::new(&loaded, VmConfig::default());
        assert_eq!(vm.run(), Outcome::Fault(FaultCode::DivideByZero));
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
            let mut vm = Vm::new(&loaded, VmConfig::default());
            assert_eq!(vm.run(), Outcome::Done(Value::Int(div)));
            let loaded = int_module(vec![vec![ConstInt(a), ConstInt(b), Rem, Return]]);
            let mut vm = Vm::new(&loaded, VmConfig::default());
            assert_eq!(vm.run(), Outcome::Done(Value::Int(rem)));
        }
    }

    #[test]
    fn fuel_exhaustion_faults() {
        let loaded = int_module(vec![vec![Jump(0)]]);
        let mut vm = Vm::new(
            &loaded,
            VmConfig {
                fuel: 1000,
                ..VmConfig::default()
            },
        );
        assert_eq!(vm.run(), Outcome::Fault(FaultCode::OutOfFuel));
    }

    #[test]
    fn load_rejects_invalid_module() {
        let module = Module {
            strings: vec![],
            types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
            selectors: vec![],
            apps: vec![],
            classes: vec![],
            funcs: vec![Func {
                name: "main".to_string(),
                type_params: 0,
                effect_params: 0,
                params: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_count: 0,
                blocks: vec![vec![Jump(9)]],
            }],
            entry: 0,
        };
        assert!(load(module).is_err());
    }

    #[test]
    fn uninitialized_field_read_faults() {
        // Hand-built bytecode reads a field before any store. The
        // checker prevents this in source programs; the VM faults.
        let module = Module {
            strings: vec![],
            types: vec![
                BcType::Unit,
                BcType::Bool,
                BcType::Int,
                BcType::Str,
                BcType::Class(0),
            ],
            selectors: vec![],
            apps: vec![],
            classes: vec![lm_bytecode::BcClass {
                name: "Point".to_string(),
                parent: lm_bytecode::NO_PARENT,
                type_params: 0,
                kind: BcClassKind::Normal,
                fields: vec![("x".to_string(), 2)],
                methods: vec![],
            }],
            funcs: vec![Func {
                name: "main".to_string(),
                type_params: 0,
                effect_params: 0,
                params: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_count: 0,
                blocks: vec![vec![New(0), LoadField(0), Return]],
            }],
            entry: 0,
        };
        let loaded = load(module).unwrap();
        let mut vm = Vm::new(&loaded, VmConfig::default());
        assert_eq!(vm.run(), Outcome::Fault(FaultCode::UninitializedField));
    }

    #[test]
    fn shows_outcomes() {
        let loaded = int_module(vec![vec![ConstInt(3), Return]]);
        let mut vm = Vm::new(&loaded, VmConfig::default());
        let outcome = vm.run();
        assert_eq!(vm.show_outcome(&outcome), "Done(3)");
        assert_eq!(
            vm.show_outcome(&Outcome::Fault(FaultCode::OutOfFuel)),
            "Fault(OutOfFuel)"
        );
    }
}
