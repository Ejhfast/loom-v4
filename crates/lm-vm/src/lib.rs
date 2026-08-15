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
pub use lm_verify::VerifyError;
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
    core: lm_bytecode::corepin::CoreLayout,
}

impl LoadedModule {
    pub fn module(&self) -> &Module {
        &self.module
    }

    pub(crate) fn dispatch(&self) -> &[DispatchRow] {
        &self.dispatch
    }

    /// The core layout the artifact declares and the verifier proved.
    pub fn core_layout(&self) -> lm_bytecode::corepin::CoreLayout {
        self.core
    }

    /// The total dispatch-table cell count, for the memory gates.
    pub fn dispatch_cells(&self) -> usize {
        self.dispatch.iter().map(|row| row.table.len()).sum()
    }
}

/// The key of one verified-code cache entry: the module verification
/// hash, the compiler ABI version, and the verifier version.
pub type VerifiedKey = ([u8; 32], u32, u32);

/// The verified-code cache key of one decoded module.
///
/// The value comes from the decoded content alone. No hash stored in
/// an artifact enters it, and the container stores no hash at all.
pub fn verified_key(module: &Module) -> VerifiedKey {
    (
        lm_bytecode::identity::verification_hash(module),
        lm_bytecode::identity::COMPILER_ABI_VERSION,
        lm_verify::VERIFIER_VERSION,
    )
}

/// One admission verdict.
///
/// The verdict carries no fact. The artifact declares its own core
/// role table, and the verifier proves the shape of every filled slot,
/// so a load reads the layout from the bytes and needs no side table.
/// A store therefore records "the verifier admitted this key" and
/// nothing else, and a damaged store entry can never supply a wrong
/// resolved value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VerifiedRecord;

/// The in-process verified-code cache.
///
/// A key is the module verification hash, the compiler ABI version,
/// and the verifier version. The verification hash covers every
/// verifier input, with each module-global index preserved, so a hit
/// skips every verifier pass. The loader always computes the key from
/// the decoded content, so a stored or forged hash never enters it.
#[derive(Debug, Default)]
pub struct VerifiedCache {
    verified: std::collections::HashSet<VerifiedKey>,
    /// The number of full verifier runs, for the cache-skip tests.
    pub verifications: u64,
}

impl VerifiedCache {
    pub fn new() -> VerifiedCache {
        VerifiedCache::default()
    }

    /// True when the cache holds an admission under this key.
    pub fn holds(&self, key: &VerifiedKey) -> bool {
        self.verified.contains(key)
    }

    /// Record one admission. A caller that reads a verdict from a
    /// store must admit the module through `load_with_record`, which
    /// proves the verdict belongs to the module.
    pub fn insert(&mut self, key: VerifiedKey, _record: VerifiedRecord) {
        self.verified.insert(key);
    }

    /// The number of stored admissions.
    pub fn len(&self) -> usize {
        self.verified.len()
    }

    pub fn is_empty(&self) -> bool {
        self.verified.is_empty()
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
    // Only a linked module executes. An import slot names a definition
    // another module provides, so an unfulfilled slot has no body to
    // run. The linker resolves every slot and produces a module with
    // an empty import table.
    if !module.imports.is_empty() {
        return Err(VerifyError {
            func: u32::MAX,
            message: format!(
                "the module has {} unresolved import slot(s); link it before it runs",
                module.imports.len()
            ),
        });
    }
    match cache {
        Some(cache) => {
            // The key is the verification hash, never the semantic
            // hash. The semantic hash answers "same program meaning?"
            // and replaces every module-global index with content, so
            // two modules that differ only in an index share it. The
            // verifier reads those indices, so such a key can certify
            // a module the verifier rejects. The verification hash
            // keeps every index and covers the operation manifest.
            //
            // The key therefore fixes every verifier input, so a hit
            // skips every pass, not only the function dataflow.
            let key = verified_key(&module);
            if !cache.verified.contains(&key) {
                lm_verify::verify_module(&module)?;
                cache.verifications += 1;
                cache.verified.insert(key);
            }
        }
        None => lm_verify::verify_module(&module)?,
    }
    Ok(admit(module))
}

/// Admit a decoded module through a verdict an external store kept.
///
/// The verdict replaces the verifier pass, so the caller must prove it
/// belongs to this module. The proof is the key: this function
/// recomputes it from the decoded content and rejects a verdict filed
/// under any other key. A store that returns a wrong or damaged
/// verdict therefore cannot admit a module.
///
/// The verdict carries no resolved value, so a store cannot supply
/// one. The core layout comes from the artifact, and the verifier
/// proved it before the verdict existed.
pub fn load_with_record(
    module: Module,
    key: &VerifiedKey,
    _record: &VerifiedRecord,
) -> Result<LoadedModule, VerifyError> {
    let reject = |message: &str| VerifyError {
        func: u32::MAX,
        message: message.to_string(),
    };
    if !module.imports.is_empty() {
        return Err(reject("the module has unresolved import slots"));
    }
    if verified_key(&module) != *key {
        return Err(reject("the stored verdict does not belong to this module"));
    }
    Ok(admit(module))
}

/// Build the sealed dispatch tables of an admitted module.
fn admit(module: Module) -> LoadedModule {
    let core = lm_bytecode::corepin::declared_layout(&module);
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
    LoadedModule {
        module,
        dispatch,
        core,
    }
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
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
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
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
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
                key: "Point".to_string(),
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
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
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
