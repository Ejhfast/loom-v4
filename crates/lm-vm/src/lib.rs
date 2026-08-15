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

mod heap;
mod host;
mod machine;
mod world;

pub use heap::{Heap, HeapStats, Object, ShapeDesc};
pub use host::{CoreCtor, Host, HostArg, HostStart, HostValue, NullHost, RecordingHost};
pub use machine::{FaultRec, MachineState, VmId};
pub use world::{RootEvent, StopMode, World};

use lm_bytecode::{DecodeError, Module};
use lm_value::Value;
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
    PolicyDenied,
    InvalidVmState,
    InvalidRequestToken,
    UnsendableValue,
    HostFault,
    /// Implementation subcode: a field was read before its first
    /// assignment. Checked source programs cannot reach this fault.
    UninitializedField,
    /// Implementation subcode: the runtime backstop behind a proven
    /// exhaustive `case` executed. Checked source programs cannot
    /// reach this fault.
    UnreachableCode,
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
            FaultCode::PolicyDenied => "PolicyDenied",
            FaultCode::InvalidVmState => "InvalidVmState",
            FaultCode::InvalidRequestToken => "InvalidRequestToken",
            FaultCode::UnsendableValue => "UnsendableValue",
            FaultCode::HostFault => "HostFault",
            FaultCode::UninitializedField => "UninitializedField",
            FaultCode::UnreachableCode => "UnreachableCode",
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

/// Resource limits and the fuel budget for one machine.
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

    pub(crate) fn dispatch(&self) -> &[Vec<u32>] {
        &self.dispatch
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

/// A single-machine view over one world with a null host and no
/// grants. Pure programs and the pre-effect test suites use it.
pub struct Vm<'m> {
    world: World<'m>,
}

impl<'m> Vm<'m> {
    pub fn new(loaded: &'m LoadedModule, config: VmConfig) -> Vm<'m> {
        Vm {
            world: World::new(loaded, config, Box::new(NullHost)),
        }
    }

    /// Read access to the root heap, for inspection and tests.
    pub fn heap(&self) -> &Heap {
        self.world.heap_of(0)
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

    fn int_module(blocks: Vec<Vec<lm_bytecode::Instr>>) -> LoadedModule {
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
        assert_eq!(vm.run(), Outcome::Fault(crate::FaultCode::IntegerOverflow));
    }

    #[test]
    fn divide_by_zero_faults() {
        let loaded = int_module(vec![vec![ConstInt(1), ConstInt(0), Div, Return]]);
        let mut vm = Vm::new(&loaded, VmConfig::default());
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
        assert_eq!(vm.run(), Outcome::Fault(crate::FaultCode::OutOfFuel));
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
        assert_eq!(
            vm.run(),
            Outcome::Fault(crate::FaultCode::UninitializedField)
        );
    }

    #[test]
    fn unreachable_instruction_faults() {
        let loaded = int_module(vec![vec![Unreachable]]);
        let mut vm = Vm::new(&loaded, VmConfig::default());
        assert_eq!(vm.run(), Outcome::Fault(crate::FaultCode::UnreachableCode));
    }

    #[test]
    fn shows_outcomes() {
        let loaded = int_module(vec![vec![ConstInt(3), Return]]);
        let mut vm = Vm::new(&loaded, VmConfig::default());
        let outcome = vm.run();
        assert_eq!(vm.show_outcome(&outcome), "Done(3)");
        assert_eq!(
            vm.show_outcome(&Outcome::Fault(crate::FaultCode::OutOfFuel)),
            "Fault(OutOfFuel)"
        );
    }
}
