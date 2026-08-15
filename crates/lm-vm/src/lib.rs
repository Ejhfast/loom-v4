//! The bytecode virtual machine.
//!
//! The VM owns explicit frames and one operand arena. A guest call
//! pushes a VM frame and never grows the Rust stack. One interpreter
//! loop executes verified code with an instruction fuel budget and a
//! hard heap cap. The result is a terminal `Done` value or a `Fault`.

mod heap;

pub use heap::Heap;

use lm_bytecode::{DecodeError, Instr, Module};
use lm_value::{StrRef, Value};
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
}

impl fmt::Display for FaultCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            FaultCode::IntegerOverflow => "IntegerOverflow",
            FaultCode::DivideByZero => "DivideByZero",
            FaultCode::OutOfFuel => "OutOfFuel",
            FaultCode::StackLimit => "StackLimit",
            FaultCode::HeapLimit => "HeapLimit",
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
    /// The hard heap cap in bytes.
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

/// A module that passed the independent verifier.
///
/// Construction through `load` is the only path, so every executed
/// function has passed verification.
#[derive(Debug)]
pub struct LoadedModule {
    module: Module,
}

impl LoadedModule {
    pub fn module(&self) -> &Module {
        &self.module
    }
}

/// Verify a decoded module and admit it for execution.
pub fn load(module: Module) -> Result<LoadedModule, VerifyError> {
    lm_verify::verify_module(&module)?;
    Ok(LoadedModule { module })
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
}

/// The virtual machine for one loaded module.
pub struct Vm<'m> {
    module: &'m Module,
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
            heap: Heap::new(config.heap_bytes),
            config,
            frames: Vec::new(),
            locals: Vec::new(),
            operands: Vec::new(),
        }
    }

    /// Run the entry function to a terminal result.
    pub fn run(&mut self) -> Outcome {
        match self.run_inner() {
            Ok(value) => Outcome::Done(value),
            Err(fault) => Outcome::Fault(fault),
        }
    }

    fn run_inner(&mut self) -> Result<Value, FaultCode> {
        let mut fuel = self.config.fuel;
        self.push_frame(self.module.entry)?;
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
                    let text = &self.module.strings[idx as usize];
                    let sref = self.heap.alloc_str(text).ok_or(FaultCode::HeapLimit)?;
                    self.push(Value::Str(sref))?;
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
                Instr::Call(callee) => self.push_frame(callee)?,
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

    /// Push a frame for a direct call. Arguments are on the operand
    /// stack in declaration order.
    fn push_frame(&mut self, callee: u32) -> Result<(), FaultCode> {
        if self.frames.len() as u32 >= self.config.max_frames {
            return Err(FaultCode::StackLimit);
        }
        let func = &self.module.funcs[callee as usize];
        let argc = func.params.len();
        let base_local = self.locals.len() as u32;
        let arg_start = self.operands.len() - argc;
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

    fn pop_str(&mut self) -> StrRef {
        match self.pop() {
            Value::Str(v) => v,
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
        let b = self.pop_str();
        let a = self.pop_str();
        let equal = self.heap.get_str(a) == self.heap.get_str(b);
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

    fn show_value(&self, value: Value) -> String {
        match value {
            Value::Unit => "()".to_string(),
            Value::Bool(v) => v.to_string(),
            Value::Int(v) => v.to_string(),
            Value::Str(sref) => render_string(self.heap.get_str(sref)),
            Value::Code(slot) => format!("<code fn{}>", slot.0),
        }
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
    use lm_bytecode::{Func, Instr::*, PrimTy};

    fn int_module(blocks: Vec<Vec<Instr>>) -> LoadedModule {
        load(Module {
            strings: vec![],
            funcs: vec![Func {
                name: "main".to_string(),
                params: vec![],
                ret: PrimTy::Int,
                local_count: 0,
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
            funcs: vec![Func {
                name: "main".to_string(),
                params: vec![],
                ret: PrimTy::Int,
                local_count: 0,
                blocks: vec![vec![Jump(9)]],
            }],
            entry: 0,
        };
        assert!(load(module).is_err());
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
