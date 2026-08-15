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

/// The sealed dispatch row of one class: a dense table over the
/// selector range the class answers.
///
/// A virtual call stays an indexed load chain: subtract the base and
/// index the table. Positions inside the range without a method hold
/// the sentinel; verified code never selects them, because the
/// verifier proves every virtual call resolves on the receiver class.
#[derive(Debug, Clone, Default)]
pub struct DispatchRow {
    /// The first selector slot the class answers.
    base: u32,
    /// Function indices for the selectors `base..base + len`.
    table: Vec<u32>,
}

impl DispatchRow {
    /// The method for one selector. Verified calls always resolve.
    ///
    /// The index is unchecked on purpose: this is the hot dispatch
    /// path, and the verifier proves every virtual call resolves on
    /// the receiver class. The debug assertion turns a future verifier
    /// gap into a test failure instead of a release panic.
    #[inline]
    pub(crate) fn method(&self, selector: u32) -> u32 {
        debug_assert!(
            selector >= self.base && ((selector - self.base) as usize) < self.table.len(),
            "the verifier admitted a virtual call the dispatch row cannot answer"
        );
        debug_assert_ne!(
            self.table[(selector - self.base) as usize],
            NO_METHOD,
            "the verifier admitted a virtual call on an empty dispatch slot"
        );
        self.table[(selector - self.base) as usize]
    }
}

/// A module that passed the independent verifier, plus the resolved
/// dispatch tables.
///
/// Construction through `load` is the only path, so every executed
/// function has passed verification. The dispatch table maps
/// `(class slot, selector slot)` to a function index with indexed
/// loads and no name lookup.
#[derive(Debug)]
pub struct LoadedModule {
    module: Module,
    dispatch: Vec<DispatchRow>,
    facts: VerifiedFacts,
}

impl LoadedModule {
    pub fn module(&self) -> &Module {
        &self.module
    }

    pub(crate) fn dispatch(&self) -> &[DispatchRow] {
        &self.dispatch
    }

    /// The definition hash of one class.
    pub fn class_hash(&self, class: u32) -> [u8; 32] {
        self.facts.class_hashes[class as usize]
    }

    /// The definition hash of one function.
    pub fn func_hash(&self, func: u32) -> [u8; 32] {
        self.facts.func_hashes[func as usize]
    }

    /// The hash-resolved core layout, shared by the verifier and the
    /// VM.
    pub fn core_layout(&self) -> lm_bytecode::corepin::CoreLayout {
        self.facts.core
    }

    /// The total dispatch-table cell count, for the memory gates.
    pub fn dispatch_cells(&self) -> usize {
        self.dispatch.iter().map(|row| row.table.len()).sum()
    }
}

/// The facts a cache hit replays: the definition hashes and the
/// hash-linked core layout.
///
/// The module semantic hash is deliberately absent. It covers the
/// export table, which holds the definition names, and the cache key
/// does not cover the function names. Call
/// `lm_bytecode::identity::module_identity` when the semantic hash is
/// needed.
#[derive(Debug, Clone)]
struct VerifiedFacts {
    class_hashes: Vec<[u8; 32]>,
    func_hashes: Vec<[u8; 32]>,
    core: lm_bytecode::corepin::CoreLayout,
}

/// The in-process verified-code cache.
///
/// A key is the module verification hash, the compiler ABI version,
/// and the verifier version. The verification hash covers every
/// verifier input, with each module-global index preserved, so a hit
/// skips every verifier pass. The loader always computes the key from
/// the decoded content, so a stored or forged hash never enters it.
///
/// An entry also carries the module identity and the core layout. Both
/// are pure functions of the key inputs: the verification hash covers
/// the whole semantic region and the class names, and identity reads
/// nothing else. Identity costs about as much as verification, so the
/// stored facts roughly double the value of a hit.
#[derive(Debug, Default)]
pub struct VerifiedCache {
    verified: std::collections::HashMap<([u8; 32], u32, u32), VerifiedFacts>,
    /// The number of full verifier runs, for the cache-skip tests.
    pub verifications: u64,
    /// The number of identity computations, for the cache-skip tests.
    pub identities: u64,
}

impl VerifiedCache {
    pub fn new() -> VerifiedCache {
        VerifiedCache::default()
    }
}

/// Verify a decoded module and admit it for execution.
pub fn load(module: Module) -> Result<LoadedModule, VerifyError> {
    load_inner(module, None)
}

/// Admit a decoded module through the verified-code cache. A second
/// load of the same semantic module under the same ABI and verifier
/// version skips re-verification.
pub fn load_cached(module: Module, cache: &mut VerifiedCache) -> Result<LoadedModule, VerifyError> {
    load_inner(module, Some(cache))
}

fn load_inner(
    module: Module,
    cache: Option<&mut VerifiedCache>,
) -> Result<LoadedModule, VerifyError> {
    // The identity is computed from the decoded content, never read
    // from the input. An unhashable module is a rejection.
    let compute = |module: &Module| -> Result<VerifiedFacts, VerifyError> {
        let identity = lm_bytecode::identity::module_identity(module).map_err(|e| VerifyError {
            func: u32::MAX,
            message: e.to_string(),
        })?;
        let core = lm_bytecode::corepin::core_layout(module, &identity);
        Ok(VerifiedFacts {
            class_hashes: identity.class_hashes,
            func_hashes: identity.func_hashes,
            core,
        })
    };
    let facts = match cache {
        Some(cache) => {
            // The key is the verification hash, never the semantic
            // hash. The semantic hash answers "same program meaning?"
            // and replaces every module-global index with content, so
            // two modules that differ only in an index share it. The
            // verifier reads those indices, so such a key can certify
            // a module the verifier rejects. The verification hash
            // keeps every index, covers the operation manifest, and
            // covers the class names.
            //
            // The key therefore fixes every verifier input, so a hit
            // skips every pass, not only the function dataflow. The
            // identity, the core layout, and the dispatch rows below
            // are pure functions of the same inputs, so a hit replays
            // them instead of recomputing them.
            let key = (
                lm_bytecode::identity::verification_hash(&module),
                lm_bytecode::identity::COMPILER_ABI_VERSION,
                lm_verify::VERIFIER_VERSION,
            );
            match cache.verified.get(&key) {
                Some(facts) => facts.clone(),
                None => {
                    let facts = compute(&module)?;
                    cache.identities += 1;
                    lm_verify::verify_with_layout(&module, facts.core)?;
                    cache.verifications += 1;
                    cache.verified.insert(key, facts.clone());
                    facts
                }
            }
        }
        None => {
            let facts = compute(&module)?;
            lm_verify::verify_with_layout(&module, facts.core)?;
            facts
        }
    };
    // Build the sealed per-class selector tables. A child inherits
    // the resolved parent methods; own methods override entries.
    // Parents precede children in the verified class table. Each row
    // spans only the selector range its class answers, so the table
    // memory follows the methods, not classes times selectors.
    let mut resolved: Vec<Vec<(u32, u32)>> = Vec::with_capacity(module.classes.len());
    let mut dispatch: Vec<DispatchRow> = Vec::with_capacity(module.classes.len());
    for class in &module.classes {
        let mut methods: Vec<(u32, u32)> = match class.parent() {
            Some(p) => resolved[p as usize].clone(),
            None => Vec::new(),
        };
        for (sel, func) in &class.methods {
            match methods.iter_mut().find(|(s, _)| s == sel) {
                Some(entry) => entry.1 = *func,
                None => methods.push((*sel, *func)),
            }
        }
        let row = match methods.iter().map(|(s, _)| *s).min() {
            Some(base) => {
                let top = methods.iter().map(|(s, _)| *s).max().expect("non-empty");
                let mut table = vec![NO_METHOD; (top - base + 1) as usize];
                for (sel, func) in &methods {
                    table[(*sel - base) as usize] = *func;
                }
                DispatchRow { base, table }
            }
            None => DispatchRow::default(),
        };
        resolved.push(methods);
        dispatch.push(row);
    }
    Ok(LoadedModule {
        module,
        dispatch,
        facts,
    })
}

/// Decode serialized bytecode, verify it, and admit it for execution.
pub fn load_bytes(bytes: &[u8]) -> Result<LoadedModule, LoadError> {
    let module = lm_bytecode::decode(bytes).map_err(LoadError::Decode)?;
    load(module).map_err(LoadError::Verify)
}

/// Decode serialized bytecode and admit it through the verified-code
/// cache.
pub fn load_bytes_cached(
    bytes: &[u8],
    cache: &mut VerifiedCache,
) -> Result<LoadedModule, LoadError> {
    let module = lm_bytecode::decode(bytes).map_err(LoadError::Decode)?;
    load_cached(module, cache).map_err(LoadError::Verify)
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
                param_muts: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_types: vec![2],
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
                param_muts: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_types: vec![],
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
                param_muts: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_types: vec![],
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
