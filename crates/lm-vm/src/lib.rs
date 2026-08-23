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

mod host;
mod machine;
mod resource;
mod schedule;
pub mod snapshot;
mod typecheck;
mod world;

pub use host::{
    CoreCtor, Host, HostArg, HostCompileDefinition, HostCompileEnv, HostCompileModule,
    HostCompileOptions, HostCompileSlot, HostCompletion, HostIpAddress, HostOpenOptions,
    HostParseStatus, HostResource, HostSeekFrom, HostShutdown, HostSocketAddress, HostStart,
    HostSyntaxDiagnostic, HostTcpKind, HostTcpResource, HostValue, HostWaitCancel, NullHost,
    RecordingHost,
};
pub use machine::{
    Block, FaultRec, FunctionVersionId, MachineState, Mailbox, Ownership, VmId, VmState,
};
pub use resource::{ResourceKind, ResourceRecord, ResourceRegistry, ResourceState};
pub use schedule::{
    CompletionKey, ScheduleEvents, SliceExit, TaskKey, TaskStatus, WaitSetKey, WaitSourceKey,
    WakeKey,
};
pub use world::{MailboxMetrics, RootEvent, StopMode, TraceBlock, TraceEvent, World};

/// The fault codes are manifest content, and the heap and the graph
/// engine name them too. They live in `lm-abi`.
pub use lm_abi::{FaultCode, SnapshotClass};
/// The heap, the native shapes, and the graph engine are separate
/// crates. `lm-vm` re-exports the parts its callers already name.
pub use lm_graph::{GraphCost, GraphLimits};
pub use lm_heap::{
    dump_shapes, BoundaryPolicy, Heap, HeapStats, Object, ShapeDesc, SharedBytes, SharedText,
};

use lm_bytecode::{DecodeError, Module};
use lm_value::Value;
pub use lm_verify::VerifyError;
use std::fmt;

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
    ///
    /// A program that serves forever is an ordinary program
    /// (specification 7.2 declares `serve(): Never`), so a root
    /// program takes no instruction cap it did not ask for. A caller
    /// that runs code it does not trust states a bound.
    ///
    /// The default is the largest value, not the absence of a bound.
    /// One machine retires about 445 million instructions each second
    /// on the reference implementation, so the default lasts about
    /// 1300 years of continuous execution. This is a lifetime budget
    /// counting down; `Machine::exec_for_quantum` is the separate
    /// per-call bound, and it is the mechanism a caller must use to
    /// limit work it does not trust.
    pub fuel: u64,
    /// The largest frame-stack depth.
    pub max_frames: u32,
    /// The largest total operand-arena and local-arena size, in values.
    pub max_stack_values: u32,
    /// The hard heap cap in logical bytes.
    pub heap_bytes: usize,
    /// The object, edge, byte, and work limits of every graph mode.
    pub graph: GraphLimits,
    /// The largest number of child machines this machine may create.
    /// A parent reserves a child from its own budget.
    pub max_children: u32,
    /// The largest number of live host resources this machine may
    /// register at one time.
    pub max_resources: u32,
    /// The largest number of accepted messages one proc mailbox may
    /// hold. A send past the bound blocks the sender.
    pub mailbox_limit: u32,
    /// The largest snapshot container this machine may write, in
    /// bytes. A capture past the bound returns
    /// `SnapshotLimitExceeded` (specification 17.4).
    pub snapshot_bytes: usize,
    /// The largest number of closed type nodes one world may hold.
    ///
    /// The language permits polymorphic recursion, so a program can
    /// ask for closed types without bound. A call past the cap takes
    /// `BoundaryLimit`. The world reads the value from the config of
    /// its root machine, because one world holds one table.
    pub max_closed_types: u32,
    /// The largest number of type environment nodes one world may
    /// hold. The cap works exactly as the one above.
    pub max_type_envs: u32,
}

impl Default for VmConfig {
    fn default() -> VmConfig {
        VmConfig {
            fuel: u64::MAX,
            max_frames: 65_536,
            max_stack_values: 4_194_304,
            heap_bytes: 64 << 20,
            graph: GraphLimits::default(),
            max_children: 1_024,
            max_resources: 1_024,
            mailbox_limit: 64,
            snapshot_bytes: 64 << 20,
            max_closed_types: lm_bytecode::closed::DEFAULT_MAX_CLOSED_TYPES,
            max_type_envs: lm_bytecode::closed::DEFAULT_MAX_TYPE_ENVS,
        }
    }
}

/// Aggregate resource limits for one machine world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldLimits {
    /// The largest machine record table.
    pub max_machines: u32,
    /// The largest live VM image record count.
    pub max_vm_images: u32,
    /// The largest logical byte cost of all live heaps.
    pub max_heap_bytes: usize,
    /// The largest live object count of all heaps.
    pub max_heap_objects: usize,
    /// The largest live host resource count.
    pub max_resources: usize,
    /// The instruction budget shared by all machines.
    ///
    /// The default is the largest value, for the reason
    /// `VmConfig::fuel` states. An embedder that bounds one world
    /// states a value.
    pub fuel: u64,
    /// The largest stored proc trace.
    pub max_trace_events: usize,
    /// The largest retained admitted-image cache, in bytes.
    ///
    /// This limit controls eviction. It never rejects an image.
    pub max_cached_image_bytes: usize,
}

impl Default for WorldLimits {
    fn default() -> WorldLimits {
        WorldLimits {
            max_machines: 4096,
            max_vm_images: 4096,
            max_heap_bytes: 1 << 30,
            max_heap_objects: 1 << 24,
            max_resources: 1 << 16,
            fuel: u64::MAX,
            max_trace_events: 1 << 20,
            max_cached_image_bytes: 256 << 20,
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
    /// The method for one selector, or `None` when the row does not
    /// answer it.
    ///
    /// The verifier proves that every virtual call of verified code
    /// resolves on the static receiver class. The runtime receiver is
    /// a heap object, and a restored machine states its own objects,
    /// so the class can be one the row does not answer. The lookup
    /// therefore tests the range and the empty slot, and the caller
    /// turns `None` into a machine fault.
    #[inline]
    pub(crate) fn method(&self, selector: u32) -> Option<u32> {
        let offset = selector.checked_sub(self.base)? as usize;
        match self.table.get(offset).copied() {
            Some(NO_METHOD) | None => None,
            Some(func) => Some(func),
        }
    }
}

/// A module that passed the independent verifier, plus the resolved
/// dispatch tables.
///
/// Construction through `load` is the only path, so every executed
/// function has passed verification. The dispatch table maps
/// `(class slot, selector slot)` to a function index with indexed
/// loads and no name lookup.
#[derive(Debug, Clone)]
pub struct LoadedModule {
    module: std::sync::Arc<Module>,
    bundle: std::sync::Arc<lm_abi::AbiBundle>,
    dispatch: std::sync::Arc<[DispatchRow]>,
    core: lm_bytecode::corepin::CoreLayout,
    /// The verifier input hash.
    ///
    /// The hash covers the operation manifest and every semantic
    /// table of the module, so one pass reads the whole program.
    /// Snapshot capture and restore both read the hash, and a search
    /// restores many worlds. Loading computes it once.
    verification: [u8; 32],
    /// The definition hash of every class and function.
    ///
    /// The canonical digest names code and classes by verified
    /// semantic identity, never by a numeric slot of one linked
    /// program. The identity pass is expensive, so it runs once, on
    /// the first digest of the process.
    identity:
        std::sync::Arc<std::sync::OnceLock<Result<lm_bytecode::identity::ModuleIdentity, String>>>,
    /// Canonical artifact bytes, created only when code reification needs them.
    artifact: std::sync::Arc<std::sync::OnceLock<SharedBytes>>,
}

impl LoadedModule {
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// Return the immutable ABI bundle used to verify this module.
    pub fn bundle(&self) -> &std::sync::Arc<lm_abi::AbiBundle> {
        &self.bundle
    }

    pub(crate) fn module_store(&self) -> std::sync::Arc<Module> {
        self.module.clone()
    }

    /// The hash of every verified input in this module.
    pub fn verification_hash(&self) -> [u8; 32] {
        self.verification
    }

    /// The verified semantic identity of this module.
    pub fn identity(&self) -> Result<&lm_bytecode::identity::ModuleIdentity, FaultCode> {
        self.identity
            .get_or_init(|| {
                lm_bytecode::identity::module_identity_with_bundle(&self.module, &self.bundle)
                    .map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|_| FaultCode::BoundaryViolation)
    }

    pub(crate) fn dispatch_store(&self) -> std::sync::Arc<[DispatchRow]> {
        self.dispatch.clone()
    }

    /// Return the canonical verified bytes that supplied this module.
    pub(crate) fn artifact_bytes(&self) -> SharedBytes {
        self.artifact
            .get_or_init(|| {
                SharedBytes::from(lm_bytecode::encode_with_bundle(&self.module, &self.bundle))
            })
            .clone()
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
    let bundle = lm_abi::standard_bundle();
    verified_key_with_bundle(module, &bundle)
}

/// Return the verified-code cache key under one ABI bundle.
pub fn verified_key_with_bundle(
    module: &Module,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
) -> VerifiedKey {
    (
        lm_bytecode::identity::verification_hash_with_bundle(bundle, module),
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
    let bundle = lm_abi::standard_bundle();
    load_with_bundle(module, &bundle)
}

/// Verify and load one decoded module under an ABI bundle.
pub fn load_with_bundle(
    module: Module,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
) -> Result<LoadedModule, VerifyError> {
    load_inner(module, bundle, None)
}

/// Admit a decoded module through the verified-code cache. A second
/// load of the same semantic module under the same ABI and verifier
/// version skips re-verification.
pub fn load_cached(module: Module, cache: &mut VerifiedCache) -> Result<LoadedModule, VerifyError> {
    let bundle = lm_abi::standard_bundle();
    load_cached_with_bundle(module, &bundle, cache)
}

/// Load one decoded module through a bundle-bound verified cache.
pub fn load_cached_with_bundle(
    module: Module,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
    cache: &mut VerifiedCache,
) -> Result<LoadedModule, VerifyError> {
    load_inner(module, bundle, Some(cache))
}

fn load_inner(
    module: Module,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
    cache: Option<&mut VerifiedCache>,
) -> Result<LoadedModule, VerifyError> {
    // Only a linked module executes. An import slot names a definition
    // another module provides, so an unfulfilled slot has no body to
    // run. The linker resolves every slot and produces a module with
    // an empty import table.
    if !module.imports.is_empty() {
        return Err(VerifyError {
            func: None,
            message: format!(
                "the module has {} unresolved import slot(s); link it before it runs",
                module.imports.len()
            ),
        });
    }
    let verification = match cache {
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
            let key = verified_key_with_bundle(&module, bundle);
            if !cache.verified.contains(&key) {
                lm_verify::verify_module_with_bundle(&module, bundle)?;
                cache.verifications = cache.verifications.saturating_add(1);
                cache.verified.insert(key);
            }
            key.0
        }
        None => {
            lm_verify::verify_module_with_bundle(&module, bundle)?;
            lm_bytecode::identity::verification_hash_with_bundle(bundle, &module)
        }
    };
    Ok(admit(module, bundle.clone(), verification))
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
    let bundle = lm_abi::standard_bundle();
    load_with_record_and_bundle(module, &bundle, key, _record)
}

/// Load one module through a stored verdict and an ABI bundle.
pub fn load_with_record_and_bundle(
    module: Module,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
    key: &VerifiedKey,
    _record: &VerifiedRecord,
) -> Result<LoadedModule, VerifyError> {
    let reject = |message: &str| VerifyError {
        func: None,
        message: message.to_string(),
    };
    if !module.imports.is_empty() {
        return Err(reject("the module has unresolved import slots"));
    }
    let found = verified_key_with_bundle(&module, bundle);
    if found != *key {
        return Err(reject("the stored verdict does not belong to this module"));
    }
    // The key proves the verdict belongs to this module. It does not
    // prove a verifier ever produced the verdict: a writer of the
    // store computes the key of any module and files a verdict under
    // it. An exact key stops a collision, never a forgery, so store
    // integrity carries the whole property.
    //
    // The module-level structural pass therefore runs on every hit.
    // It costs a small part of a verifier run, and it bounds the
    // damage of a store an attacker reaches: the table rules of these
    // exact bytes hold, whatever the verdict claims.
    lm_verify::verify_structure_only_with_bundle(&module, bundle)?;
    Ok(admit(module, bundle.clone(), found.0))
}

/// Build the sealed dispatch tables of an admitted module.
fn admit(
    module: Module,
    bundle: std::sync::Arc<lm_abi::AbiBundle>,
    verification: [u8; 32],
) -> LoadedModule {
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
        module: std::sync::Arc::new(module),
        bundle,
        dispatch: dispatch.into(),
        core,
        verification,
        identity: std::sync::Arc::new(std::sync::OnceLock::new()),
        artifact: std::sync::Arc::new(std::sync::OnceLock::new()),
    }
}

/// Decode serialized bytecode, verify it, and admit it for execution.
pub fn load_bytes(bytes: &[u8]) -> Result<LoadedModule, LoadError> {
    let bundle = lm_abi::standard_bundle();
    load_bytes_with_bundle(bytes, &bundle)
}

/// Decode and load artifact bytes under one ABI bundle.
pub fn load_bytes_with_bundle(
    bytes: &[u8],
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
) -> Result<LoadedModule, LoadError> {
    let module = lm_bytecode::decode_with_bundle(bytes, bundle).map_err(LoadError::Decode)?;
    load_with_bundle(module, bundle).map_err(LoadError::Verify)
}

/// Decode serialized bytecode and admit it through the verified-code
/// cache.
pub fn load_bytes_cached(
    bytes: &[u8],
    cache: &mut VerifiedCache,
) -> Result<LoadedModule, LoadError> {
    let bundle = lm_abi::standard_bundle();
    load_bytes_cached_with_bundle(bytes, &bundle, cache)
}

/// Decode artifact bytes through a bundle-bound verified cache.
pub fn load_bytes_cached_with_bundle(
    bytes: &[u8],
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
    cache: &mut VerifiedCache,
) -> Result<LoadedModule, LoadError> {
    let module = lm_bytecode::decode_with_bundle(bytes, bundle).map_err(LoadError::Decode)?;
    load_cached_with_bundle(module, bundle, cache).map_err(LoadError::Verify)
}

/// A single-machine view over one world with a null host and no
/// grants. Pure programs and the pre-effect test suites use it.
pub struct Vm {
    world: World,
}

impl Vm {
    pub fn new(loaded: &LoadedModule, config: VmConfig) -> Vm {
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
            bytes: vec![],
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
                local_types: vec![2],
                blocks,
            }],
            imports: vec![],
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: Vec::new(),
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
    fn a_world_owns_its_verified_code_store() {
        let mut world = {
            let loaded = int_module(vec![vec![ConstInt(40), ConstInt(2), Add, Return]]);
            World::new(&loaded, VmConfig::default(), Box::new(NullHost))
        };
        assert_eq!(world.run_root(), Outcome::Done(Value::Int(42)));
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
        let fault = vm.world.root_fault().expect("the root fault exists");
        assert_eq!(fault.trace.len(), 1);
        assert_eq!(fault.trace[0].function, 0);
        assert_eq!(fault.trace[0].block, 0);
        assert_eq!(fault.trace[0].instruction, 0);
    }

    #[test]
    fn load_rejects_invalid_module() {
        let module = Module {
            strings: vec![],
            bytes: vec![],
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
                blocks: vec![vec![Jump(9)]],
            }],
            imports: vec![],
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: Vec::new(),
        };
        assert!(load(module).is_err());
    }

    #[test]
    fn uninitialized_field_read_faults() {
        // Hand-built bytecode reads a field before any store. The
        // checker prevents this in source programs; the VM faults.
        let module = Module {
            strings: vec![],
            bytes: vec![],
            types: vec![
                BcType::Unit,
                BcType::Bool,
                BcType::Int,
                BcType::Str,
                BcType::Class(0),
            ],
            selectors: vec![],
            apps: vec![],
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![vec![]],
            func_bounds: vec![vec![]],
            classes: vec![lm_bytecode::BcClass {
                name: "Point".to_string(),
                key: "Point".to_string(),
                is_final: false,
                is_frozen: false,
                parent: lm_bytecode::NO_PARENT,
                parent_args: Vec::new(),
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
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: Vec::new(),
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
