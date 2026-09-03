//! Verified bytecode analysis and immutable native region plans.

use crate::{Failure, MAX_REGION_INSTRUCTIONS, MAX_REGION_LOCALS, MAX_REGION_STACK};
use lm_bytecode::{BcType, ExtendedInstr, Func, Instr, Module, NativeInstr, NumericInstr};
use lm_value::ValueTag;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// One scalar representation used by the native ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarKind {
    Unit,
    Bool,
    Int,
    Float,
    Char,
    /// One heap object with a source-unit type index.
    Object(u32),
    /// One canonical tagged value with a source-unit type index.
    Tagged(u32),
    /// One machine-local callback with a source-unit type index.
    Callback(u32),
    Operation,
}

#[derive(Clone, Copy)]
pub(super) struct FunctionDefinition<'a> {
    pub(super) function: u32,
    pub(super) runtime: &'a Func,
    pub(super) source: &'a Module,
    pub(super) bundle: &'a Arc<lm_abi::AbiBundle>,
    pub(super) source_function: u32,
    pub(super) class_relocation: Option<&'a [u32]>,
}

/// Immutable verified input for one native compilation.
pub struct FunctionInput<'a> {
    pub(super) root: FunctionDefinition<'a>,
    direct_callees: Vec<FunctionDefinition<'a>>,
    guarded_interface_callees: Vec<GuardedInterfaceCallee>,
    behaviors: crate::FunctionBehaviors,
    runtime_string_count: usize,
    runtime_byte_count: usize,
    runtime_core: lm_bytecode::corepin::CoreLayout,
}

impl<'a> FunctionInput<'a> {
    /// Create one function input from a published function and its source unit.
    #[inline]
    pub fn new(
        function: u32,
        runtime: &'a Func,
        source: &'a Module,
        bundle: &'a Arc<lm_abi::AbiBundle>,
        source_function: u32,
    ) -> FunctionInput<'a> {
        FunctionInput {
            root: FunctionDefinition {
                function,
                runtime,
                source,
                bundle,
                source_function,
                class_relocation: None,
            },
            direct_callees: Vec::new(),
            guarded_interface_callees: Vec::new(),
            behaviors: crate::FunctionBehaviors::default(),
            runtime_string_count: source.strings.len(),
            runtime_byte_count: source.bytes.len(),
            runtime_core: lm_bytecode::corepin::declared_layout(source),
        }
    }

    /// Supply the relocated string-table size for byte literal slots.
    pub fn set_runtime_string_count(&mut self, count: usize) {
        self.runtime_string_count = count;
    }

    /// Supply the relocated byte-table size for regular-expression slots.
    pub fn set_runtime_byte_count(&mut self, count: usize) {
        self.runtime_byte_count = count;
    }

    /// Supply the relocated core roles for runtime value dispatch.
    pub fn set_runtime_core_roles(&mut self, roles: &[u32; lm_bytecode::CORE_ROLE_COUNT]) {
        self.runtime_core = lm_bytecode::corepin::layout_from_roles(roles);
    }

    /// Supply the source-to-runtime class relocation for this unit.
    pub fn set_class_relocation(&mut self, classes: &'a [u32]) {
        self.root.class_relocation = Some(classes);
    }

    /// Supply transitive behavior facts for this namespace revision.
    pub fn set_function_behaviors(&mut self, behaviors: crate::FunctionBehaviors) {
        self.behaviors = behaviors;
    }

    /// Add one exact direct callee used by the root function.
    pub fn add_direct_callee(
        &mut self,
        function: u32,
        runtime: &'a Func,
        source: &'a Module,
        bundle: &'a Arc<lm_abi::AbiBundle>,
        source_function: u32,
    ) {
        if function == self.root.function
            || self
                .direct_callees
                .iter()
                .any(|definition| definition.function == function)
        {
            return;
        }
        self.direct_callees.push(FunctionDefinition {
            function,
            runtime,
            source,
            bundle,
            source_function,
            class_relocation: None,
        });
    }

    /// Supply one direct callee and its source-to-runtime class relocation.
    pub fn add_relocated_direct_callee(
        &mut self,
        function: u32,
        runtime: &'a Func,
        source: &'a Module,
        bundle: &'a Arc<lm_abi::AbiBundle>,
        source_function: u32,
        classes: &'a [u32],
    ) {
        self.add_direct_callee(function, runtime, source, bundle, source_function);
        if let Some(definition) = self
            .direct_callees
            .iter_mut()
            .find(|definition| definition.function == function)
        {
            definition.class_relocation = Some(classes);
        }
    }

    /// Add one guarded interface target and its class relocation.
    #[allow(clippy::too_many_arguments)]
    pub fn add_relocated_guarded_interface_callee(
        &mut self,
        interface: u32,
        method: u32,
        receiver: ScalarKind,
        function: u32,
        runtime: &'a Func,
        source: &'a Module,
        bundle: &'a Arc<lm_abi::AbiBundle>,
        source_function: u32,
        classes: &'a [u32],
    ) {
        self.add_relocated_direct_callee(
            function,
            runtime,
            source,
            bundle,
            source_function,
            classes,
        );
        let candidate = GuardedInterfaceCallee {
            interface,
            method,
            receiver,
            function,
        };
        if !self.guarded_interface_callees.contains(&candidate) {
            self.guarded_interface_callees.push(candidate);
        }
    }

    pub(super) fn runtime_string_count(&self) -> usize {
        self.runtime_string_count
    }

    pub(super) fn runtime_byte_count(&self) -> usize {
        self.runtime_byte_count
    }

    pub(super) fn definition(&self, function: u32) -> Option<FunctionDefinition<'a>> {
        std::iter::once(self.root)
            .chain(self.direct_callees.iter().copied())
            .find(|definition| definition.function == function)
    }

    fn behavior(&self, function: u32) -> crate::FunctionBehavior {
        self.behaviors.get(function)
    }

    pub(super) fn child(&self, function: u32) -> Option<FunctionInput<'a>> {
        let root = self.definition(function)?;
        Some(FunctionInput {
            root,
            direct_callees: Vec::new(),
            guarded_interface_callees: Vec::new(),
            behaviors: self.behaviors.clone(),
            runtime_string_count: self.runtime_string_count,
            runtime_byte_count: self.runtime_byte_count,
            runtime_core: self.runtime_core,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GuardedInterfaceCallee {
    interface: u32,
    method: u32,
    receiver: ScalarKind,
    function: u32,
}

/// Clock-free native compilation counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompilerMetrics {
    pub compilation_attempts: u64,
    pub compiled_regions: u64,
    pub compiled_code_bytes: u64,
    pub compiled_segments: u64,
    pub compiled_call_sites: u64,
    pub compiled_inlined_call_sites: u64,
    pub compiled_heap_read_sites: u64,
    pub compiled_heap_write_sites: u64,
    pub compiled_allocation_sites: u64,
    pub compiled_effect_sites: u64,
    pub compiled_interpreter_sites: u64,
}

/// One supported native entry and its required scalar values.
#[derive(Debug, Clone, Copy)]
pub struct EntryPlan<'a> {
    pub(super) index: u32,
    pub(super) live_locals: &'a [bool],
    pub(super) operand_kinds: &'a [ScalarKind],
}

impl EntryPlan<'_> {
    /// Return the native entry index.
    #[inline(always)]
    pub fn index(&self) -> u32 {
        self.index
    }

    /// Return the local slots that native code can read.
    #[inline(always)]
    pub fn live_locals(&self) -> &[bool] {
        self.live_locals
    }

    /// Return the required operand representations.
    #[inline(always)]
    pub fn operand_kinds(&self) -> &[ScalarKind] {
        self.operand_kinds
    }
}

/// One native exit category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    Fuel,
    Poll,
    Return,
    IntegerOverflow,
    DivideByZero,
    TypeMismatch,
    UninitializedField,
    Call,
    HeapLimit,
    Effect,
    StackLimit,
    StackRollover,
    InlineCall,
    Replay,
    Literal,
    Unreachable,
    GrowActivation,
    TypeResolution,
    TypeEnvironment,
    InterfaceCall,
    GenericVirtualCall,
    CallbackCall,
    GuestFault,
    GrowRoots,
    Boundary,
}

/// One validated native exit record.
#[derive(Debug, Clone, Copy)]
pub struct ExecutionExit {
    pub(super) retired: u64,
    pub(super) kind: ExitKind,
    pub(super) block: u32,
    pub(super) instruction: u32,
    pub(super) stack_len: u32,
    pub(super) result_tag: u64,
    pub(super) result: u64,
}

impl ExecutionExit {
    #[inline(always)]
    pub fn retired(&self) -> u64 {
        self.retired
    }

    #[inline(always)]
    pub fn kind(&self) -> ExitKind {
        self.kind
    }

    #[inline(always)]
    pub fn block(&self) -> u32 {
        self.block
    }

    #[inline(always)]
    pub fn instruction(&self) -> u32 {
        self.instruction
    }

    #[inline(always)]
    pub fn stack_len(&self) -> u32 {
        self.stack_len
    }

    /// Return the canonical result tag.
    #[inline(always)]
    pub fn result_tag(&self) -> u64 {
        self.result_tag
    }

    #[inline(always)]
    pub fn result(&self) -> u64 {
        self.result
    }

    /// Replace one transient object result before detached return handling.
    pub fn replace_object_result(&mut self, token: u64, reference: lm_value::ObjRef) {
        if self.result_tag != ValueTag::Obj as u64 || self.result != token {
            return;
        }
        self.result = u64::from(reference.slot) | (u64::from(reference.generation) << 32);
    }

    /// Add retired instructions from an earlier detached frame.
    pub fn add_prior_retired(mut self, retired: u64) -> Result<ExecutionExit, Failure> {
        self.retired = self
            .retired
            .checked_add(retired)
            .ok_or(Failure::BackendUnavailable)?;
        Ok(self)
    }

    /// Convert this exit into one retained entry position.
    pub fn resume_at(&mut self, block: u32, instruction: u32, stack_len: u32) {
        self.kind = ExitKind::Fuel;
        self.block = block;
        self.instruction = instruction;
        self.stack_len = stack_len;
        self.result_tag = 0;
        self.result = 0;
    }
}

/// One exact reason that prevents native compilation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedReason {
    MissingSource,
    NonScalarType,
    UnsupportedInstruction,
    InvalidStack,
    InvalidControlFlow,
    RegionLimit,
}

impl UnsupportedReason {
    /// Return the stable diagnostic label.
    pub fn label(self) -> &'static str {
        match self {
            UnsupportedReason::MissingSource => "missing source metadata",
            UnsupportedReason::NonScalarType => "missing native value representation",
            UnsupportedReason::UnsupportedInstruction => "missing opcode treatment",
            UnsupportedReason::InvalidStack => "invalid native stack analysis",
            UnsupportedReason::InvalidControlFlow => "invalid native control-flow analysis",
            UnsupportedReason::RegionLimit => "native region resource limit",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum SegmentExit {
    Continue {
        fallthrough_ip: u32,
    },
    Jump {
        target_block: u32,
    },
    Conditional {
        target_block: u32,
        jump_on_true: bool,
        fallthrough_ip: u32,
    },
    Call {
        target: u32,
        app: Option<u32>,
        fallthrough_ip: u32,
    },
    VirtualCall {
        selector: u32,
        fallthrough_ip: u32,
    },
    ValueCall {
        fallthrough_ip: u32,
    },
    GenericVirtualCall {
        selector: u32,
        application: u32,
        fallthrough_ip: u32,
    },
    InterfaceCall {
        interface: u32,
        method: u32,
        recv_ty: u32,
        app: u32,
        fallthrough_ip: u32,
    },
    SlotCall {
        slot: u32,
        application: Option<u32>,
        constructor: bool,
        fallthrough_ip: u32,
    },
    Allocation {
        fallthrough_ip: u32,
    },
    Effect {
        fallthrough_ip: u32,
    },
    Boundary {
        fallthrough_ip: Option<u32>,
    },
    Unreachable,
    Return,
}

#[derive(Debug, Clone)]
pub(super) struct Segment {
    pub(super) block: u32,
    pub(super) start: u32,
    pub(super) end: u32,
    pub(super) cost: u32,
    pub(super) fuel_reserve: u32,
    /// Instructions executed after the active reserve and before this segment.
    pub(super) reserved_prefix_cost: u32,
    /// Successor edges that retain the active reserve without one fuel write.
    pub(super) carry_reserved_cost: Vec<bool>,
    /// True when one native predecessor carries an uncharged prefix.
    pub(super) carries_reserved_prefix: bool,
    /// True when a runtime-filled cache can retry this segment.
    pub(super) retry_entry: bool,
    /// True when one cold replay edge can check all integer overflow flags.
    pub(super) defer_integer_overflow: bool,
    pub(super) exit: SegmentExit,
    pub(super) uses: Vec<bool>,
    pub(super) definitions: Vec<bool>,
    pub(super) successors: Vec<usize>,
    pub(super) live_in: Vec<bool>,
    /// Locals that one internal path can change before this segment exits.
    pub(super) dirty_locals: Vec<bool>,
    pub(super) entry_stack: Vec<ScalarKind>,
    /// Locals that can contain one pending native instance at entry.
    pub(super) virtual_locals_in: Vec<bool>,
    /// Operands that can contain one pending native instance at entry.
    pub(super) virtual_stack_in: Vec<bool>,
    /// Instructions that materialize pending instances before replay.
    pub(super) virtual_barriers: Vec<u32>,
    pub(super) call_contract: Option<CallContract>,
    pub(super) exit_stack: Vec<ScalarKind>,
    pub(super) boundary_stack: Vec<ScalarKind>,
    pub(super) heap_accesses: Vec<HeapAccess>,
    pub(super) option_accesses: Vec<OptionAccess>,
    pub(super) fuel_stacks: Vec<(u32, Vec<ScalarKind>)>,
    pub(super) replay_stacks: Vec<(u32, Vec<ScalarKind>)>,
    pub(super) fault_stacks: Vec<(u32, Vec<ScalarKind>)>,
    pub(super) allocations: Vec<AllocationSite>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ValueContract {
    pub(super) kind: ScalarKind,
    pub(super) object: Option<ObjectContract>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ObjectContract {
    Str,
    Text,
    Instance(u32),
    List,
    Map,
    Tuple,
    Closure,
    Bytes,
    Digest,
    Regex,
    RegexMatch,
    StringBuilder,
    ByteBuffer,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct HeapAccess {
    pub(super) instruction: u32,
    pub(super) kind: HeapAccessKind,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum HeapAccessKind {
    LoadCapture {
        value: ValueContract,
    },
    LoadField {
        receiver_class: u32,
        value: ValueContract,
    },
    StoreField {
        receiver_class: u32,
        value: ValueContract,
    },
    TupleGet {
        value: ValueContract,
    },
    IsType {
        target_class: u32,
    },
    CastType {
        target_class: u32,
    },
    ListLen,
    ListAt {
        value: ValueContract,
    },
    ListSet {
        value: ValueContract,
    },
    ListPush {
        value: ValueContract,
    },
    ListInsert {
        value: ValueContract,
    },
    ListRemove {
        value: ValueContract,
        swap: bool,
    },
    ListTruncate,
    ListReserve,
    ListReorder,
    ListCapacity,
    ListEpoch,
    ListIterLen,
    MapLen,
    MapHas {
        key: ValueContract,
    },
    MapAt {
        key: ValueContract,
        value: ValueContract,
    },
    MapGet {
        key: ValueContract,
    },
    MapPut {
        key: ValueContract,
    },
    MapNextIndex,
    MapKeyAt {
        value: ValueContract,
    },
    MapValueAt {
        value: ValueContract,
    },
    MapRemove {
        key: ValueContract,
    },
    MapClear,
    MapReserve,
    MapProbe,
    MapProbeKey {
        value: ValueContract,
    },
    MapProbeValue {
        value: ValueContract,
    },
    MapProbeSetValue {
        value: ValueContract,
    },
    MapProbeRemove {
        value: ValueContract,
    },
    MapInsertHashed {
        key: ValueContract,
        value: ValueContract,
    },
    MapWriteGuard,
    MapEpoch,
    MapIterLen,
    DigestCompare,
    AsCallback,
    SealInstance {
        class: u32,
    },
    BytesLen,
    BytesAt,
    BytesGet,
    TextByteLen,
    TextScalarLen,
    TextAtByte,
    TextAt,
    TextIsBoundary,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct OptionAccess {
    pub(super) instruction: u32,
    pub(super) family_type: u32,
    pub(super) kind: OptionAccessKind,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum OptionAccessKind {
    None,
    Payload { value: ValueContract },
    ListGet { value: ValueContract },
    ListPop { value: ValueContract },
    MapGet { value: ValueContract },
    MapRemove { value: ValueContract },
    MapPut { value: ValueContract, discard: bool },
    RegexCaptures { value: ValueContract },
    RegexMatchGroup { value: ValueContract },
    RegexMatchNamed { value: ValueContract },
    IsType { target: OptionTarget },
    CastType { target: OptionTarget },
}

#[derive(Debug, Clone, Copy)]
pub(super) enum OptionTarget {
    Family,
    Some,
    None,
}

#[derive(Debug, Clone)]
pub(super) struct CallContract {
    pub(super) params: Vec<ScalarKind>,
    /// Parameters that can receive one pending instance.
    pub(super) virtual_params: Vec<bool>,
    pub(super) local_count: Option<usize>,
    pub(super) result: ScalarKind,
    pub(super) receiver: Option<VirtualReceiver>,
    pub(super) value_target: Option<ValueCallTarget>,
    /// True when this call can return one transient instance.
    pub(super) virtual_result: bool,
    /// One constructor result that can stay in SSA values.
    pub(super) scalar_result: Option<ScalarReplacement>,
    /// Transitive behavior of one exact target.
    pub(super) behavior: crate::FunctionBehavior,
}

/// One constant field value in a scalar-replaced instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScalarConstant {
    pub(super) bits: u64,
    pub(super) tag: u64,
}

/// One source for a scalar-replaced field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScalarFieldSource {
    Parameter(u32),
    Constant(ScalarConstant),
}

/// One direct constructor that can produce SSA field values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ScalarReplacement {
    pub(super) site: u32,
    pub(super) class: u32,
    pub(super) frozen: bool,
    pub(super) fields: Vec<ScalarFieldSource>,
    pub(super) retired_cost: u32,
    pub(super) frame_count: u32,
    pub(super) stack_values: u32,
}

/// One SSA instance shape used by generated field loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScalarInstance {
    pub(super) class: u32,
    pub(super) field_count: u32,
    pub(super) frozen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ValueCallTarget {
    Closure,
    Callback,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum VirtualReceiver {
    Immediate { class: u32 },
    Object { tag: u32, class: u32 },
    Instance { class: u32 },
    Text { string: u32, substring: u32 },
}

#[derive(Debug, Clone, Copy)]
enum CallValueKind {
    Fixed(ScalarKind),
    Variable(u32),
}

#[derive(Debug, Clone)]
struct CallSignature {
    params: Vec<CallValueKind>,
    virtual_params: Vec<bool>,
    local_count: usize,
    result: CallValueKind,
    virtual_constructor: Option<VirtualConstructor>,
    behavior: crate::FunctionBehavior,
}

/// One generated constructor that can keep its fields in native storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VirtualConstructor {
    pub(super) class: u32,
    pub(super) field_count: u32,
    pub(super) object_local: u32,
}

/// One bounded direct callee that uses the shared segment emitter.
#[derive(Debug, Clone)]
pub(super) struct InlineFunctionPlan {
    pub(super) plan: Box<RegionPlan>,
    pub(super) max_path_cost: u32,
}

/// One interface target selected by immediate value tags.
#[derive(Debug, Clone, Copy)]
pub(super) struct GuardedInterfaceInline {
    pub(super) receiver: ScalarKind,
    pub(super) function: u32,
}

struct InlineSelection {
    functions: HashMap<u32, InlineFunctionPlan>,
    interfaces: HashMap<(u32, u32), Vec<GuardedInterfaceInline>>,
}

#[derive(Debug, Clone)]
pub(super) struct RegionPlan {
    pub(super) local_kinds: Vec<ScalarKind>,
    /// Locals whose list data stays fixed for this native call.
    pub(super) cached_list_data: Vec<bool>,
    /// Parameters whose list data stays fixed for this native call.
    pub(super) preloaded_list_data: Vec<bool>,
    pub(super) result_kind: ScalarKind,
    pub(super) virtual_constructor: Option<VirtualConstructor>,
    pub(super) scalar_instances: Vec<ScalarInstance>,
    pub(super) max_stack: usize,
    pub(super) max_stack_values: usize,
    pub(super) max_roots: usize,
    pub(super) segments: Vec<Segment>,
    pub(super) entries: std::collections::HashMap<(u32, u32), usize>,
    pub(super) inline_functions: HashMap<u32, InlineFunctionPlan>,
    pub(super) guarded_interface_inlines: HashMap<(u32, u32), Vec<GuardedInterfaceInline>>,
    pub(super) call_sites: usize,
    pub(super) inlined_call_sites: usize,
    pub(super) heap_read_sites: usize,
    pub(super) heap_write_sites: usize,
    pub(super) allocation_sites: usize,
    pub(super) collection_sites: usize,
    pub(super) effect_sites: usize,
    pub(super) interpreter_sites: usize,
    pub(super) type_resolution_sites: usize,
}

struct SegmentAnalysis {
    uses: Vec<bool>,
    definitions: Vec<bool>,
    exit_stack: Vec<ScalarKind>,
    max_stack: usize,
    max_stack_values: usize,
    boundary_stack: Vec<ScalarKind>,
    heap_accesses: Vec<HeapAccess>,
    option_accesses: Vec<OptionAccess>,
    fuel_stacks: Vec<(u32, Vec<ScalarKind>)>,
    replay_stacks: Vec<(u32, Vec<ScalarKind>)>,
    fault_stacks: Vec<(u32, Vec<ScalarKind>)>,
    allocations: Vec<AllocationSite>,
    call_contract: Option<CallContract>,
}

#[derive(Debug, Clone)]
struct VerifiedPoint {
    initialized: Vec<bool>,
    stack: Vec<ScalarKind>,
    stack_types: Vec<u32>,
}

struct SegmentAnalysisContext<'a> {
    func: &'a Func,
    source_func: &'a Func,
    module: &'a Module,
    locals: &'a [ScalarKind],
    calls: &'a HashMap<u32, CallSignature>,
    verified_types: &'a [BcType],
    class_relocation: Option<&'a [u32]>,
    runtime_core: lm_bytecode::corepin::CoreLayout,
}

#[derive(Debug, Clone)]
pub(super) struct AllocationSite {
    pub(super) instruction: u32,
    pub(super) stack: Vec<ScalarKind>,
}

impl RegionPlan {
    pub(super) fn operand_kinds(&self, block: u32, instruction: u32) -> Option<&[ScalarKind]> {
        if let Some(index) = self.entries.get(&(block, instruction)).copied() {
            return Some(&self.segments[index].entry_stack);
        }
        self.segments
            .iter()
            .find(|segment| {
                segment.block == block
                    && segment.end.checked_sub(1) == Some(instruction)
                    && matches!(
                        segment.exit,
                        SegmentExit::Call { .. }
                            | SegmentExit::VirtualCall { .. }
                            | SegmentExit::ValueCall { .. }
                            | SegmentExit::GenericVirtualCall { .. }
                            | SegmentExit::InterfaceCall { .. }
                            | SegmentExit::SlotCall { .. }
                            | SegmentExit::Effect { .. }
                            | SegmentExit::Boundary { .. }
                    )
            })
            .map(|segment| segment.boundary_stack.as_slice())
            .or_else(|| {
                self.segments.iter().find_map(|segment| {
                    segment
                        .fuel_stacks
                        .iter()
                        .find(|(at, _)| segment.block == block && *at == instruction)
                        .map(|(_, stack)| stack.as_slice())
                })
            })
            .or_else(|| {
                self.segments.iter().find_map(|segment| {
                    segment
                        .replay_stacks
                        .iter()
                        .find(|(at, _)| segment.block == block && *at == instruction)
                        .map(|(_, stack)| stack.as_slice())
                })
            })
    }

    pub(super) fn fault_operand_kinds(
        &self,
        block: u32,
        instruction: u32,
    ) -> Option<&[ScalarKind]> {
        self.segments.iter().find_map(|segment| {
            segment
                .fault_stacks
                .iter()
                .find(|(at, _)| segment.block == block && *at == instruction)
                .map(|(_, stack)| stack.as_slice())
        })
    }

    pub(super) fn suspended_operand_kinds(
        &self,
        block: u32,
        instruction: u32,
    ) -> Option<&[ScalarKind]> {
        self.segments.iter().find_map(|segment| {
            let fallthrough_ip = match segment.exit {
                SegmentExit::Call { fallthrough_ip, .. }
                | SegmentExit::VirtualCall { fallthrough_ip, .. }
                | SegmentExit::ValueCall { fallthrough_ip }
                | SegmentExit::GenericVirtualCall { fallthrough_ip, .. }
                | SegmentExit::InterfaceCall { fallthrough_ip, .. }
                | SegmentExit::SlotCall { fallthrough_ip, .. } => fallthrough_ip,
                _ => return None,
            };
            if segment.block != block || fallthrough_ip != instruction {
                return None;
            }
            let parameters = segment.call_contract.as_ref()?.params.len();
            let callable = usize::from(matches!(segment.exit, SegmentExit::ValueCall { .. }));
            let prefix = segment
                .boundary_stack
                .len()
                .checked_sub(parameters.checked_add(callable)?)?;
            Some(&segment.boundary_stack[..prefix])
        })
    }

    pub(super) fn materialization_operand_kinds(
        &self,
        kind: ExitKind,
        block: u32,
        instruction: u32,
    ) -> Option<&[ScalarKind]> {
        match kind {
            ExitKind::Return => Some(&[]),
            ExitKind::Replay => self.operand_kinds(block, instruction),
            ExitKind::IntegerOverflow
            | ExitKind::DivideByZero
            | ExitKind::TypeMismatch
            | ExitKind::UninitializedField
            | ExitKind::HeapLimit
            | ExitKind::Unreachable
            | ExitKind::GuestFault => self.fault_operand_kinds(block, instruction),
            ExitKind::StackLimit => self.fault_operand_kinds(block, instruction).or_else(|| {
                instruction
                    .checked_sub(1)
                    .and_then(|at| self.operand_kinds(block, at))
            }),
            _ => self.operand_kinds(block, instruction),
        }
    }

    pub(super) fn for_function(input: &FunctionInput<'_>) -> Result<RegionPlan, UnsupportedReason> {
        Self::for_function_mode(input, true)
    }

    fn for_function_mode(
        input: &FunctionInput<'_>,
        select_inlines: bool,
    ) -> Result<RegionPlan, UnsupportedReason> {
        let runtime = input.root.runtime;
        let instructions = runtime
            .blocks
            .iter()
            .try_fold(0usize, |total, block| total.checked_add(block.len()));
        if runtime.local_types.len() > MAX_REGION_LOCALS
            || instructions.is_none_or(|count| count > MAX_REGION_INSTRUCTIONS)
        {
            return Err(UnsupportedReason::RegionLimit);
        }
        let source = input.root.source;
        let source_func = source
            .funcs
            .get(input.root.source_function as usize)
            .ok_or(UnsupportedReason::MissingSource)?;
        if source_func.blocks.len() != runtime.blocks.len()
            || source_func.local_types.len() != runtime.local_types.len()
            || source_func
                .blocks
                .iter()
                .zip(&runtime.blocks)
                .any(|(source, linked)| source.len() != linked.len())
        {
            return Err(UnsupportedReason::InvalidControlFlow);
        }
        let mut segments = split_segments(runtime)?;
        let mut requested_points = Vec::new();
        for (block, instructions) in runtime.blocks.iter().enumerate() {
            let block = u32::try_from(block).map_err(|_| UnsupportedReason::RegionLimit)?;
            for instruction in 0..=instructions.len() {
                let instruction =
                    u32::try_from(instruction).map_err(|_| UnsupportedReason::RegionLimit)?;
                requested_points.push((block, instruction));
            }
        }
        let metadata = lm_verify::verify_function_metadata_at_with_bundle(
            source,
            input.root.bundle,
            input.root.source_function,
            &requested_points,
        )
        .map_err(|_| UnsupportedReason::MissingSource)?;
        let verified_points: HashMap<(u32, u32), VerifiedPoint> = requested_points
            .into_iter()
            .zip(metadata.points())
            .filter_map(|(point, state)| state.as_ref().map(|state| (point, state)))
            .map(|(point, state)| {
                let stack = state
                    .stack()
                    .iter()
                    .map(|ty| scalar_kind_in(source, metadata.types(), *ty))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((
                    point,
                    VerifiedPoint {
                        initialized: state.locals().iter().map(Option::is_some).collect(),
                        stack,
                        stack_types: state.stack().to_vec(),
                    },
                ))
            })
            .collect::<Result<_, UnsupportedReason>>()?;
        segments.retain(|segment| verified_points.contains_key(&(segment.block, segment.start)));
        let local_kinds = source_func
            .local_types
            .iter()
            .map(|ty| scalar_kind(source, *ty))
            .collect::<Result<Vec<_>, _>>()?;
        let result_kind = scalar_kind(source, source_func.ret)?;
        let call_contracts = call_contracts(input)?;
        let virtual_constructor = call_contracts
            .get(&input.root.function)
            .and_then(|signature| signature.virtual_constructor);
        let virtual_parameters = call_contracts
            .get(&input.root.function)
            .map(|signature| signature.virtual_params.as_slice())
            .unwrap_or(&[]);
        let entries: std::collections::HashMap<(u32, u32), usize> = segments
            .iter()
            .enumerate()
            .map(|(index, segment)| ((segment.block, segment.start), index))
            .collect();
        resolve_successors(&mut segments, &entries)?;
        let mut max_stack = 0;
        let mut max_stack_values = 0;
        let mut call_sites = 0;
        let mut heap_read_sites = 0;
        let mut heap_write_sites = 0;
        let mut allocation_sites = 0;
        let mut collection_sites = 0;
        let mut list_growth_sites = 0;
        let mut calls_may_grow_list = false;
        let mut effect_sites = 0;
        let interpreter_sites = 0;
        let mut type_resolution_sites = 0;
        let analysis_context = SegmentAnalysisContext {
            func: runtime,
            source_func,
            module: source,
            locals: &local_kinds,
            calls: &call_contracts,
            verified_types: metadata.types(),
            class_relocation: input.root.class_relocation,
            runtime_core: input.runtime_core,
        };
        for (index, segment) in segments.iter_mut().enumerate() {
            let state = verified_points
                .get(&(segment.block, segment.start))
                .ok_or(UnsupportedReason::InvalidControlFlow)?;
            let entry_stack = state.stack.clone();
            segment.entry_stack = entry_stack.clone();
            let analysis = analyze_segment(&analysis_context, segment, &verified_points)?;
            segment.uses = analysis.uses;
            segment.definitions = analysis.definitions;
            segment.call_contract = analysis.call_contract;
            segment.exit_stack = analysis.exit_stack.clone();
            segment.boundary_stack = analysis.boundary_stack;
            segment.heap_accesses = analysis.heap_accesses;
            segment.option_accesses = analysis.option_accesses;
            type_resolution_sites += segment.option_accesses.len();
            heap_read_sites += segment
                .option_accesses
                .iter()
                .filter(|access| {
                    matches!(
                        access.kind,
                        OptionAccessKind::ListGet { .. } | OptionAccessKind::MapGet { .. }
                    )
                })
                .count();
            segment.fuel_stacks = analysis.fuel_stacks;
            for access in &segment.heap_accesses {
                match access.kind {
                    HeapAccessKind::StoreField { .. }
                    | HeapAccessKind::ListSet { .. }
                    | HeapAccessKind::ListPush { .. }
                    | HeapAccessKind::ListInsert { .. }
                    | HeapAccessKind::ListRemove { .. }
                    | HeapAccessKind::ListTruncate
                    | HeapAccessKind::ListReserve
                    | HeapAccessKind::ListReorder
                    | HeapAccessKind::ListEpoch
                    | HeapAccessKind::MapEpoch
                    | HeapAccessKind::MapPut { .. }
                    | HeapAccessKind::MapRemove { .. }
                    | HeapAccessKind::MapClear
                    | HeapAccessKind::MapReserve
                    | HeapAccessKind::MapProbeSetValue { .. }
                    | HeapAccessKind::MapProbeRemove { .. }
                    | HeapAccessKind::MapInsertHashed { .. }
                    | HeapAccessKind::MapWriteGuard
                    | HeapAccessKind::SealInstance { .. } => {
                        heap_write_sites += 1;
                        if matches!(
                            access.kind,
                            HeapAccessKind::ListPush { .. }
                                | HeapAccessKind::ListInsert { .. }
                                | HeapAccessKind::ListReserve
                                | HeapAccessKind::MapReserve
                                | HeapAccessKind::MapInsertHashed { .. }
                        ) {
                            collection_sites += 1;
                        }
                        if matches!(
                            access.kind,
                            HeapAccessKind::ListPush { .. }
                                | HeapAccessKind::ListInsert { .. }
                                | HeapAccessKind::ListReserve
                        ) {
                            list_growth_sites += 1;
                        }
                    }
                    _ => heap_read_sites += 1,
                }
            }
            segment.replay_stacks = analysis.replay_stacks;
            segment.fault_stacks = analysis.fault_stacks;
            segment.allocations = analysis.allocations;
            allocation_sites += segment.allocations.len();
            segment.cost = segment.end - segment.start;
            if matches!(
                segment.exit,
                SegmentExit::Call { .. }
                    | SegmentExit::VirtualCall { .. }
                    | SegmentExit::ValueCall { .. }
                    | SegmentExit::GenericVirtualCall { .. }
                    | SegmentExit::InterfaceCall { .. }
                    | SegmentExit::SlotCall { .. }
            ) {
                call_sites += 1;
                calls_may_grow_list |= segment
                    .call_contract
                    .as_ref()
                    .is_none_or(|contract| contract.behavior.may_grow_list());
                segment.cost = segment.cost.saturating_sub(1);
            }
            if matches!(segment.exit, SegmentExit::Effect { .. }) {
                segment.cost = segment.cost.saturating_sub(1);
                effect_sites += 1;
            }
            if matches!(segment.exit, SegmentExit::Boundary { .. }) {
                segment.cost = segment.cost.saturating_sub(1);
            }
            max_stack = max_stack.max(analysis.max_stack);
            max_stack_values = max_stack_values.max(analysis.max_stack_values);
            debug_assert_eq!(index, entries[&(segment.block, segment.start)]);
        }
        compute_reserved_costs(&mut segments)?;
        for segment in &segments {
            for successor in &segment.successors {
                if !stacks_use_equal_representations(
                    &segment.exit_stack,
                    &segments[*successor].entry_stack,
                ) {
                    return Err(UnsupportedReason::InvalidControlFlow);
                }
            }
        }
        if max_stack > MAX_REGION_STACK {
            return Err(UnsupportedReason::RegionLimit);
        }
        compute_liveness(&mut segments, local_kinds.len());
        compute_dirty_locals(&mut segments, local_kinds.len());
        let scalar_instances = select_virtual_results(
            input,
            runtime,
            source,
            source_func,
            &mut segments,
            local_kinds.len(),
            virtual_constructor,
        )?;
        compute_virtual_flow(
            runtime,
            source,
            source_func,
            &mut segments,
            local_kinds.len(),
            virtual_constructor,
            virtual_parameters,
        )?;
        compute_fuel_reserves(&mut segments)?;
        for segment in &mut segments {
            segment.defer_integer_overflow = can_defer_integer_overflow(runtime, segment);
            if segment.defer_integer_overflow
                && !segment
                    .replay_stacks
                    .iter()
                    .any(|(instruction, _)| *instruction == segment.start)
            {
                segment
                    .replay_stacks
                    .push((segment.start, segment.entry_stack.clone()));
            }
        }
        let stable_list_data = !calls_may_grow_list && list_growth_sites == 0;
        let cached_list_data: Vec<bool> = source_func
            .local_types
            .iter()
            .copied()
            .enumerate()
            .map(|(slot, ty)| {
                stable_list_data
                    && matches!(source.types.get(ty as usize), Some(BcType::List(_)))
                    && segments.iter().any(|segment| segment.uses[slot])
            })
            .collect();
        let preloaded_list_data = cached_list_data
            .iter()
            .copied()
            .enumerate()
            .map(|(slot, cached)| {
                cached
                    && slot < source_func.params.len()
                    && segments.iter().all(|segment| !segment.definitions[slot])
            })
            .collect();
        let root_local_count = local_kinds
            .iter()
            .filter(|kind| is_root_kind(**kind))
            .count();
        let max_roots = root_local_count
            .checked_add(max_stack)
            .ok_or(UnsupportedReason::RegionLimit)?;
        let inline_selection = if select_inlines {
            select_inline_functions(input, &mut segments)?
        } else {
            InlineSelection {
                functions: HashMap::new(),
                interfaces: HashMap::new(),
            }
        };
        let inline_functions = inline_selection.functions;
        let guarded_interface_inlines = inline_selection.interfaces;
        let inlined_call_sites = segments
            .iter()
            .filter(|segment| match segment.exit {
                SegmentExit::Call {
                    target, app: None, ..
                } => inline_functions.contains_key(&target),
                SegmentExit::InterfaceCall {
                    interface, method, ..
                } => guarded_interface_inlines
                    .get(&(interface, method))
                    .is_some_and(|targets| !targets.is_empty()),
                _ => false,
            })
            .count();
        Ok(RegionPlan {
            local_kinds,
            cached_list_data,
            preloaded_list_data,
            result_kind,
            virtual_constructor,
            scalar_instances,
            max_stack,
            max_stack_values,
            max_roots,
            segments,
            entries,
            inline_functions,
            guarded_interface_inlines,
            call_sites,
            inlined_call_sites,
            heap_read_sites,
            heap_write_sites,
            allocation_sites,
            collection_sites,
            effect_sites,
            interpreter_sites,
            type_resolution_sites,
        })
    }

    pub(super) fn distance_to_entry(&self, block: u32, instruction: u32) -> Option<u32> {
        self.segments
            .iter()
            .find(|segment| {
                segment.block == block && segment.start <= instruction && instruction < segment.end
            })
            .map(|segment| segment.end - instruction)
    }
}

fn select_inline_functions(
    input: &FunctionInput<'_>,
    segments: &mut [Segment],
) -> Result<InlineSelection, UnsupportedReason> {
    const MAX_INLINE_INSTRUCTIONS: usize = 32;
    const MAX_INLINE_SEGMENTS: usize = 12;
    const MAX_INLINE_BUDGET: usize = 96;

    let mut plans = HashMap::new();
    let mut interface_plans = HashMap::<(u32, u32), Vec<GuardedInterfaceInline>>::new();
    let mut expanded = 0usize;
    let call_counts = segments.iter().fold(HashMap::new(), |mut counts, segment| {
        if let SegmentExit::Call {
            target, app: None, ..
        } = segment.exit
        {
            *counts.entry(target).or_insert(0usize) += 1;
        }
        counts
    });
    let mut call_counts = call_counts;
    for candidate in &input.guarded_interface_callees {
        let count = segments
            .iter()
            .filter(|segment| {
                matches!(
                    segment.exit,
                    SegmentExit::InterfaceCall {
                        interface,
                        method,
                        ..
                    } if interface == candidate.interface && method == candidate.method
                )
            })
            .count();
        if count != 0 {
            *call_counts.entry(candidate.function).or_insert(0) += count;
        }
    }
    for segment in segments.iter_mut() {
        let SegmentExit::Call {
            target, app: None, ..
        } = segment.exit
        else {
            continue;
        };
        if plans.contains_key(&target) {
            ensure_inline_replay(segment);
            continue;
        }
        if let Some((plan, cost)) = make_inline_function_plan(
            input,
            target,
            call_counts.get(&target).copied().unwrap_or(1),
            expanded,
            MAX_INLINE_INSTRUCTIONS,
            MAX_INLINE_SEGMENTS,
            MAX_INLINE_BUDGET,
        )? {
            expanded = expanded.saturating_add(cost);
            plans.insert(target, plan);
            ensure_inline_replay(segment);
        }
    }
    for candidate in &input.guarded_interface_callees {
        let key = (candidate.interface, candidate.method);
        if !segments.iter().any(|segment| {
            matches!(
                segment.exit,
                SegmentExit::InterfaceCall {
                    interface,
                    method,
                    ..
                } if (interface, method) == key
            )
        }) {
            continue;
        }
        if let std::collections::hash_map::Entry::Vacant(entry) = plans.entry(candidate.function) {
            let Some((plan, cost)) = make_inline_function_plan(
                input,
                candidate.function,
                call_counts.get(&candidate.function).copied().unwrap_or(1),
                expanded,
                MAX_INLINE_INSTRUCTIONS,
                MAX_INLINE_SEGMENTS,
                MAX_INLINE_BUDGET,
            )?
            else {
                continue;
            };
            expanded = expanded.saturating_add(cost);
            entry.insert(plan);
        }
        let targets = interface_plans.entry(key).or_default();
        if !targets.iter().any(|target| {
            target.receiver == candidate.receiver && target.function == candidate.function
        }) {
            targets.push(GuardedInterfaceInline {
                receiver: candidate.receiver,
                function: candidate.function,
            });
        }
        for segment in segments.iter_mut() {
            if matches!(
                segment.exit,
                SegmentExit::InterfaceCall {
                    interface,
                    method,
                    ..
                } if (interface, method) == key
            ) {
                ensure_inline_replay(segment);
            }
        }
    }
    Ok(InlineSelection {
        functions: plans,
        interfaces: interface_plans,
    })
}

#[allow(clippy::too_many_arguments)]
fn make_inline_function_plan(
    input: &FunctionInput<'_>,
    target: u32,
    call_count: usize,
    expanded: usize,
    max_instructions: usize,
    max_segments: usize,
    max_budget: usize,
) -> Result<Option<(InlineFunctionPlan, usize)>, UnsupportedReason> {
    let behavior = input.behavior(target);
    if behavior.may_suspend_or_perform()
        || behavior.may_collect()
        || behavior.may_mutate()
        || behavior.has_dynamic_call()
    {
        return Ok(None);
    }
    let Some(definition) = input.definition(target) else {
        return Ok(None);
    };
    let instruction_count = definition
        .runtime
        .blocks
        .iter()
        .map(Vec::len)
        .sum::<usize>();
    let expanded_instructions = instruction_count.saturating_mul(call_count);
    if instruction_count == 0
        || instruction_count > max_instructions
        || definition.runtime.type_params != 0
        || definition.runtime.effect_params != 0
        || !definition.runtime.captures.is_empty()
        || definition
            .runtime
            .blocks
            .iter()
            .flatten()
            .any(|instruction| {
                !matches!(
                    crate::instruction_treatment(instruction).class(),
                    crate::TreatmentClass::Inline | crate::TreatmentClass::Guarded
                )
            })
        || expanded.saturating_add(expanded_instructions) > max_budget
    {
        return Ok(None);
    }
    let Some(child) = input.child(target) else {
        return Ok(None);
    };
    let Ok(mut plan) = RegionPlan::for_function_mode(&child, false) else {
        return Ok(None);
    };
    if plan.segments.len() > max_segments
        || plan.call_sites != 0
        || plan.heap_write_sites != 0
        || plan.allocation_sites != 0
        || plan.effect_sites != 0
        || plan.interpreter_sites != 0
        || plan.type_resolution_sites != 0
        || plan.virtual_constructor.is_some()
        || !plan.scalar_instances.is_empty()
    {
        return Ok(None);
    }
    let Some(max_path_cost) = acyclic_max_path_cost(&plan) else {
        return Ok(None);
    };
    for segment in &mut plan.segments {
        segment.live_in.fill(true);
    }
    plan.cached_list_data.fill(false);
    plan.preloaded_list_data.fill(false);
    Ok(Some((
        InlineFunctionPlan {
            plan: Box::new(plan),
            max_path_cost,
        },
        expanded_instructions,
    )))
}

fn ensure_inline_replay(segment: &mut Segment) {
    if segment.replay_stacks.is_empty() {
        segment
            .replay_stacks
            .push((segment.start, segment.entry_stack.clone()));
    }
}

fn acyclic_max_path_cost(plan: &RegionPlan) -> Option<u32> {
    fn visit(plan: &RegionPlan, index: usize, states: &mut [u8], costs: &mut [u32]) -> Option<u32> {
        match *states.get(index)? {
            1 => return None,
            2 => return costs.get(index).copied(),
            _ => {}
        }
        states[index] = 1;
        let tail = plan.segments[index]
            .successors
            .iter()
            .copied()
            .map(|successor| visit(plan, successor, states, costs))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .max()
            .unwrap_or(0);
        let cost = plan.segments[index].cost.checked_add(tail)?;
        costs[index] = cost;
        states[index] = 2;
        Some(cost)
    }

    let entry = plan.entries.get(&(0, 0)).copied()?;
    let mut states = vec![0; plan.segments.len()];
    let mut costs = vec![0; plan.segments.len()];
    visit(plan, entry, &mut states, &mut costs)
}

mod calls;
mod contracts;
mod flow;
mod segments;

use calls::*;
use contracts::*;
use flow::*;
use segments::*;

pub(crate) use calls::is_root_kind;
pub use calls::type_has_native_representation;
pub(crate) use flow::{compute_dirty_locals, compute_liveness, transfer_virtual_instruction};
pub(crate) use segments::{bypasses_fuel_check, split_segments};
