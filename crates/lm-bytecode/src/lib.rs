//! Loom bytecode formats.
//!
//! This crate defines two forms:
//! - a compact serialized byte format for storage and transfer;
//! - a fixed-size decoded instruction form for the verifier and the VM.
//!
//! The decoder validates structure only. The independent verifier in
//! `lm-verify` validates tables, types, rows, type applications,
//! jumps, calls, and stack shapes.

pub mod artifact;
pub mod closed;
pub mod corepin;
pub mod debug;
pub mod hash;
pub mod identity;
pub mod interface;
pub mod stack;

pub use stack::{stack_effect, StackEffectTables};

use std::fmt;

/// The sentinel that encodes "no parent class".
pub const NO_PARENT: u32 = u32::MAX;

/// Sentinel for an interface method without a default function.
pub const NO_FUNC: u32 = u32::MAX;

/// The reserved module path of the pinned core image.
///
/// Every module embeds one copy of the core, and every copy carries
/// the same qualified keys. A source module path never equals this
/// value, so a user class never takes a core key.
pub const CORE_MODULE: &str = "core";

/// The sentinel for an unfilled core role slot.
pub const NO_ROLE: u32 = u32::MAX;
/// The sentinel for a slot instruction without a type application.
pub const NO_APP: u32 = u32::MAX;

/// The largest interface or method index in one interface call site.
pub const MAX_INTERFACE_CALL_INDEX: u32 = u16::MAX as u32;

/// Pack one interface index and one method index into one call site.
pub const fn pack_interface_call_site(interface: u32, method: u32) -> Option<u32> {
    if interface > MAX_INTERFACE_CALL_INDEX || method > MAX_INTERFACE_CALL_INDEX {
        None
    } else {
        Some((interface << 16) | method)
    }
}

/// Unpack one interface call site into its interface and method indices.
pub const fn unpack_interface_call_site(site: u32) -> (u32, u32) {
    (site >> 16, site & MAX_INTERFACE_CALL_INDEX)
}

/// The number of stable core role slots. The order is
/// `corepin::PINNED_LABELS`.
pub const CORE_ROLE_COUNT: usize = 261;

/// Join a module path and a declaration name into one qualified key.
///
/// The key is the nominal identity of a class (specification 8.6). An
/// empty module path names a single-file module, which has no path.
pub fn qualified_key(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_string()
    } else {
        format!("{module}.{name}")
    }
}

/// Derive one stable late-binding slot key from its binding and contract.
pub fn slot_key(binding: &str, contract_hash: &[u8; 32]) -> [u8; 32] {
    let mut bytes = b"lm-slot-key-v2\0".to_vec();
    bytes.extend_from_slice(binding.as_bytes());
    bytes.extend_from_slice(contract_hash);
    hash::hash256(&bytes)
}

/// Derive one test or host slot key without a source binding contract.
pub fn ad_hoc_slot_key(binding: &str) -> [u8; 32] {
    let mut bytes = b"lm-ad-hoc-slot-key-v1\0".to_vec();
    bytes.extend_from_slice(binding.as_bytes());
    hash::hash256(&bytes)
}

/// One element of an effect row in the serialized module.
///
/// `Op` names one operation in the exact ABI bundle.
/// `Group` names one group in the exact ABI bundle.
/// `Var` names one effect parameter of the enclosing function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BcRow {
    Op(u32),
    Group(u32),
    Var(u32),
}

/// One type application: the generic arguments of a call site or an
/// allocation site. `types` aligns with the callee type parameters.
/// `rows` aligns with the callee effect parameters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeApp {
    pub types: Vec<u32>,
    pub rows: Vec<Vec<BcRow>>,
}

/// One applied nominal interface in bytecode metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BcInterfaceUse {
    pub interface: u32,
    pub types: Vec<u32>,
    pub rows: Vec<Vec<BcRow>>,
}

/// One entry in the module type table.
///
/// Types reference other types by index. A canonical table only
/// references earlier entries and holds no duplicate entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BcType {
    Unit,
    Never,
    Bool,
    Int,
    Float,
    Str,
    /// An instance type of a class without generic parameters.
    Class(u32),
    /// An instance type of a generic class applied to arguments.
    Inst(u32, Vec<u32>),
    /// A list type with one element type index.
    List(u32),
    /// A map type with a key type index and a value type index.
    Map(u32, u32),
    /// A tuple type with element type indices.
    Tuple(Vec<u32>),
    /// A function value type: parameters, parameter `mut` markers,
    /// result, and effect row. The marker vector length equals the
    /// parameter vector length by construction of the decoder; the
    /// verifier checks hand-built modules.
    Fn(Vec<u32>, Vec<bool>, u32, Vec<BcRow>),
    /// A function value that cannot escape the active call chain.
    Callback(Vec<u32>, Vec<bool>, u32, Vec<BcRow>),
    /// One type parameter of the enclosing generic function.
    Var(u32),
    /// One associated type selected through a nominal interface.
    Projection {
        base: u32,
        interface: u32,
        assoc: u32,
    },
    /// The frozen machine `Fault` value type.
    Fault,
    /// The opaque pending-request token type.
    Request,
    /// The holder-local policy-table handle type.
    PolicyTable,
    /// The persistent virtual machine image type.
    Vm,
    /// One active invocation typed by its terminal result index.
    Run(u32),
    /// A holder-local one-shot wait typed by its result index.
    Wait(u32),
    /// A typed pending call: argument-view type index and reply type
    /// index.
    PendingCall(u32, u32),
    /// A proc handle: mailbox message type index and terminal result
    /// type index.
    Handle(u32, u32),
    /// An identity-indexed operation value: the manifest operation
    /// slot and the function type index.
    Op(u32, u32),
    /// The frozen canonical graph digest of one value.
    Digest,
    /// One admitted VM snapshot without a distinguished result type.
    VmSnapshot,
    /// One VM snapshot with a distinguished run result type.
    RunSnapshot(u32),
    /// Immutable binary data.
    Bytes,
    /// A typed file resource designator.
    FileHandle,
    /// A holder-local resource-management designator.
    ResourceHandle,
    /// An opaque extension host resource designator.
    HostResource,
}

/// The declaration kind of one class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BcClassKind {
    /// An ordinary class.
    Normal,
    /// The abstract closed parent of one enum family. It cannot be
    /// allocated.
    Abstract,
    /// One final case class of an enum family. It cannot be a parent.
    Case,
}

/// One class-table entry. Fields hold the full layout: inherited
/// fields first, own fields after them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BcClass {
    pub name: String,
    /// The qualified key: the nominal identity of the class
    /// (specification 8.6). The linker merges on this value plus the
    /// structural hash. It never enters the class's own structural
    /// hash.
    pub key: String,
    /// True when the class cannot have a subclass.
    pub is_final: bool,
    /// True when completed instances are always frozen.
    pub is_frozen: bool,
    /// Parent class index, or `NO_PARENT`.
    pub parent: u32,
    /// Type arguments of a generic parent, as type indices. Empty
    /// when the parent declares no type parameters.
    pub parent_args: Vec<u32>,
    /// The number of generic type parameters.
    pub type_params: u32,
    pub kind: BcClassKind,
    /// Full field layout: `(name, type index)`.
    pub fields: Vec<(String, u32)>,
    /// Own method table: `(selector index, function index)`.
    pub methods: Vec<(u32, u32)>,
    /// Field default markers, aligned with `fields`.
    pub field_defaults: Vec<bool>,
    /// The first field declared by this class.
    pub own_start: u32,
    /// True when the source class declares `init`.
    pub has_init: bool,
}

/// One associated type requirement of a nominal interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BcAssociated {
    pub name: String,
    pub bounds: Vec<BcInterfaceUse>,
}

/// One method requirement of a nominal interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BcInterfaceMethod {
    pub selector: u32,
    pub mut_self: bool,
    /// Method-owned type parameters.
    pub type_params: u32,
    pub type_bounds: Vec<Vec<BcInterfaceUse>>,
    /// Method-owned effect parameters.
    pub effect_params: u32,
    pub premises: Vec<BcTypePremise>,
    pub params: Vec<u32>,
    pub param_muts: Vec<bool>,
    /// Declared parameter names, aligned with `params`.
    pub param_names: Vec<String>,
    pub ret: u32,
    pub row: Vec<BcRow>,
    /// The interface-owned default function, or `NO_FUNC`.
    pub default: u32,
}

/// One type premise on an interface method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BcTypePremise {
    pub subject: u32,
    pub bounds: Vec<BcInterfaceUse>,
}

/// One nominal interface contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BcInterface {
    pub name: String,
    pub key: String,
    pub type_params: u32,
    pub effect_params: u32,
    pub generic_is_effect: Vec<bool>,
    pub parents: Vec<BcInterfaceUse>,
    pub type_bounds: Vec<Vec<BcInterfaceUse>>,
    pub associated: Vec<BcAssociated>,
    pub methods: Vec<BcInterfaceMethod>,
}

/// One explicit class-owned interface conformance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BcConformancePremise {
    pub param: u32,
    pub bounds: Vec<BcInterfaceUse>,
}

/// One explicit class-owned interface conformance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BcConformance {
    pub class: u32,
    pub application: BcInterfaceUse,
    pub premises: Vec<BcConformancePremise>,
    pub associated: Vec<u32>,
    /// One entry per interface method.
    /// True selects compatible class dispatch. False selects the default.
    pub method_overrides: Vec<bool>,
}

/// One callable contract used by a late-bound slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BcCallableContract {
    pub type_params: u32,
    pub effect_params: u32,
    pub type_bounds: Vec<Vec<BcInterfaceUse>>,
    pub params: Vec<u32>,
    pub param_muts: Vec<bool>,
    pub ret: u32,
    pub row: Vec<BcRow>,
}

/// The immutable contract of one late-bound VM slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotContract {
    Function(BcCallableContract),
    Method(BcCallableContract),
    Class {
        type_params: u32,
        abi: [u8; 32],
        ty: u32,
        constructor: BcCallableContract,
    },
    Value {
        ty: u32,
    },
    Process {
        message: u32,
        result: u32,
    },
}

/// One portable initial target for a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotTarget {
    Function(u32),
    /// A compatible nominal class and its current construction function.
    Class {
        class: u32,
        constructor: u32,
    },
}

/// One portable late-binding declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotSpec {
    /// The canonical source binding of this slot.
    pub binding: String,
    /// True when compiled calls read this slot.
    pub late: bool,
    pub key: [u8; 32],
    /// The intrinsic, body-independent contract identity.
    pub contract_hash: [u8; 32],
    pub contract: SlotContract,
    pub initial: Option<SlotTarget>,
}

impl BcClass {
    pub fn parent(&self) -> Option<u32> {
        if self.parent == NO_PARENT {
            None
        } else {
            Some(self.parent)
        }
    }
}

/// One decoded instruction. The form is a fixed-size Rust enum.
///
/// Jump operands name a target basic block, not a raw byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Use one-byte tags to keep interpreter instructions compact.
#[repr(u8)]
pub enum Instr {
    /// Push the unit value.
    ConstUnit,
    /// Push a Bool constant.
    ConstBool(bool),
    /// Push an Int constant.
    ConstInt(i64),
    /// Push one canonical IEEE 754 binary64 constant.
    ConstFloat(u64),
    /// Push one Unicode scalar value.
    ConstChar(u32),
    /// Allocate the module string with this pool index and push it.
    ConstStr(u32),
    /// Allocate the module byte literal with this pool index.
    ConstBytes(u32),
    /// Push the compiled regular expression for this string pool index.
    ConstRegex(u32),
    /// Push the value of a local slot.
    LoadLocal(u32),
    /// Pop one value into a local slot.
    StoreLocal(u32),
    /// Pop and discard one value.
    Pop,
    /// Checked Int add. Overflow faults.
    Add,
    /// Checked Int subtract. Overflow faults.
    Sub,
    /// Checked Int multiply. Overflow faults.
    Mul,
    /// Int division that truncates toward zero. Zero divisor faults.
    Div,
    /// Int remainder with the dividend sign. Zero divisor faults.
    Rem,
    /// Checked Int negation. Overflow faults.
    Neg,
    /// Bool negation.
    Not,
    LtInt,
    LeInt,
    GtInt,
    GeInt,
    EqInt,
    NeInt,
    EqBool,
    NeBool,
    /// Run one native value instruction.
    Native(NativeInstr),
    /// Run one numeric or bitwise instruction from the extended family.
    Numeric(NumericInstr),
    /// Reference identity equality for heap objects.
    EqRef,
    NeRef,
    /// Direct call of a non-generic function by table index.
    Call(u32),
    /// Direct call of a generic function with a type application.
    CallG {
        func: u32,
        app: u32,
    },
    /// Virtual call: pop `argc` arguments over one receiver, select
    /// the method through the runtime class and the selector slot.
    CallVirtual {
        selector: u32,
        argc: u32,
    },
    /// Virtual call with a type application for the receiver class
    /// arguments plus the method's own generic arguments.
    CallVirtualG {
        selector: u32,
        argc: u32,
        app: u32,
    },
    /// Call a closure value: pop `argc` arguments over the closure.
    CallValue {
        argc: u32,
    },
    /// Allocate a closure over `captures` popped values for `func`.
    MakeClosure {
        func: u32,
        captures: u32,
    },
    /// Push one captured value of the active closure.
    LoadCapture(u32),
    /// Allocate an instance of a non-generic class. Fields start
    /// without a value.
    New(u32),
    /// Allocate an instance of a generic class with a type application.
    NewG {
        class: u32,
        app: u32,
    },
    /// Pop an instance and push one field value.
    LoadField(u32),
    /// Pop a value and an instance, then write the field.
    StoreField(u32),
    /// Allocate a tuple of the given tuple type from `count` popped
    /// values. Tuples are born frozen.
    TupleNew {
        ty: u32,
        count: u32,
    },
    /// Pop a tuple and push the element at a fixed position.
    TupleGet(u32),
    /// Pop an instance and push whether its class matches the target
    /// type or extends it.
    IsType(u32),
    /// Pop an instance. Fault `BadCast` unless its class matches the
    /// target type or extends it. Push the instance at the target type.
    CastType(u32),
    /// Allocate a list of the given list type from `count` popped values.
    ListNew {
        ty: u32,
        count: u32,
    },
    /// Pop a list and push its length.
    ListLen,
    /// Pop an index and a list, then push the element. Faults
    /// `IndexOutOfBounds` outside the range.
    ListAt,
    /// Pop a value and a list, then append. Pushes unit.
    ListPush,
    /// Allocate a map of the given map type from `count` popped pairs.
    MapNew {
        ty: u32,
        count: u32,
    },
    /// Pop a map and push its entry count.
    MapLen,
    /// Pop a key and a map, then push whether the key exists.
    MapHas,
    /// Pop a key and a map, then push the value. Faults `MissingKey`.
    MapAt,
    /// Pop a value, a key, and a map. Insert or replace the entry.
    /// Push the old value unless `discard` is true.
    MapPut {
        ty: u32,
        discard: bool,
    },
    /// Pop an object reference, freeze its graph, push the same reference.
    Freeze,
    /// Pop a frozen object of `ty` and push its canonical digest.
    Digest {
        ty: u32,
    },
    /// Pop two digests and push their value equality.
    EqDigest,
    /// Pop two digests and push their value inequality.
    NeDigest,
    /// Unconditional jump to a block. Ends the block.
    Jump(u32),
    /// Pop a Bool. Jump to the block when the value is false.
    JumpIfFalse(u32),
    /// Pop a Bool. Jump to the block when the value is true.
    JumpIfTrue(u32),
    /// Pop the result value and return it. Ends the block.
    Return,
    /// Perform the exact manifest operation `op` over `argc` popped
    /// arguments and push the reply.
    ///
    /// `reply_ty` is the module type index of the reply. It may name a
    /// type variable of the performing function, and `Frame.env`
    /// closes it at run time. The verifier proves that it equals the
    /// type the dataflow pushes, so the value comes from verified code
    /// and never from a snapshot container. The world checks the reply
    /// value against it at every boundary crossing.
    Perform {
        op: u32,
        argc: u32,
        reply_ty: u32,
    },
    /// Perform through a first-class operation value: pop `argc`
    /// arguments over the operation value and push the reply.
    ///
    /// `reply_ty` carries the same rule as the field of `Perform`.
    PerformValue {
        argc: u32,
        reply_ty: u32,
    },
    /// Push the first-class value of the exact manifest operation.
    OpConst(u32),
    /// Edit a policy table: pop the table handle and, for a mock, the
    /// handler closure. `action`: 0 pass, 1 block, 2 mock, 3 clear.
    /// `kind`: 0 exact operation, 1 group. Push unit.
    TableEdit {
        action: u32,
        kind: u32,
        slot: u32,
    },
    /// Pop a Request and push `Option[PendingCall[...]]` for the
    /// exact operation.
    AsCall {
        op: u32,
        ty: u32,
    },
    /// Pop a PendingCall and push its boundary-copied argument view.
    CallArgs,
    /// Pop a Fault value and push its stable code as a string.
    FaultCode,
    /// Pop a Request and push the qualified name of its operation.
    ///
    /// The name comes from the pending record of the target machine,
    /// so the request must still be live.
    RequestOp,
    /// Pop a reason string and push one frozen `PolicyDenied` fault.
    ///
    /// This is the one fault a program can build. A holder needs it
    /// to deny a request through `reject`. The code is fixed, so no
    /// program can claim a machine-internal fault.
    FaultDenied,
    /// Pop a message and stop with `UserPanic`.
    RaiseUserPanic,
    /// Pop a message and stop with `AssertionFailed`.
    RaiseAssertionFailed,
    /// Pop a Fault and stop with its complete stored record.
    RaiseFault,
    /// The runtime backstop behind a proven-exhaustive `case`. It
    /// faults if executed. Ends the block.
    Unreachable,
    /// Structural equality for a sealed enum value: the same arm and
    /// equal fields. The walk keeps its own stack.
    EqValue,
    NeValue,
    /// Call one method through a verified nominal interface bound.
    CallInterface {
        /// The packed interface and method indices.
        site: u32,
        /// The static receiver type under the active environment.
        recv_ty: u32,
        /// The method-owned generic application, or `NO_APP`.
        app: u32,
    },
    /// Run one instruction from an added bytecode family.
    Extended(ExtendedInstr),
}

/// One numeric or bitwise instruction in the prefixed opcode family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NumericInstr {
    IntBitAnd,
    IntBitOr,
    IntBitXor,
    IntBitNot,
    IntShl,
    IntShr,
    IntUshr,
    IntWrappingAdd,
    IntWrappingSub,
    IntWrappingMul,
    IntRotateLeft,
    IntRotateRight,
    IntToFloat,
    FloatNeg,
    FloatAdd,
    FloatSub,
    FloatMul,
    FloatDiv,
    FloatEq,
    FloatNe,
    FloatLt,
    FloatLe,
    FloatGt,
    FloatGe,
    FloatIsNan,
    FloatHash,
    FloatBits,
    FloatFromBits,
    FloatToIntStatus,
    FloatToIntValue,
    SbAppendFloat,
    BytesBitAnd,
    BytesBitOr,
    BytesBitXor,
    BytesBitNot,
    TextParseFloatStatus,
    TextParseFloatValue,
    FloatFixed,
    IntCountOnes,
    IntLeadingZeros,
    IntTrailingZeros,
    IntSignum,
    FloatAbs,
    FloatMin,
    FloatMax,
    FloatSqrt,
    FloatFloor,
    FloatCeil,
    FloatRound,
    FloatTrunc,
    FloatIsFinite,
    FloatIsInfinite,
    FloatRem,
    FloatCopySign,
    FloatMulAdd,
    FloatPow,
    FloatExp,
    FloatExp2,
    FloatExpM1,
    FloatLn,
    FloatLog2,
    FloatLog10,
    FloatLn1P,
    FloatCbrt,
    FloatHypot,
    FloatSin,
    FloatCos,
    FloatTan,
    FloatAsin,
    FloatAcos,
    FloatAtan,
    FloatAtan2,
    FloatSinh,
    FloatCosh,
    FloatTanh,
    FloatAsinh,
    FloatAcosh,
    FloatAtanh,
    IntRotateLeft32,
    IntRotateRight32,
}

impl NumericInstr {
    fn from_tag(tag: u8) -> Option<NumericInstr> {
        Some(match tag {
            0 => NumericInstr::IntBitAnd,
            1 => NumericInstr::IntBitOr,
            2 => NumericInstr::IntBitXor,
            3 => NumericInstr::IntBitNot,
            4 => NumericInstr::IntShl,
            5 => NumericInstr::IntShr,
            6 => NumericInstr::IntUshr,
            7 => NumericInstr::IntWrappingAdd,
            8 => NumericInstr::IntWrappingSub,
            9 => NumericInstr::IntWrappingMul,
            10 => NumericInstr::IntRotateLeft,
            11 => NumericInstr::IntRotateRight,
            12 => NumericInstr::IntToFloat,
            13 => NumericInstr::FloatNeg,
            14 => NumericInstr::FloatAdd,
            15 => NumericInstr::FloatSub,
            16 => NumericInstr::FloatMul,
            17 => NumericInstr::FloatDiv,
            18 => NumericInstr::FloatEq,
            19 => NumericInstr::FloatNe,
            20 => NumericInstr::FloatLt,
            21 => NumericInstr::FloatLe,
            22 => NumericInstr::FloatGt,
            23 => NumericInstr::FloatGe,
            24 => NumericInstr::FloatIsNan,
            25 => NumericInstr::FloatHash,
            26 => NumericInstr::FloatBits,
            27 => NumericInstr::FloatFromBits,
            28 => NumericInstr::FloatToIntStatus,
            29 => NumericInstr::FloatToIntValue,
            30 => NumericInstr::SbAppendFloat,
            31 => NumericInstr::BytesBitAnd,
            32 => NumericInstr::BytesBitOr,
            33 => NumericInstr::BytesBitXor,
            34 => NumericInstr::BytesBitNot,
            35 => NumericInstr::TextParseFloatStatus,
            36 => NumericInstr::TextParseFloatValue,
            37 => NumericInstr::FloatFixed,
            38 => NumericInstr::IntCountOnes,
            39 => NumericInstr::IntLeadingZeros,
            40 => NumericInstr::IntTrailingZeros,
            41 => NumericInstr::IntSignum,
            42 => NumericInstr::FloatAbs,
            43 => NumericInstr::FloatMin,
            44 => NumericInstr::FloatMax,
            45 => NumericInstr::FloatSqrt,
            46 => NumericInstr::FloatFloor,
            47 => NumericInstr::FloatCeil,
            48 => NumericInstr::FloatRound,
            49 => NumericInstr::FloatTrunc,
            50 => NumericInstr::FloatIsFinite,
            51 => NumericInstr::FloatIsInfinite,
            52 => NumericInstr::FloatRem,
            53 => NumericInstr::FloatCopySign,
            54 => NumericInstr::FloatMulAdd,
            55 => NumericInstr::FloatPow,
            56 => NumericInstr::FloatExp,
            57 => NumericInstr::FloatExp2,
            58 => NumericInstr::FloatExpM1,
            59 => NumericInstr::FloatLn,
            60 => NumericInstr::FloatLog2,
            61 => NumericInstr::FloatLog10,
            62 => NumericInstr::FloatLn1P,
            63 => NumericInstr::FloatCbrt,
            64 => NumericInstr::FloatHypot,
            65 => NumericInstr::FloatSin,
            66 => NumericInstr::FloatCos,
            67 => NumericInstr::FloatTan,
            68 => NumericInstr::FloatAsin,
            69 => NumericInstr::FloatAcos,
            70 => NumericInstr::FloatAtan,
            71 => NumericInstr::FloatAtan2,
            72 => NumericInstr::FloatSinh,
            73 => NumericInstr::FloatCosh,
            74 => NumericInstr::FloatTanh,
            75 => NumericInstr::FloatAsinh,
            76 => NumericInstr::FloatAcosh,
            77 => NumericInstr::FloatAtanh,
            78 => NumericInstr::IntRotateLeft32,
            79 => NumericInstr::IntRotateRight32,
            _ => return None,
        })
    }
}

/// One instruction in the extended dispatch family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Use one-byte tags to keep extended instructions compact.
#[repr(u8)]
pub enum ExtendedInstr {
    /// Create one machine-local callback descriptor.
    MakeCallback { func: u32, captures: u32 },
    /// Treat one heap closure as a nonescaping callback.
    AsCallback,
    /// Reclassify one payload as a native `Some` value.
    OptionSome { ty: u32 },
    /// Push one native `None` value.
    OptionNone { ty: u32 },
    /// Pop a native `Some` value and push its direct payload.
    OptionPayload { ty: u32 },
    /// Pop an index and a list, then push `Option[element]`.
    ListGet { ty: u32 },
    /// Pop a key and a map, then push `Option[value]`.
    MapGet { ty: u32 },
    /// Pop a value, a Text key, and a Map[String, value].
    /// Insert an owned String only when the key is absent.
    MapPutText { ty: u32, discard: bool },
    /// Pop a byte range and a Map[String, String].
    /// Push the existing or newly allocated String.
    MapInternTextRange,
    /// Pop a list and push its structural epoch.
    ListEpoch,
    /// Pop an epoch and a list. Push the length after an epoch check.
    ListIterLen,
    /// Pop a map and push its structural epoch.
    MapEpoch,
    /// Pop an epoch and a map. Push the length after an epoch check.
    MapIterLen,
    /// Pop an epoch, cursor, and map. Push the next live raw index.
    MapNextIndex,
    /// Pop one frozen-class instance, seal it, and push it.
    SealInstance,
    /// Pop a position and a map. Push the key at that position.
    MapKeyAt,
    /// Pop a position and a map. Push the value at that position.
    MapValueAt,
    /// Pop a list and push its current capacity.
    ListCapacity,
    /// Pop a value, an index, and a list. Replace the indexed value.
    ListSet,
    /// Pop a list and push its final element when one exists.
    ListPop { ty: u32 },
    /// Pop a value, an index, and a list. Insert the value at the index.
    ListInsert,
    /// Pop an index and a list. Remove and push the indexed value.
    ListRemove,
    /// Pop an index and a list. Swap-remove and push the indexed value.
    ListSwapRemove,
    /// Pop two indices and a list. Swap the indexed values.
    ListSwap,
    /// Pop an additional capacity and a list. Reserve that capacity.
    ListReserve,
    /// Pop a target length and a list. Remove trailing values.
    ListTruncate,
    /// Pop a value and a list. Push whether the list contains it.
    ListContains,
    /// Pop a list, increment its structural epoch, and push unit.
    ListReorder,
    /// Pop a key and a map. Remove and push the old value when one exists.
    MapRemove { ty: u32 },
    /// Pop a map and remove every entry.
    MapClear,
    /// Pop an additional capacity and a map. Reserve that capacity.
    MapReserve,
    /// Call the current function target of one VM slot.
    CallSlot { slot: u32, app: u32 },
    /// Call the current construction target of one class slot.
    NewSlot { slot: u32, app: u32 },
    /// Load the current value target of one VM slot.
    LoadSlot { slot: u32 },
    /// Send through the current process target of one VM slot.
    SendSlot { slot: u32 },
    /// Pop a SyntaxTree and push its root SyntaxNode.
    SyntaxTreeRoot,
    /// Pop a syntax view and push its stable kind number.
    SyntaxKind,
    /// Pop a syntax view and push its category number.
    SyntaxCategory,
    /// Pop a syntax view and push its first source byte.
    SyntaxRangeStart,
    /// Pop a syntax view and push its final source byte.
    SyntaxRangeEnd,
    /// Pop a syntax view and push its shared source text.
    SyntaxText,
    /// Pop a syntax view and push its immediate child views.
    SyntaxChildren,
    /// Pop a syntax view and push one compact independent view.
    SyntaxDetach,
    /// Package one value with its closed static type.
    DynPack { ty: u32 },
    /// Pop one dynamic package and push its rendered text.
    DynRender,
    /// Build one immutable token from a kind and exact text.
    SyntaxBuildToken,
    /// Build one immutable trivia item from a kind and exact text.
    SyntaxBuildTrivia,
    /// Build one immutable node from a kind and child elements.
    SyntaxBuildNode,
    /// Convert one immutable syntax node into a syntax tree.
    SyntaxToTree,
    /// Push one portable view of a named function definition.
    FunctionCode { func: u32 },
    /// Push one portable view of a named class definition.
    ClassCode { class: u32 },
    /// Describe one exact source module surface.
    ModuleCode { module: u32 },
    /// Pop a module descriptor and push its source declarations.
    ReflectionDeclarations,
    /// Pop a declaration descriptor and push its effective methods.
    ReflectionMembers,
    /// Pop a reflection descriptor and push its source name.
    ReflectionName,
    /// Pop a declaration descriptor and push its declaration kind.
    ReflectionDeclarationKind,
    /// Pop a member descriptor and push its member kind.
    ReflectionMemberKind,
    /// Read optional source data from portable definition code.
    CodeSource { ty: u32 },
    /// Read the stable binding data from portable definition code.
    CodeDefinition,
    /// Pop a fault and push its primary optional source location.
    FaultSite { ty: u32 },
    /// Pop a fault and push its bounded source trace.
    FaultTrace { ty: u32 },
    /// Pop a prior token, semantic hash, and map. Push the next probe token.
    MapProbe,
    /// Pop a probe token and push whether it names one entry.
    MapProbeFound,
    /// Pop a probe token and map. Push the selected key.
    MapProbeKey,
    /// Pop a probe token and map. Push the selected value.
    MapProbeValue,
    /// Pop a value, probe token, and map. Replace the selected value.
    MapProbeSetValue,
    /// Pop a probe token and map. Remove and push the selected value.
    MapProbeRemove,
    /// Pop a token, hash, value, key, and map. Insert one new entry.
    MapInsertHashed,
    /// Pop a map, check its write capability, and push unit.
    MapWriteGuard,
    /// Prepare one exact host operation as a selectable wait source.
    PrepareWait { op_argc: u32, reply_ty: u32 },
    /// Search text and push an optional regular-expression match.
    RegexCaptures { ty: u32 },
    /// Load one optional numbered capture.
    RegexMatchGroup { ty: u32 },
    /// Load one optional named capture.
    RegexMatchNamed { ty: u32 },
}

const WAIT_FIELD_BITS: u32 = 16;
const WAIT_FIELD_MAX: u32 = (1 << WAIT_FIELD_BITS) - 1;

impl ExtendedInstr {
    /// Build one compact prepared wait instruction.
    pub fn prepare_wait(op: u32, argc: u32, reply_ty: u32) -> Option<ExtendedInstr> {
        if op > WAIT_FIELD_MAX || argc > WAIT_FIELD_MAX {
            return None;
        }
        Some(ExtendedInstr::PrepareWait {
            op_argc: op | (argc << WAIT_FIELD_BITS),
            reply_ty,
        })
    }

    /// Return the operation and argument count of one prepared wait.
    pub fn wait_parts(op_argc: u32) -> (u32, u32) {
        (op_argc & WAIT_FIELD_MAX, op_argc >> WAIT_FIELD_BITS)
    }
}

/// One native value instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeInstr {
    /// Text equality by Unicode scalar content.
    EqStr,
    NeStr,
    /// Pop Text and push its UTF-8 byte length.
    StrByteLen,
    /// Pop Text and push its Unicode scalar count.
    StrCharCount,
    /// Pop two Text values and push their String concatenation.
    StrConcat,
    /// Pop a prefix and a String, then test the prefix.
    StrStartsWith,
    /// Pop a suffix and a String, then test the suffix.
    StrEndsWith,
    /// Pop a needle and a String, then test for the needle.
    StrContains,
    /// Pop a needle and Text, then push its scalar index or -1.
    StrFindIndex,
    /// Pop a needle and Text, then push its byte index or -1.
    TextFindByteIndex,
    /// Pop a byte position and Text, then push one Char.
    TextAtByte,
    /// Pop a Text, then push the Substring without outer whitespace.
    TextTrim,
    /// Pop a Text, then push the Substring without leading whitespace.
    TextTrimStart,
    /// Pop a Text, then push the Substring without trailing whitespace.
    TextTrimEnd,
    /// Pop a Text, then push new text with ASCII letters in lower case.
    TextToLowerAscii,
    /// Pop a Text, then push new text with ASCII letters in upper case.
    TextToUpperAscii,
    /// Pop a replacement, a needle, and a Text, then push new text.
    TextReplace,
    /// Pop a radix and Text, then push the parse status code.
    TextParseIntStatus,
    /// Pop a radix and Text, then push the parsed integer.
    TextParseIntValue,
    /// Pop a width and Text, then push left-padded String text.
    TextPadStart,
    /// Pop a width and Text, then push right-padded String text.
    TextPadEnd,
    /// Pop a suffix and Bytes, then push whether it ends with it.
    BytesEndsWith,
    /// Pop a needle and Bytes, then push whether it contains it.
    BytesContains,
    /// Pop a separator and Text, then push a List of Substring pieces.
    TextSplit,
    /// Pop a Text, then push a List of its Substring lines.
    TextLines,
    /// Pop a scalar index and Text, then push one Char.
    TextAt,
    /// Pop a scalar range and Text, then push one shared Substring.
    TextSlice,
    /// Pop a byte position and Text, then test its UTF-8 boundary.
    TextIsBoundary,
    /// Pop a byte range and Text, then push one shared Substring.
    TextSliceBytes,
    /// Pop Text and push shared immutable bytes.
    TextBytes,
    TextLt,
    TextLe,
    TextGt,
    TextGe,
    /// Pop Text and push a bounded String.
    TextToString,
    /// Pop a Char and push its scalar code point.
    CharCodepoint,
    /// Pop a Char and push its UTF-8 byte length.
    CharUtf8Len,
    EqChar,
    NeChar,
    LtChar,
    LeChar,
    GtChar,
    GeChar,
    /// Allocate an empty string builder.
    SbNew,
    /// Pop a string and a builder, append, and push the builder.
    SbAppendStr,
    /// Pop an Int and a builder, append its decimal text, push the builder.
    SbAppendInt,
    /// Pop a Bool and a builder, append its text, push the builder.
    SbAppendBool,
    /// Pop a builder and push its content as a new string.
    SbBuild,
    /// Pop a Char and builder, append, and push the builder.
    SbAppendChar,
    /// Pop a builder and push its UTF-8 byte length.
    SbByteLen,
    /// Move a builder into String storage and invalidate it.
    SbFinish,
    /// Allocate an empty byte buffer.
    BbNew,
    /// Pop an Int and a buffer, append one byte, push the buffer.
    BbAppend,
    /// Pop a buffer and push its byte length.
    BbLen,
    /// Pop a buffer and push its content as immutable bytes.
    BbBuild,
    /// Move a buffer into Bytes storage and invalidate it.
    BbFinish,
    /// Pop a string and push its immutable UTF-8 bytes.
    BytesNew,
    /// Pop immutable bytes and push their length.
    BytesLen,
    /// Pop immutable bytes, decode UTF-8, and push a string.
    BytesText,
    /// Pop a length, start, and bytes, then push bounded UTF-8 text.
    BytesTextRange,
    /// Pop a builder and push its UTF-8 byte length.
    SbLen,
    /// Pop a builder, clear it, and push the builder.
    SbClear,
    /// Pop bytes and a buffer, extend the buffer, and push the buffer.
    BbExtend,
    /// Pop an Int and a buffer, reserve capacity, and push the buffer.
    BbReserve,
    /// Pop a buffer, clear it, and push the buffer.
    BbClear,
    /// Pop an index and a buffer, then push one byte or -1.
    BbAt,
    /// Pop a byte, an index, and a buffer. Replace the indexed byte.
    BbSet,
    /// Pop a buffer and push its current capacity.
    BbCapacity,
    /// Pop a length and a buffer. Remove trailing bytes.
    BbTruncate,
    /// Pop a start, needle, and buffer, then push an index or -1.
    BbFindFrom,
    /// Pop an index and bytes, then push the byte as an Int.
    BytesAt,
    /// Pop an index and bytes, then push the byte or -1.
    BytesGet,
    /// Pop an offset and bytes, then read one big-endian 32-bit word.
    BytesReadU32Be,
    /// Pop an offset and bytes, then read one little-endian 32-bit word.
    BytesReadU32Le,
    /// Pop a length, start, and bytes, then push a shared slice.
    BytesSlice,
    /// Pop two byte values, concatenate them, and push new bytes.
    BytesConcat,
    /// Pop a prefix and bytes, then test the prefix.
    BytesStartsWith,
    /// Pop a needle and bytes, then push its index or -1.
    BytesFindIndex,
    /// Pop bytes and push lowercase hexadecimal text.
    BytesHex,
    /// Pop bytes and push whether they contain valid UTF-8.
    BytesIsUtf8,
    /// Bytes equality by content.
    EqBytes,
    NeBytes,
    LtBytes,
    LeBytes,
    GtBytes,
    GeBytes,
    /// Pop bytes and push an exact copied span.
    BytesCompact,
    /// Pop valid UTF-8 bytes and push one shared Substring.
    BytesTextView,
    /// Pop Text and push its stable semantic hash.
    TextHash,
    /// Pop Bytes and push its stable semantic hash.
    BytesHash,
    /// Pop two integers and push their ordered hash mix.
    HashCombine,
    /// Pop two integers and push their order-independent hash mix.
    HashUnorderedCombine,
    /// Check one dynamic regular-expression pattern.
    RegexCompileStatus,
    /// Compile one previously checked regular-expression pattern.
    RegexCompileValue,
    /// Pop a regular expression and push its source text.
    RegexSource,
    /// Test whether a regular expression matches text.
    RegexIsMatch,
    /// Count non-overlapping regular-expression matches.
    RegexCount,
    /// Split text at regular-expression matches.
    RegexSplit,
    /// Replace regular-expression matches with expanded text.
    RegexReplaceAll,
    /// Load the absolute start byte of a match.
    RegexMatchStart,
    /// Load the absolute end byte of a match.
    RegexMatchEnd,
    /// Copy the complete matched text.
    RegexMatchText,
    /// Load the capture count, including the complete match.
    RegexMatchGroupCount,
}

impl Instr {
    /// Return true when the instruction ends a basic block.
    pub fn is_terminator(&self) -> bool {
        matches!(
            self,
            Instr::Jump(_)
                | Instr::Return
                | Instr::RaiseUserPanic
                | Instr::RaiseAssertionFailed
                | Instr::RaiseFault
                | Instr::Unreachable
        )
    }
}

/// One function body as basic blocks of decoded instructions.
#[derive(Debug, Clone, PartialEq)]
pub struct Func {
    pub name: String,
    /// The number of generic type parameters. Type entries `Var(i)`
    /// with `i` below this count may appear in the body and signature.
    pub type_params: u32,
    /// The number of effect parameters available to row `Var` elements.
    pub effect_params: u32,
    /// Parameter types as type-table indices.
    pub params: Vec<u32>,
    /// Parameter `mut` markers, aligned with `params`.
    pub param_muts: Vec<bool>,
    /// Result type as a type-table index.
    pub ret: u32,
    /// The declared effect row in canonical order.
    pub row: Vec<BcRow>,
    /// Capture types as type-table indices. Only a closure body has
    /// captures. A direct or virtual call target must have none.
    pub captures: Vec<u32>,
    /// The declared type of every local slot, as type-table indices.
    /// Parameters use the first slots, so the prefix must equal
    /// `params`. The verifier validates the table; store and load
    /// checks use the declared slot type.
    pub local_types: Vec<u32>,
    pub blocks: Vec<Vec<Instr>>,
    /// Declared parameter names, aligned with `params` when present.
    /// This compiler surface data stays after the execution fields.
    pub param_names: Vec<String>,
}

impl Func {
    /// The total local slot count of the function.
    pub fn local_count(&self) -> u32 {
        self.local_types.len() as u32
    }
}

/// The kind of one import slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    /// A class or enum declaration. `def` is a class index.
    Class,
    /// The construction function of an imported class. `def` is a
    /// function index.
    Ctor,
    /// One method of an imported class. `def` is a function index.
    Method,
    /// A top-level function. `def` is a function index.
    Func,
    /// A compile-time constant pin. `def` is `NO_IMPORT_DEF`.
    Constant,
}

impl ImportKind {
    fn tag(self) -> u8 {
        match self {
            ImportKind::Class => 0,
            ImportKind::Ctor => 1,
            ImportKind::Method => 2,
            ImportKind::Func => 3,
            ImportKind::Constant => 4,
        }
    }

    /// True when the slot declares a function, not a class.
    pub fn is_func(self) -> bool {
        matches!(
            self,
            ImportKind::Ctor | ImportKind::Method | ImportKind::Func
        )
    }
}

/// One named import slot.
///
/// A slot pins an export that another module provides. A definition
/// slot names one sparse local declaration. A constant slot has no
/// runtime declaration because the compiler inlines its value. The
/// linker rejects a provider whose interface hash differs from the
/// pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// The providing module path, for example `mathlib.matrix`.
    pub module: String,
    /// The exported name, for example `Matrix` or `Matrix.scale`.
    pub name: String,
    pub kind: ImportKind,
    /// The local definition index, or `NO_IMPORT_DEF` for a constant.
    pub def: u32,
    /// The pinned interface hash of the provider export.
    pub hash: [u8; 32],
}

/// The kind of one export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    Function,
    Class,
    Enum,
    EnumCase,
    Interface,
    /// A compile-time value stored only in a module interface.
    Constant,
}

impl ExportKind {
    pub fn tag(self) -> u8 {
        match self {
            ExportKind::Function => 0,
            ExportKind::Class => 1,
            ExportKind::Enum => 2,
            ExportKind::EnumCase => 3,
            ExportKind::Interface => 4,
            ExportKind::Constant => 5,
        }
    }

    pub fn from_tag(tag: u8) -> Option<ExportKind> {
        match tag {
            0 => Some(ExportKind::Function),
            1 => Some(ExportKind::Class),
            2 => Some(ExportKind::Enum),
            3 => Some(ExportKind::EnumCase),
            4 => Some(ExportKind::Interface),
            5 => Some(ExportKind::Constant),
            _ => None,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            ExportKind::Function => "fn",
            ExportKind::Class => "class",
            ExportKind::Enum => "enum",
            ExportKind::EnumCase => "case",
            ExportKind::Interface => "interface",
            ExportKind::Constant => "const",
        }
    }

    /// True when the export names a class-like definition.
    pub fn is_class(self) -> bool {
        matches!(
            self,
            ExportKind::Class | ExportKind::Enum | ExportKind::EnumCase
        )
    }

    /// True when the export names an interface contract.
    pub fn is_interface(self) -> bool {
        matches!(self, ExportKind::Interface)
    }

    /// True when the export is a compile-time constant.
    pub fn is_constant(self) -> bool {
        matches!(self, ExportKind::Constant)
    }
}

/// One named function binding.
///
/// A name is a temporary reference to an identity, never a part of
/// it. A binding maps a qualified name to a function value, and the
/// function value carries its own structural hash. Several bindings
/// may name one function value: two modules with equal bodies share
/// one code object and keep two bindings.
///
/// The binding table lives in the export section, so a binding key
/// never enters the semantic region and never enters a structural
/// hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncBinding {
    /// The fully qualified binding name. A free function takes
    /// `<module path>.<name>`. A method or an `init` takes
    /// `<class key>.<name>`. A generated constructor takes
    /// `<class key>.<new>`.
    pub key: String,
    /// The function value this name points at.
    pub func: u32,
    /// The class this binding constructs, or `NO_CLASS`.
    ///
    /// A key alone does not identify a constructor. This field ties
    /// the binding to its class. The verifier proves the relation.
    pub class: u32,
}

/// The sentinel for a binding that constructs no class.
pub const NO_CLASS: u32 = u32::MAX;

/// The name segment of a generated construction function.
pub const CTOR_SEGMENT: &str = "<new>";

/// The binding key of the construction function of one class.
pub fn ctor_binding_key(class_key: &str) -> String {
    format!("{class_key}.{CTOR_SEGMENT}")
}

/// The sentinel for an export without a construction function.
pub const NO_CTOR: u32 = u32::MAX;

/// The sentinel for an import without a runtime definition.
pub const NO_IMPORT_DEF: u32 = u32::MAX;

/// One recursively literal compile-time value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstValue {
    Unit,
    Bool(bool),
    Int(i64),
    Float(u64),
    Char(char),
    String(String),
    Bytes(Vec<u8>),
    Tuple(Vec<ConstValue>),
}

/// One typed compile-time constant in a module export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constant {
    /// The declared module type index.
    pub ty: u32,
    pub value: ConstValue,
}

/// One exported top-level definition of the source module.
///
/// The table names definitions and constants that another module can import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub kind: ExportKind,
    pub name: String,
    /// True when this entry describes one top-level source declaration.
    pub source: bool,
    /// The class index or the function index.
    pub def: u32,
    /// The construction function index of a class export, or
    /// `NO_CTOR`.
    pub ctor: u32,
    /// The compile-time value of a constant export.
    pub constant: Option<Constant>,
}

/// One source declaration in a reified module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectionDeclaration {
    pub kind: ExportKind,
    pub name: String,
    /// The function, class, or interface index. A constant uses
    /// `NO_REFLECTION_DEF`.
    pub def: u32,
    /// The callable function. Classes use their constructor.
    pub callable: u32,
}

/// One exact module surface available to runtime reflection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectionModule {
    pub name: String,
    pub declarations: Vec<ReflectionDeclaration>,
}

/// A reflection constant has no runtime definition.
pub const NO_REFLECTION_DEF: u32 = u32::MAX;

/// One decoded module.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub strings: Vec<String>,
    /// Immutable raw byte literals.
    pub bytes: Vec<Vec<u8>>,
    pub types: Vec<BcType>,
    /// Global selector names in first-encounter order.
    pub selectors: Vec<String>,
    /// Type applications referenced by generic call and allocation
    /// sites.
    pub apps: Vec<TypeApp>,
    /// Nominal interface contracts in dense declaration order.
    pub interfaces: Vec<BcInterface>,
    /// Explicit class conformances.
    pub conformances: Vec<BcConformance>,
    /// Interface bounds aligned with the class table.
    pub class_bounds: Vec<Vec<Vec<BcInterfaceUse>>>,
    /// Interface bounds aligned with the function table.
    pub func_bounds: Vec<Vec<Vec<BcInterfaceUse>>>,
    /// The import slots, in declaration order. An empty table marks a
    /// linked module, which is the only kind the loader admits.
    pub imports: Vec<Import>,
    /// Portable late-binding contracts in dense module order.
    pub slots: Vec<SlotSpec>,
    /// The stable core role slots: one class index per role, or
    /// `NO_ROLE`. The compiler fills the table, the linker relocates
    /// it, and the verifier proves the shape of every filled slot.
    /// The verifier and the VM then read slots, never a source name
    /// and never a definition hash.
    pub core_roles: [u32; CORE_ROLE_COUNT],
    /// Exact source module surfaces used by `codeof` expressions.
    pub reflections: Vec<ReflectionModule>,
    pub classes: Vec<BcClass>,
    pub funcs: Vec<Func>,
    /// Index of the entry function.
    pub entry: u32,
    /// The exported top-level definitions. The export section holds
    /// this table, so it stays outside the semantic region.
    pub exports: Vec<Export>,
    /// The named function bindings. Each entry maps a qualified name
    /// to a function value. The export section holds this table. A
    /// published slot can contain a hash derived from the binding key.
    pub bindings: Vec<FuncBinding>,
    /// Optional source and diagnostic metadata.
    ///
    /// This content stays outside the semantic and verification regions.
    pub debug: Vec<u8>,
}

/// One append-only table in a published code revision.
///
/// A publication adds one immutable chunk. Older revisions keep
/// their chunk lists and never copy existing entries.
pub struct CodeTable<T> {
    first: std::sync::Arc<Vec<T>>,
    later: std::sync::Arc<Vec<CodeChunk<T>>>,
    len: usize,
}

struct CodeChunk<T> {
    start: usize,
    values: std::sync::Arc<Vec<T>>,
}

impl<T> Clone for CodeChunk<T> {
    fn clone(&self) -> CodeChunk<T> {
        CodeChunk {
            start: self.start,
            values: self.values.clone(),
        }
    }
}

impl<T> Clone for CodeTable<T> {
    fn clone(&self) -> CodeTable<T> {
        CodeTable {
            first: self.first.clone(),
            later: self.later.clone(),
            len: self.len,
        }
    }
}

impl<T> Default for CodeTable<T> {
    fn default() -> CodeTable<T> {
        CodeTable {
            first: std::sync::Arc::new(Vec::new()),
            later: std::sync::Arc::new(Vec::new()),
            len: 0,
        }
    }
}

impl<T> CodeTable<T> {
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline(always)]
    pub fn get(&self, index: usize) -> Option<&T> {
        if self.later.is_empty() {
            return self.first.get(index);
        }
        if index < self.first.len() {
            return self.first.get(index);
        }
        if index >= self.len {
            return None;
        }
        let chunks = self.later.as_slice();
        if let Some(last) = chunks.last() {
            if index >= last.start {
                return last.values.get(index - last.start);
            }
        }
        let chunk = chunks
            .binary_search_by(|chunk| {
                if index < chunk.start {
                    std::cmp::Ordering::Greater
                } else if index >= chunk.start + chunk.values.len() {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .ok()?;
        let chunk = &chunks[chunk];
        chunk.values.get(index - chunk.start)
    }

    pub fn iter(&self) -> CodeTableIter<'_, T> {
        CodeTableIter {
            first: self.first.iter(),
            chunks: self.later.iter(),
            values: None,
        }
    }

    pub fn push(&mut self, value: T) {
        if self.later.is_empty()
            && (self.first.is_empty() || std::sync::Arc::strong_count(&self.first) == 1)
        {
            if self.first.is_empty() {
                self.first = std::sync::Arc::new(vec![value]);
            } else {
                std::sync::Arc::get_mut(&mut self.first)
                    .expect("an extendable first code chunk is unique")
                    .push(value);
            }
            self.len += 1;
            return;
        }
        let chunks = std::sync::Arc::make_mut(&mut self.later);
        let can_extend = chunks
            .last()
            .is_some_and(|chunk| std::sync::Arc::strong_count(&chunk.values) == 1);
        if can_extend {
            let chunk = chunks.last_mut().expect("the last chunk exists");
            std::sync::Arc::get_mut(&mut chunk.values)
                .expect("an extendable code chunk is unique")
                .push(value);
        } else {
            chunks.push(CodeChunk {
                start: self.len,
                values: std::sync::Arc::new(vec![value]),
            });
        }
        self.len += 1;
    }

    pub fn replace_recent(&mut self, index: usize, value: T) -> Result<(), T> {
        if index < self.first.len() {
            let Some(target) =
                std::sync::Arc::get_mut(&mut self.first).and_then(|values| values.get_mut(index))
            else {
                return Err(value);
            };
            *target = value;
            return Ok(());
        }
        let chunks = std::sync::Arc::make_mut(&mut self.later);
        let Some(chunk) = chunks.last_mut() else {
            return Err(value);
        };
        if index < chunk.start || std::sync::Arc::strong_count(&chunk.values) != 1 {
            return Err(value);
        }
        let Some(target) = std::sync::Arc::get_mut(&mut chunk.values)
            .and_then(|values| values.get_mut(index - chunk.start))
        else {
            return Err(value);
        };
        *target = value;
        Ok(())
    }

    pub fn contains(&self, value: &T) -> bool
    where
        T: PartialEq,
    {
        self.iter().any(|item| item == value)
    }

    pub fn to_vec(&self) -> Vec<T>
    where
        T: Clone,
    {
        self.iter().cloned().collect()
    }

    pub fn chunk_count(&self) -> usize {
        usize::from(!self.first.is_empty()) + self.later.len()
    }
}

impl<T> From<Vec<T>> for CodeTable<T> {
    fn from(values: Vec<T>) -> CodeTable<T> {
        let len = values.len();
        CodeTable {
            first: std::sync::Arc::new(values),
            later: std::sync::Arc::new(Vec::new()),
            len,
        }
    }
}

impl<T> std::ops::Index<usize> for CodeTable<T> {
    type Output = T;

    #[inline(always)]
    fn index(&self, index: usize) -> &T {
        self.get(index).expect("the code-table index is in range")
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for CodeTable<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entries(self.iter()).finish()
    }
}

impl<T: PartialEq> PartialEq for CodeTable<T> {
    fn eq(&self, other: &CodeTable<T>) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl<T: Eq> Eq for CodeTable<T> {}

pub struct CodeTableIter<'a, T> {
    first: std::slice::Iter<'a, T>,
    chunks: std::slice::Iter<'a, CodeChunk<T>>,
    values: Option<std::slice::Iter<'a, T>>,
}

impl<'a, T> Iterator for CodeTableIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        if let Some(value) = self.first.next() {
            return Some(value);
        }
        loop {
            if let Some(value) = self.values.as_mut().and_then(Iterator::next) {
                return Some(value);
            }
            self.values = Some(self.chunks.next()?.values.iter());
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let current = self.first.len() + self.values.as_ref().map_or(0, ExactSizeIterator::len);
        let later: usize = self
            .chunks
            .as_slice()
            .iter()
            .map(|chunk| chunk.values.len())
            .sum();
        let remaining = current + later;
        (remaining, Some(remaining))
    }
}

impl<T> ExactSizeIterator for CodeTableIter<'_, T> {}

impl<'a, T> IntoIterator for &'a CodeTable<T> {
    type Item = &'a T;
    type IntoIter = CodeTableIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// A table view from one module or one runtime revision.
pub enum CodeTableRef<'a, T> {
    Slice(&'a [T]),
    Chunks(&'a CodeTable<T>),
}

impl<'a, T> CodeTableRef<'a, T> {
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            CodeTableRef::Slice(values) => values.len(),
            CodeTableRef::Chunks(values) => values.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline(always)]
    pub fn get(&self, index: usize) -> Option<&'a T> {
        match self {
            CodeTableRef::Slice(values) => values.get(index),
            CodeTableRef::Chunks(values) => values.get(index),
        }
    }

    #[inline]
    pub fn iter(&self) -> CodeTableRefIter<'a, T> {
        match self {
            CodeTableRef::Slice(values) => CodeTableRefIter::Slice(values.iter()),
            CodeTableRef::Chunks(values) => CodeTableRefIter::Chunks(values.iter()),
        }
    }
}

pub enum CodeTableRefIter<'a, T> {
    Slice(std::slice::Iter<'a, T>),
    Chunks(CodeTableIter<'a, T>),
}

impl<'a, T> Iterator for CodeTableRefIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        match self {
            CodeTableRefIter::Slice(values) => values.next(),
            CodeTableRefIter::Chunks(values) => values.next(),
        }
    }
}

/// Dense tables used by one published code namespace.
///
/// A `Module` is one unresolved `LinkUnit` payload. These tables hold
/// relocated definitions. They contain no import or export records.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CodeTables {
    pub strings: CodeTable<String>,
    pub bytes: CodeTable<Vec<u8>>,
    pub types: CodeTable<BcType>,
    pub selectors: CodeTable<String>,
    pub apps: CodeTable<TypeApp>,
    pub interfaces: CodeTable<BcInterface>,
    pub conformances: CodeTable<BcConformance>,
    pub class_bounds: CodeTable<Vec<Vec<BcInterfaceUse>>>,
    pub func_bounds: CodeTable<Vec<Vec<BcInterfaceUse>>>,
    pub slots: CodeTable<SlotSpec>,
    pub reflections: CodeTable<ReflectionModule>,
    pub classes: CodeTable<BcClass>,
    pub funcs: CodeTable<Func>,
    pub debug: Vec<u8>,
}

/// Read-only access to relocated code tables.
///
/// A compiler `Module` and a runtime `CodeTables` value both provide
/// this view. Runtime code never needs module linkage records.
pub trait CodeTableView {
    fn strings(&self) -> CodeTableRef<'_, String>;
    fn types(&self) -> CodeTableRef<'_, BcType>;
    fn apps(&self) -> CodeTableRef<'_, TypeApp>;
    fn classes(&self) -> CodeTableRef<'_, BcClass>;
    fn interfaces(&self) -> CodeTableRef<'_, BcInterface>;
    fn conformances(&self) -> CodeTableRef<'_, BcConformance>;
    fn slots(&self) -> CodeTableRef<'_, SlotSpec>;
    fn reflections(&self) -> CodeTableRef<'_, ReflectionModule>;
    fn funcs(&self) -> CodeTableRef<'_, Func>;

    fn core_role(&self, _index: usize) -> Option<u32> {
        None
    }
}

impl CodeTableView for Module {
    fn strings(&self) -> CodeTableRef<'_, String> {
        CodeTableRef::Slice(&self.strings)
    }

    fn types(&self) -> CodeTableRef<'_, BcType> {
        CodeTableRef::Slice(&self.types)
    }

    fn apps(&self) -> CodeTableRef<'_, TypeApp> {
        CodeTableRef::Slice(&self.apps)
    }

    fn classes(&self) -> CodeTableRef<'_, BcClass> {
        CodeTableRef::Slice(&self.classes)
    }

    fn interfaces(&self) -> CodeTableRef<'_, BcInterface> {
        CodeTableRef::Slice(&self.interfaces)
    }

    fn conformances(&self) -> CodeTableRef<'_, BcConformance> {
        CodeTableRef::Slice(&self.conformances)
    }

    fn slots(&self) -> CodeTableRef<'_, SlotSpec> {
        CodeTableRef::Slice(&self.slots)
    }

    fn reflections(&self) -> CodeTableRef<'_, ReflectionModule> {
        CodeTableRef::Slice(&self.reflections)
    }

    fn funcs(&self) -> CodeTableRef<'_, Func> {
        CodeTableRef::Slice(&self.funcs)
    }

    fn core_role(&self, index: usize) -> Option<u32> {
        self.core_roles
            .get(index)
            .copied()
            .filter(|class| *class != NO_ROLE)
    }
}

impl CodeTableView for CodeTables {
    fn strings(&self) -> CodeTableRef<'_, String> {
        CodeTableRef::Chunks(&self.strings)
    }

    fn types(&self) -> CodeTableRef<'_, BcType> {
        CodeTableRef::Chunks(&self.types)
    }

    fn apps(&self) -> CodeTableRef<'_, TypeApp> {
        CodeTableRef::Chunks(&self.apps)
    }

    fn classes(&self) -> CodeTableRef<'_, BcClass> {
        CodeTableRef::Chunks(&self.classes)
    }

    fn interfaces(&self) -> CodeTableRef<'_, BcInterface> {
        CodeTableRef::Chunks(&self.interfaces)
    }

    fn conformances(&self) -> CodeTableRef<'_, BcConformance> {
        CodeTableRef::Chunks(&self.conformances)
    }

    fn slots(&self) -> CodeTableRef<'_, SlotSpec> {
        CodeTableRef::Chunks(&self.slots)
    }

    fn reflections(&self) -> CodeTableRef<'_, ReflectionModule> {
        CodeTableRef::Chunks(&self.reflections)
    }

    fn funcs(&self) -> CodeTableRef<'_, Func> {
        CodeTableRef::Chunks(&self.funcs)
    }
}

impl<T: CodeTableView + ?Sized> CodeTableView for std::sync::Arc<T> {
    fn strings(&self) -> CodeTableRef<'_, String> {
        (**self).strings()
    }

    fn types(&self) -> CodeTableRef<'_, BcType> {
        (**self).types()
    }

    fn apps(&self) -> CodeTableRef<'_, TypeApp> {
        (**self).apps()
    }

    fn classes(&self) -> CodeTableRef<'_, BcClass> {
        (**self).classes()
    }

    fn interfaces(&self) -> CodeTableRef<'_, BcInterface> {
        (**self).interfaces()
    }

    fn conformances(&self) -> CodeTableRef<'_, BcConformance> {
        (**self).conformances()
    }

    fn slots(&self) -> CodeTableRef<'_, SlotSpec> {
        (**self).slots()
    }

    fn reflections(&self) -> CodeTableRef<'_, ReflectionModule> {
        (**self).reflections()
    }

    fn funcs(&self) -> CodeTableRef<'_, Func> {
        (**self).funcs()
    }

    fn core_role(&self, index: usize) -> Option<u32> {
        (**self).core_role(index)
    }
}

impl Module {
    /// The import slot of one class, when the class is imported.
    pub fn class_import(&self, class: u32) -> Option<&Import> {
        self.imports
            .iter()
            .find(|i| i.kind == ImportKind::Class && i.def == class)
    }

    /// The import slot of one function, when the function is imported.
    pub fn func_import(&self, func: u32) -> Option<&Import> {
        self.imports
            .iter()
            .find(|i| i.kind.is_func() && i.def == func)
    }

    /// The imported class flags, one per class.
    pub fn extern_classes(&self) -> Vec<bool> {
        let mut out = vec![false; self.classes.len()];
        for import in &self.imports {
            if import.kind == ImportKind::Class && (import.def as usize) < out.len() {
                out[import.def as usize] = true;
            }
        }
        out
    }

    /// The imported function flags, one per function.
    pub fn extern_funcs(&self) -> Vec<bool> {
        let mut out = vec![false; self.funcs.len()];
        for import in &self.imports {
            if import.kind.is_func() && (import.def as usize) < out.len() {
                out[import.def as usize] = true;
            }
        }
        out
    }
}

const MAGIC: &[u8; 4] = b"LMBC";

/// The container format version.
///
/// The format uses append-only tags. Existing tags keep their encoded
/// values when the format gains a new item.
pub const VERSION: u16 = 72;

/// The byte length of the container header: the magic, the version,
/// the ABI bundle digest, and three section-table entries.
const HEADER_LEN: usize = 4 + 2 + 32 + 3 * 8;
#[cfg(test)]
const SECTION_TABLE_AT: usize = 4 + 2 + 32;

// Opcode bytes for the serialized form.
const OP_CONST_UNIT: u8 = 0x00;
const OP_CONST_BOOL: u8 = 0x01;
const OP_CONST_INT: u8 = 0x02;
const OP_CONST_STR: u8 = 0x03;
const OP_LOAD_LOCAL: u8 = 0x04;
const OP_STORE_LOCAL: u8 = 0x05;
const OP_POP: u8 = 0x06;
const OP_RAISE_FAULT: u8 = 0x07;
const OP_CONST_CHAR: u8 = 0x08;
const OP_ADD: u8 = 0x10;
const OP_SUB: u8 = 0x11;
const OP_MUL: u8 = 0x12;
const OP_DIV: u8 = 0x13;
const OP_REM: u8 = 0x14;
const OP_NEG: u8 = 0x15;
const OP_NOT: u8 = 0x16;
const OP_LT_INT: u8 = 0x20;
const OP_LE_INT: u8 = 0x21;
const OP_GT_INT: u8 = 0x22;
const OP_GE_INT: u8 = 0x23;
const OP_EQ_INT: u8 = 0x24;
const OP_NE_INT: u8 = 0x25;
const OP_EQ_BOOL: u8 = 0x26;
const OP_NE_BOOL: u8 = 0x27;
const OP_EQ_STR: u8 = 0x28;
const OP_NE_STR: u8 = 0x29;
const OP_EQ_REF: u8 = 0x2a;
const OP_NE_REF: u8 = 0x2b;
const OP_EQ_VALUE: u8 = 0xb6;
const OP_NE_VALUE: u8 = 0xb7;
const OP_CALL: u8 = 0x30;
const OP_JUMP: u8 = 0x31;
const OP_JUMP_IF_FALSE: u8 = 0x32;
const OP_JUMP_IF_TRUE: u8 = 0x33;
const OP_RETURN: u8 = 0x34;
const OP_CALL_VIRTUAL: u8 = 0x40;
const OP_CALL_VALUE: u8 = 0x41;
const OP_MAKE_CLOSURE: u8 = 0x42;
const OP_LOAD_CAPTURE: u8 = 0x43;
const OP_NEW: u8 = 0x44;
const OP_LOAD_FIELD: u8 = 0x45;
const OP_STORE_FIELD: u8 = 0x46;
const OP_LIST_NEW: u8 = 0x47;
const OP_LIST_LEN: u8 = 0x48;
const OP_LIST_AT: u8 = 0x49;
const OP_LIST_PUSH: u8 = 0x4a;
const OP_MAP_NEW: u8 = 0x4b;
const OP_MAP_LEN: u8 = 0x4c;
const OP_MAP_HAS: u8 = 0x4d;
const OP_MAP_AT: u8 = 0x4e;
const OP_MAP_PUT: u8 = 0x4f;
const OP_SB_NEW: u8 = 0x50;
const OP_SB_APPEND_STR: u8 = 0x51;
const OP_SB_APPEND_INT: u8 = 0x52;
const OP_SB_APPEND_BOOL: u8 = 0x53;
const OP_SB_BUILD: u8 = 0x54;
const OP_BB_NEW: u8 = 0x55;
const OP_BB_APPEND: u8 = 0x56;
const OP_BB_LEN: u8 = 0x57;
const OP_BB_BUILD: u8 = 0x58;
const OP_FREEZE: u8 = 0x59;
const OP_BYTES_NEW: u8 = 0x5a;
const OP_BYTES_LEN: u8 = 0x5b;
const OP_BYTES_TEXT: u8 = 0x5c;
const OP_CALL_G: u8 = 0x60;
const OP_CALL_VIRTUAL_G: u8 = 0x61;
const OP_NEW_G: u8 = 0x62;
const OP_TUPLE_NEW: u8 = 0x63;
const OP_TUPLE_GET: u8 = 0x64;
const OP_IS_TYPE: u8 = 0x65;
const OP_CAST_TYPE: u8 = 0x66;
const OP_STR_BYTE_LEN: u8 = 0x67;
const OP_STR_CHAR_COUNT: u8 = 0x68;
const OP_STR_CONCAT: u8 = 0x69;
const OP_STR_STARTS_WITH: u8 = 0x6a;
const OP_STR_ENDS_WITH: u8 = 0x6b;
const OP_STR_CONTAINS: u8 = 0x6c;
const OP_STR_FIND_INDEX: u8 = 0x6d;
const OP_BYTES_AT: u8 = 0x6e;
const OP_BYTES_GET: u8 = 0x6f;
const OP_PERFORM: u8 = 0x70;
const OP_PERFORM_VALUE: u8 = 0x71;
const OP_OP_CONST: u8 = 0x72;
const OP_TABLE_EDIT: u8 = 0x73;
const OP_AS_CALL: u8 = 0x74;
const OP_CALL_ARGS: u8 = 0x75;
const OP_FAULT_CODE: u8 = 0x76;
const OP_UNREACHABLE: u8 = 0x77;
const OP_DIGEST: u8 = 0x78;
const OP_EQ_DIGEST: u8 = 0x79;
const OP_NE_DIGEST: u8 = 0x7a;
const OP_BYTES_SLICE: u8 = 0x7b;
const OP_BYTES_CONCAT: u8 = 0x7c;
const OP_BYTES_STARTS_WITH: u8 = 0x7d;
const OP_BYTES_FIND_INDEX: u8 = 0x7e;
const OP_BYTES_HEX: u8 = 0x7f;
const OP_BYTES_IS_UTF8: u8 = 0x80;
const OP_EQ_BYTES: u8 = 0x81;
const OP_NE_BYTES: u8 = 0x82;
const OP_SB_LEN: u8 = 0x83;
const OP_SB_CLEAR: u8 = 0x84;
const OP_BB_EXTEND: u8 = 0x85;
const OP_BB_RESERVE: u8 = 0x86;
const OP_BB_CLEAR: u8 = 0x87;
const OP_FAULT_DENIED: u8 = 0x88;
const OP_REQUEST_OP: u8 = 0x89;
const OP_TEXT_AT: u8 = 0x8a;
const OP_TEXT_SLICE: u8 = 0x8b;
const OP_TEXT_IS_BOUNDARY: u8 = 0x8c;
const OP_TEXT_SLICE_BYTES: u8 = 0x8d;
const OP_TEXT_BYTES: u8 = 0x8e;
const OP_TEXT_LT: u8 = 0x8f;
const OP_TEXT_LE: u8 = 0x90;
const OP_TEXT_GT: u8 = 0x91;
const OP_TEXT_GE: u8 = 0x92;
const OP_TEXT_TO_STRING: u8 = 0x93;
const OP_CHAR_CODEPOINT: u8 = 0x94;
const OP_CHAR_UTF8_LEN: u8 = 0x95;
const OP_EQ_CHAR: u8 = 0x96;
const OP_NE_CHAR: u8 = 0x97;
const OP_LT_CHAR: u8 = 0x98;
const OP_LE_CHAR: u8 = 0x99;
const OP_GT_CHAR: u8 = 0x9a;
const OP_GE_CHAR: u8 = 0x9b;
const OP_BYTES_COMPACT: u8 = 0x9c;
const OP_BYTES_TEXT_VIEW: u8 = 0x9d;
const OP_LT_BYTES: u8 = 0x9e;
const OP_LE_BYTES: u8 = 0x9f;
const OP_GT_BYTES: u8 = 0xa0;
const OP_GE_BYTES: u8 = 0xa1;
const OP_SB_APPEND_CHAR: u8 = 0xa2;
const OP_SB_BYTE_LEN: u8 = 0xa3;
const OP_SB_FINISH: u8 = 0xa4;
const OP_BB_FINISH: u8 = 0xa5;
const OP_TEXT_FIND_BYTE_INDEX: u8 = 0xa6;
const OP_TEXT_AT_BYTE: u8 = 0xa7;
const OP_TEXT_TRIM: u8 = 0xa8;
const OP_TEXT_TRIM_START: u8 = 0xa9;
const OP_TEXT_TRIM_END: u8 = 0xaa;
const OP_TEXT_TO_LOWER_ASCII: u8 = 0xab;
const OP_TEXT_TO_UPPER_ASCII: u8 = 0xac;
const OP_TEXT_REPLACE: u8 = 0xad;
const OP_TEXT_PARSE_INT_STATUS: u8 = 0xae;
const OP_TEXT_PARSE_INT_VALUE: u8 = 0xaf;
const OP_BYTES_ENDS_WITH: u8 = 0xb0;
const OP_BYTES_CONTAINS: u8 = 0xb1;
const OP_TEXT_SPLIT: u8 = 0xb2;
const OP_TEXT_LINES: u8 = 0xb3;
const OP_BB_AT: u8 = 0xb4;
const OP_BB_FIND_FROM: u8 = 0xb5;
const OP_OPTION_SOME: u8 = 0xb8;
const OP_OPTION_NONE: u8 = 0xb9;
const OP_LIST_GET: u8 = 0xba;
const OP_MAP_GET: u8 = 0xbb;
const OP_CALL_INTERFACE: u8 = 0xbc;
const OP_LIST_EPOCH: u8 = 0xbd;
const OP_LIST_ITER_LEN: u8 = 0xbe;
const OP_MAP_EPOCH: u8 = 0xbf;
const OP_MAP_ITER_LEN: u8 = 0xc0;
const OP_MAP_KEY_AT: u8 = 0xc1;
const OP_MAP_VALUE_AT: u8 = 0xc2;
const OP_LIST_CAPACITY: u8 = 0xc3;
const OP_LIST_SET: u8 = 0xc4;
const OP_LIST_POP: u8 = 0xc5;
const OP_LIST_INSERT: u8 = 0xc6;
const OP_LIST_REMOVE: u8 = 0xc7;
const OP_LIST_SWAP_REMOVE: u8 = 0xc8;
const OP_LIST_RESERVE: u8 = 0xc9;
const OP_LIST_TRUNCATE: u8 = 0xca;
const OP_LIST_CONTAINS: u8 = 0xcb;
const OP_MAP_REMOVE: u8 = 0xcc;
const OP_MAP_CLEAR: u8 = 0xcd;
const OP_MAP_RESERVE: u8 = 0xce;
const OP_MAKE_CALLBACK: u8 = 0xcf;
const OP_AS_CALLBACK: u8 = 0xd0;
const OP_OPTION_PAYLOAD: u8 = 0xd1;
const OP_LIST_REORDER: u8 = 0xd2;
const OP_CALL_SLOT: u8 = 0xd3;
const OP_NEW_SLOT: u8 = 0xd4;
const OP_LOAD_SLOT: u8 = 0xd5;
const OP_SEND_SLOT: u8 = 0xd6;
const OP_SYNTAX_TREE_ROOT: u8 = 0xd7;
const OP_SYNTAX_KIND: u8 = 0xd8;
const OP_SYNTAX_CATEGORY: u8 = 0xd9;
const OP_SYNTAX_RANGE_START: u8 = 0xda;
const OP_SYNTAX_RANGE_END: u8 = 0xdb;
const OP_SYNTAX_TEXT: u8 = 0xdc;
const OP_SYNTAX_CHILDREN: u8 = 0xdd;
const OP_SYNTAX_DETACH: u8 = 0xde;
const OP_DYN_PACK: u8 = 0xdf;
const OP_DYN_RENDER: u8 = 0xe0;
const OP_SYNTAX_BUILD_TOKEN: u8 = 0xe1;
const OP_SYNTAX_BUILD_TRIVIA: u8 = 0xe2;
const OP_SYNTAX_BUILD_NODE: u8 = 0xe3;
const OP_SYNTAX_TO_TREE: u8 = 0xe4;
const OP_FUNCTION_CODE: u8 = 0xe5;
const OP_CLASS_CODE: u8 = 0xe6;
const OP_CODE_SOURCE: u8 = 0xe7;
const OP_FAULT_SITE: u8 = 0xe8;
const OP_FAULT_TRACE: u8 = 0xe9;
const OP_CODE_DEFINITION: u8 = 0xea;
const OP_RAISE_USER_PANIC: u8 = 0xeb;
const OP_RAISE_ASSERTION_FAILED: u8 = 0xec;
const OP_TEXT_HASH: u8 = 0xed;
const OP_BYTES_HASH: u8 = 0xee;
const OP_MAP_PROBE: u8 = 0xef;
const OP_MAP_PROBE_FOUND: u8 = 0xf0;
const OP_MAP_PROBE_KEY: u8 = 0xf1;
const OP_MAP_PROBE_VALUE: u8 = 0xf2;
const OP_MAP_PROBE_SET_VALUE: u8 = 0xf3;
const OP_MAP_PROBE_REMOVE: u8 = 0xf4;
const OP_MAP_INSERT_HASHED: u8 = 0xf5;
const OP_MAP_WRITE_GUARD: u8 = 0xf6;
const OP_HASH_COMBINE: u8 = 0xf7;
const OP_HASH_UNORDERED_COMBINE: u8 = 0xf8;
const OP_MAP_NEXT_INDEX: u8 = 0xf9;
const OP_SEAL_INSTANCE: u8 = 0xfa;
const OP_NUMERIC: u8 = 0xfb;
const OP_CONST_FLOAT: u8 = 0xfc;
const OP_CONST_BYTES: u8 = 0xfd;
const OP_EXTENSION: u8 = 0xfe;
const OP_PREPARE_WAIT: u8 = 0xff;

const EXT_TEXT_PAD_START: u8 = 0;
const EXT_TEXT_PAD_END: u8 = 1;
const EXT_MAP_PUT_TEXT: u8 = 2;
const EXT_BYTES_TEXT_RANGE: u8 = 3;
const EXT_MAP_INTERN_TEXT_RANGE: u8 = 4;
const EXT_CONST_REGEX: u8 = 5;
const EXT_REGEX_COMPILE_STATUS: u8 = 6;
const EXT_REGEX_COMPILE_VALUE: u8 = 7;
const EXT_REGEX_SOURCE: u8 = 8;
const EXT_REGEX_IS_MATCH: u8 = 9;
const EXT_REGEX_COUNT: u8 = 10;
const EXT_REGEX_SPLIT: u8 = 11;
const EXT_REGEX_REPLACE_ALL: u8 = 12;
const EXT_REGEX_MATCH_START: u8 = 13;
const EXT_REGEX_MATCH_END: u8 = 14;
const EXT_REGEX_MATCH_TEXT: u8 = 15;
const EXT_REGEX_MATCH_GROUP_COUNT: u8 = 16;
const EXT_REGEX_CAPTURES: u8 = 17;
const EXT_REGEX_MATCH_GROUP: u8 = 18;
const EXT_REGEX_MATCH_NAMED: u8 = 19;
const EXT_LIST_SWAP: u8 = 20;
const EXT_BB_SET: u8 = 21;
const EXT_BB_CAPACITY: u8 = 22;
const EXT_BB_TRUNCATE: u8 = 23;
const EXT_BYTES_READ_U32_BE: u8 = 24;
const EXT_BYTES_READ_U32_LE: u8 = 25;
const EXT_MODULE_CODE: u8 = 26;
const EXT_REFLECTION_DECLARATIONS: u8 = 27;
const EXT_REFLECTION_MEMBERS: u8 = 28;
const EXT_REFLECTION_NAME: u8 = 29;
const EXT_REFLECTION_DECLARATION_KIND: u8 = 30;
const EXT_REFLECTION_MEMBER_KIND: u8 = 31;

fn native_extension_tag(instr: NativeInstr) -> Option<u8> {
    Some(match instr {
        NativeInstr::RegexCompileStatus => EXT_REGEX_COMPILE_STATUS,
        NativeInstr::RegexCompileValue => EXT_REGEX_COMPILE_VALUE,
        NativeInstr::RegexSource => EXT_REGEX_SOURCE,
        NativeInstr::RegexIsMatch => EXT_REGEX_IS_MATCH,
        NativeInstr::RegexCount => EXT_REGEX_COUNT,
        NativeInstr::RegexSplit => EXT_REGEX_SPLIT,
        NativeInstr::RegexReplaceAll => EXT_REGEX_REPLACE_ALL,
        NativeInstr::RegexMatchStart => EXT_REGEX_MATCH_START,
        NativeInstr::RegexMatchEnd => EXT_REGEX_MATCH_END,
        NativeInstr::RegexMatchText => EXT_REGEX_MATCH_TEXT,
        NativeInstr::RegexMatchGroupCount => EXT_REGEX_MATCH_GROUP_COUNT,
        NativeInstr::BbSet => EXT_BB_SET,
        NativeInstr::BbCapacity => EXT_BB_CAPACITY,
        NativeInstr::BbTruncate => EXT_BB_TRUNCATE,
        NativeInstr::BytesReadU32Be => EXT_BYTES_READ_U32_BE,
        NativeInstr::BytesReadU32Le => EXT_BYTES_READ_U32_LE,
        _ => return None,
    })
}

// Type tags for the serialized type table.
const TY_UNIT: u8 = 0;
const TY_BOOL: u8 = 1;
const TY_INT: u8 = 2;
const TY_STR: u8 = 3;
const TY_CLASS: u8 = 4;
const TY_LIST: u8 = 5;
const TY_MAP: u8 = 6;
const TY_FN: u8 = 7;
const TY_INST: u8 = 10;
const TY_TUPLE: u8 = 11;
const TY_VAR: u8 = 12;
const TY_FAULT: u8 = 13;
const TY_REQUEST: u8 = 14;
const TY_POLICY_TABLE: u8 = 15;
const TY_VM: u8 = 16;
const TY_RUN: u8 = 17;
const TY_PENDING_CALL: u8 = 18;
const TY_OP: u8 = 19;
const TY_DIGEST: u8 = 20;
const TY_HANDLE: u8 = 21;
const TY_VM_SNAPSHOT: u8 = 22;
const TY_RUN_SNAPSHOT: u8 = 23;
const TY_BYTES: u8 = 24;
const TY_FILE_HANDLE: u8 = 25;
const TY_RESOURCE_HANDLE: u8 = 26;
const TY_WAIT: u8 = 27;
const TY_PROJECTION: u8 = 28;
const TY_CALLBACK: u8 = 29;
const TY_HOST_RESOURCE: u8 = 30;
const TY_FLOAT: u8 = 31;
const TY_NEVER: u8 = 32;

// Row element tags.
const ROW_OP: u8 = 0;
const ROW_VAR: u8 = 1;
const ROW_GROUP: u8 = 2;

// Class kind tags.
const KIND_NORMAL: u8 = 0;
const KIND_ABSTRACT: u8 = 1;
const KIND_CASE: u8 = 2;

const SLOT_FUNCTION: u8 = 0;
const SLOT_METHOD: u8 = 1;
const SLOT_CLASS: u8 = 2;
const SLOT_VALUE: u8 = 3;
const SLOT_PROCESS: u8 = 4;

/// Encode a module into the sectioned container form.
///
/// The container holds the magic and version header, a section
/// table, the semantic region, the export section with the
/// definition names and the function bindings, and an optional debug
/// section. The semantic bytes of a definition do not contain
/// its own name.
pub fn encode(module: &Module) -> Vec<u8> {
    encode_with_bundle(module, &lm_abi::standard_bundle())
}

/// Encode a module for one exact ABI bundle.
pub fn encode_with_bundle(module: &Module, bundle: &lm_abi::AbiBundle) -> Vec<u8> {
    let semantic = encode_semantic(module);
    let exports = encode_exports(module);
    let debug = &module.debug;
    let mut out = Vec::with_capacity(HEADER_LEN + semantic.len() + exports.len() + debug.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&bundle.digest());
    let mut offset = HEADER_LEN as u32;
    for section in [&semantic, &exports, debug] {
        write_u32(&mut out, offset);
        write_u32(&mut out, section.len() as u32);
        offset += section.len() as u32;
    }
    out.extend_from_slice(&semantic);
    out.extend_from_slice(&exports);
    out.extend_from_slice(debug);
    out
}

/// The semantic region of one module, as bytes. This is the exact
/// input set of the verifier: the strings, types, selectors,
/// applications, classes, functions, and the entry index, with every
/// module-global index preserved. The definition names and the debug
/// content stay outside, so neither a rename nor a debug edit changes
/// these bytes.
pub fn semantic_section(module: &Module) -> Vec<u8> {
    encode_semantic(module)
}

/// The exact linker-visible surface of one module.
///
/// The section excludes debug data. It includes every name, key,
/// binding, and export target that can change linking or execution.
pub(crate) fn linkage_section(module: &Module) -> Vec<u8> {
    encode_exports(module)
}

/// Encode the semantic region: every table except the definition
/// names.
fn encode_semantic(module: &Module) -> Vec<u8> {
    let mut out = Vec::new();
    write_u32(&mut out, module.strings.len() as u32);
    for s in &module.strings {
        write_bytes(&mut out, s.as_bytes());
    }
    write_u32(&mut out, module.bytes.len() as u32);
    for bytes in &module.bytes {
        write_bytes(&mut out, bytes);
    }
    write_u32(&mut out, module.types.len() as u32);
    for ty in &module.types {
        encode_type(&mut out, ty);
    }
    write_u32(&mut out, module.selectors.len() as u32);
    for s in &module.selectors {
        write_bytes(&mut out, s.as_bytes());
    }
    write_u32(&mut out, module.apps.len() as u32);
    for app in &module.apps {
        write_u32(&mut out, app.types.len() as u32);
        for t in &app.types {
            write_u32(&mut out, *t);
        }
        write_u32(&mut out, app.rows.len() as u32);
        for row in &app.rows {
            encode_row(&mut out, row);
        }
    }
    write_u32(&mut out, module.interfaces.len() as u32);
    for interface in &module.interfaces {
        write_u32(&mut out, interface.type_params);
        write_u32(&mut out, interface.effect_params);
        write_u32(&mut out, interface.generic_is_effect.len() as u32);
        for is_effect in &interface.generic_is_effect {
            out.push(u8::from(*is_effect));
        }
        write_u32(&mut out, interface.parents.len() as u32);
        for parent in &interface.parents {
            encode_interface_use(&mut out, parent);
        }
        encode_type_bounds(&mut out, &interface.type_bounds);
        write_u32(&mut out, interface.associated.len() as u32);
        for associated in &interface.associated {
            write_bytes(&mut out, associated.name.as_bytes());
            write_u32(&mut out, associated.bounds.len() as u32);
            for bound in &associated.bounds {
                encode_interface_use(&mut out, bound);
            }
        }
        write_u32(&mut out, interface.methods.len() as u32);
        for method in &interface.methods {
            write_u32(&mut out, method.selector);
            out.push(u8::from(method.mut_self));
            write_u32(&mut out, method.type_params);
            encode_type_bounds(&mut out, &method.type_bounds);
            write_u32(&mut out, method.effect_params);
            write_u32(&mut out, method.premises.len() as u32);
            for premise in &method.premises {
                write_u32(&mut out, premise.subject);
                write_u32(&mut out, premise.bounds.len() as u32);
                for bound in &premise.bounds {
                    encode_interface_use(&mut out, bound);
                }
            }
            write_u32(&mut out, method.params.len() as u32);
            for param in &method.params {
                write_u32(&mut out, *param);
            }
            write_u32(&mut out, method.param_muts.len() as u32);
            for marker in &method.param_muts {
                out.push(u8::from(*marker));
            }
            write_u32(&mut out, method.ret);
            encode_row(&mut out, &method.row);
            write_u32(&mut out, method.default);
        }
    }
    write_u32(&mut out, module.conformances.len() as u32);
    for conformance in &module.conformances {
        write_u32(&mut out, conformance.class);
        encode_interface_use(&mut out, &conformance.application);
        write_u32(&mut out, conformance.premises.len() as u32);
        for premise in &conformance.premises {
            write_u32(&mut out, premise.param);
            write_u32(&mut out, premise.bounds.len() as u32);
            for bound in &premise.bounds {
                encode_interface_use(&mut out, bound);
            }
        }
        write_u32(&mut out, conformance.associated.len() as u32);
        for associated in &conformance.associated {
            write_u32(&mut out, *associated);
        }
        write_u32(&mut out, conformance.method_overrides.len() as u32);
        for selected in &conformance.method_overrides {
            out.push(u8::from(*selected));
        }
    }
    write_u32(&mut out, module.class_bounds.len() as u32);
    for bounds in &module.class_bounds {
        encode_type_bounds(&mut out, bounds);
    }
    write_u32(&mut out, module.func_bounds.len() as u32);
    for bounds in &module.func_bounds {
        encode_type_bounds(&mut out, bounds);
    }
    write_u32(&mut out, module.imports.len() as u32);
    for import in &module.imports {
        write_bytes(&mut out, import.module.as_bytes());
        write_bytes(&mut out, import.name.as_bytes());
        out.push(import.kind.tag());
        write_u32(&mut out, import.def);
        out.extend_from_slice(&import.hash);
    }
    write_u32(&mut out, module.slots.len() as u32);
    for slot in &module.slots {
        out.extend_from_slice(&slot.key);
        out.extend_from_slice(&slot.contract_hash);
        encode_slot_contract(&mut out, &slot.contract);
        match slot.initial {
            None => out.push(0),
            Some(SlotTarget::Function(func)) => {
                out.push(1);
                write_u32(&mut out, func);
            }
            Some(SlotTarget::Class { class, constructor }) => {
                out.push(2);
                write_u32(&mut out, class);
                write_u32(&mut out, constructor);
            }
        }
    }
    // The core role table: one class index per stable role.
    for slot in &module.core_roles {
        write_u32(&mut out, *slot);
    }
    write_u32(&mut out, module.classes.len() as u32);
    for class in &module.classes {
        write_u32(&mut out, class.parent);
        write_u32(&mut out, class.parent_args.len() as u32);
        for arg in &class.parent_args {
            write_u32(&mut out, *arg);
        }
        write_u32(&mut out, class.type_params);
        out.push(match class.kind {
            BcClassKind::Normal => KIND_NORMAL,
            BcClassKind::Abstract => KIND_ABSTRACT,
            BcClassKind::Case => KIND_CASE,
        });
        out.push(u8::from(class.is_final));
        out.push(u8::from(class.is_frozen));
        write_u32(&mut out, class.fields.len() as u32);
        for (name, ty) in &class.fields {
            write_bytes(&mut out, name.as_bytes());
            write_u32(&mut out, *ty);
        }
        write_u32(&mut out, class.methods.len() as u32);
        for (sel, func) in &class.methods {
            write_u32(&mut out, *sel);
            write_u32(&mut out, *func);
        }
    }
    write_u32(&mut out, module.reflections.len() as u32);
    for reflection in &module.reflections {
        write_bytes(&mut out, reflection.name.as_bytes());
        write_u32(&mut out, reflection.declarations.len() as u32);
        for declaration in &reflection.declarations {
            out.push(declaration.kind.tag());
            write_bytes(&mut out, declaration.name.as_bytes());
            write_u32(&mut out, declaration.def);
            write_u32(&mut out, declaration.callable);
        }
    }
    write_u32(&mut out, module.funcs.len() as u32);
    for func in &module.funcs {
        write_u32(&mut out, func.type_params);
        write_u32(&mut out, func.effect_params);
        write_u32(&mut out, func.params.len() as u32);
        for p in &func.params {
            write_u32(&mut out, *p);
        }
        // The marker vector carries its own count. The encoding is
        // therefore self-describing: no reader takes the count from
        // the parameter table.
        write_u32(&mut out, func.param_muts.len() as u32);
        for m in &func.param_muts {
            out.push(u8::from(*m));
        }
        write_u32(&mut out, func.ret);
        encode_row(&mut out, &func.row);
        write_u32(&mut out, func.captures.len() as u32);
        for c in &func.captures {
            write_u32(&mut out, *c);
        }
        write_u32(&mut out, func.local_types.len() as u32);
        for t in &func.local_types {
            write_u32(&mut out, *t);
        }
        write_u32(&mut out, func.blocks.len() as u32);
        for block in &func.blocks {
            write_u32(&mut out, block.len() as u32);
            for instr in block {
                encode_instr(&mut out, instr);
            }
        }
    }
    write_u32(&mut out, module.entry);
    out
}

fn encode_callable_contract(out: &mut Vec<u8>, contract: &BcCallableContract) {
    write_u32(out, contract.type_params);
    write_u32(out, contract.effect_params);
    encode_type_bounds(out, &contract.type_bounds);
    write_u32(out, contract.params.len() as u32);
    for param in &contract.params {
        write_u32(out, *param);
    }
    write_u32(out, contract.param_muts.len() as u32);
    for marker in &contract.param_muts {
        out.push(u8::from(*marker));
    }
    write_u32(out, contract.ret);
    encode_row(out, &contract.row);
}

fn encode_slot_contract(out: &mut Vec<u8>, contract: &SlotContract) {
    match contract {
        SlotContract::Function(callable) => {
            out.push(SLOT_FUNCTION);
            encode_callable_contract(out, callable);
        }
        SlotContract::Method(callable) => {
            out.push(SLOT_METHOD);
            encode_callable_contract(out, callable);
        }
        SlotContract::Class {
            type_params,
            abi,
            ty,
            constructor,
        } => {
            out.push(SLOT_CLASS);
            write_u32(out, *type_params);
            out.extend_from_slice(abi);
            write_u32(out, *ty);
            encode_callable_contract(out, constructor);
        }
        SlotContract::Value { ty } => {
            out.push(SLOT_VALUE);
            write_u32(out, *ty);
        }
        SlotContract::Process { message, result } => {
            out.push(SLOT_PROCESS);
            write_u32(out, *message);
            write_u32(out, *result);
        }
    }
}

/// Encode the export section: the definition names and the class
/// qualified keys in definition index order, classes first, then
/// functions, and then the exported top-level definitions.
fn encode_exports(module: &Module) -> Vec<u8> {
    let mut out = Vec::new();
    write_u32(&mut out, module.interfaces.len() as u32);
    for interface in &module.interfaces {
        write_bytes(&mut out, interface.name.as_bytes());
        write_bytes(&mut out, interface.key.as_bytes());
        for method in &interface.methods {
            write_u32(&mut out, method.param_names.len() as u32);
            for name in &method.param_names {
                write_bytes(&mut out, name.as_bytes());
            }
        }
    }
    write_u32(&mut out, module.classes.len() as u32);
    for class in &module.classes {
        write_bytes(&mut out, class.name.as_bytes());
        write_bytes(&mut out, class.key.as_bytes());
        write_u32(&mut out, class.field_defaults.len() as u32);
        for marker in &class.field_defaults {
            out.push(u8::from(*marker));
        }
        write_u32(&mut out, class.own_start);
        out.push(u8::from(class.has_init));
    }
    write_u32(&mut out, module.funcs.len() as u32);
    for func in &module.funcs {
        write_bytes(&mut out, func.name.as_bytes());
        write_u32(&mut out, func.param_names.len() as u32);
        for name in &func.param_names {
            write_bytes(&mut out, name.as_bytes());
        }
    }
    write_u32(&mut out, module.slots.len() as u32);
    for slot in &module.slots {
        write_bytes(&mut out, slot.binding.as_bytes());
        out.push(u8::from(slot.late));
    }
    write_u32(&mut out, module.bindings.len() as u32);
    for binding in &module.bindings {
        write_bytes(&mut out, binding.key.as_bytes());
        write_u32(&mut out, binding.func);
        write_u32(&mut out, binding.class);
    }
    write_u32(&mut out, module.exports.len() as u32);
    for export in &module.exports {
        out.push(export.kind.tag());
        write_bytes(&mut out, export.name.as_bytes());
        out.push(u8::from(export.source));
        write_u32(&mut out, export.def);
        write_u32(&mut out, export.ctor);
        match &export.constant {
            None => out.push(0),
            Some(constant) => {
                out.push(1);
                write_u32(&mut out, constant.ty);
                interface::encode_const_value(&mut out, &constant.value);
            }
        }
    }
    out
}

fn encode_row(out: &mut Vec<u8>, row: &[BcRow]) {
    write_u32(out, row.len() as u32);
    for elem in row {
        match elem {
            BcRow::Op(idx) => {
                out.push(ROW_OP);
                write_u32(out, *idx);
            }
            BcRow::Group(idx) => {
                out.push(ROW_GROUP);
                write_u32(out, *idx);
            }
            BcRow::Var(idx) => {
                out.push(ROW_VAR);
                write_u32(out, *idx);
            }
        }
    }
}

fn encode_interface_use(out: &mut Vec<u8>, application: &BcInterfaceUse) {
    write_u32(out, application.interface);
    write_u32(out, application.types.len() as u32);
    for ty in &application.types {
        write_u32(out, *ty);
    }
    write_u32(out, application.rows.len() as u32);
    for row in &application.rows {
        encode_row(out, row);
    }
}

fn encode_type_bounds(out: &mut Vec<u8>, bounds: &[Vec<BcInterfaceUse>]) {
    write_u32(out, bounds.len() as u32);
    for parameter in bounds {
        write_u32(out, parameter.len() as u32);
        for application in parameter {
            encode_interface_use(out, application);
        }
    }
}

fn encode_type(out: &mut Vec<u8>, ty: &BcType) {
    match ty {
        BcType::Unit => out.push(TY_UNIT),
        BcType::Never => out.push(TY_NEVER),
        BcType::Bool => out.push(TY_BOOL),
        BcType::Int => out.push(TY_INT),
        BcType::Float => out.push(TY_FLOAT),
        BcType::Str => out.push(TY_STR),
        BcType::Class(c) => {
            out.push(TY_CLASS);
            write_u32(out, *c);
        }
        BcType::Inst(c, args) => {
            out.push(TY_INST);
            write_u32(out, *c);
            write_u32(out, args.len() as u32);
            for a in args {
                write_u32(out, *a);
            }
        }
        BcType::List(e) => {
            out.push(TY_LIST);
            write_u32(out, *e);
        }
        BcType::Map(k, v) => {
            out.push(TY_MAP);
            write_u32(out, *k);
            write_u32(out, *v);
        }
        BcType::Tuple(elems) => {
            out.push(TY_TUPLE);
            write_u32(out, elems.len() as u32);
            for e in elems {
                write_u32(out, *e);
            }
        }
        BcType::Fn(params, muts, ret, row) => {
            out.push(TY_FN);
            write_u32(out, params.len() as u32);
            for p in params {
                write_u32(out, *p);
            }
            // The marker vector carries its own count, like the
            // parameter marker vector of a function.
            write_u32(out, muts.len() as u32);
            for m in muts {
                out.push(u8::from(*m));
            }
            write_u32(out, *ret);
            encode_row(out, row);
        }
        BcType::Callback(params, muts, ret, row) => {
            out.push(TY_CALLBACK);
            write_u32(out, params.len() as u32);
            for p in params {
                write_u32(out, *p);
            }
            write_u32(out, muts.len() as u32);
            for m in muts {
                out.push(u8::from(*m));
            }
            write_u32(out, *ret);
            encode_row(out, row);
        }
        BcType::Var(i) => {
            out.push(TY_VAR);
            write_u32(out, *i);
        }
        BcType::Projection {
            base,
            interface,
            assoc,
        } => {
            out.push(TY_PROJECTION);
            write_u32(out, *base);
            write_u32(out, *interface);
            write_u32(out, *assoc);
        }
        BcType::Digest => out.push(TY_DIGEST),
        BcType::Fault => out.push(TY_FAULT),
        BcType::Request => out.push(TY_REQUEST),
        BcType::PolicyTable => out.push(TY_POLICY_TABLE),
        BcType::Vm => out.push(TY_VM),
        BcType::Run(t) => {
            out.push(TY_RUN);
            write_u32(out, *t);
        }
        BcType::Wait(t) => {
            out.push(TY_WAIT);
            write_u32(out, *t);
        }
        BcType::PendingCall(a, r) => {
            out.push(TY_PENDING_CALL);
            write_u32(out, *a);
            write_u32(out, *r);
        }
        BcType::Handle(m, r) => {
            out.push(TY_HANDLE);
            write_u32(out, *m);
            write_u32(out, *r);
        }
        BcType::VmSnapshot => out.push(TY_VM_SNAPSHOT),
        BcType::RunSnapshot(t) => {
            out.push(TY_RUN_SNAPSHOT);
            write_u32(out, *t);
        }
        BcType::Bytes => out.push(TY_BYTES),
        BcType::FileHandle => out.push(TY_FILE_HANDLE),
        BcType::ResourceHandle => out.push(TY_RESOURCE_HANDLE),
        BcType::HostResource => out.push(TY_HOST_RESOURCE),
        BcType::Op(op, f) => {
            out.push(TY_OP);
            write_u32(out, *op);
            write_u32(out, *f);
        }
    }
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    write_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

fn encode_instr(out: &mut Vec<u8>, instr: &Instr) {
    match instr {
        Instr::ConstUnit => out.push(OP_CONST_UNIT),
        Instr::ConstBool(v) => {
            out.push(OP_CONST_BOOL);
            out.push(u8::from(*v));
        }
        Instr::ConstInt(v) => {
            out.push(OP_CONST_INT);
            out.extend_from_slice(&v.to_le_bytes());
        }
        Instr::ConstFloat(bits) => {
            out.push(OP_CONST_FLOAT);
            out.extend_from_slice(&bits.to_le_bytes());
        }
        Instr::ConstChar(value) => {
            out.push(OP_CONST_CHAR);
            write_u32(out, *value);
        }
        Instr::ConstStr(idx) => {
            out.push(OP_CONST_STR);
            write_u32(out, *idx);
        }
        Instr::ConstBytes(idx) => {
            out.push(OP_CONST_BYTES);
            write_u32(out, *idx);
        }
        Instr::ConstRegex(idx) => {
            out.push(OP_EXTENSION);
            out.push(EXT_CONST_REGEX);
            write_u32(out, *idx);
        }
        Instr::Numeric(instr) => {
            out.push(OP_NUMERIC);
            out.push(*instr as u8);
        }
        Instr::LoadLocal(slot) => {
            out.push(OP_LOAD_LOCAL);
            write_u32(out, *slot);
        }
        Instr::StoreLocal(slot) => {
            out.push(OP_STORE_LOCAL);
            write_u32(out, *slot);
        }
        Instr::Pop => out.push(OP_POP),
        Instr::Add => out.push(OP_ADD),
        Instr::Sub => out.push(OP_SUB),
        Instr::Mul => out.push(OP_MUL),
        Instr::Div => out.push(OP_DIV),
        Instr::Rem => out.push(OP_REM),
        Instr::Neg => out.push(OP_NEG),
        Instr::Not => out.push(OP_NOT),
        Instr::LtInt => out.push(OP_LT_INT),
        Instr::LeInt => out.push(OP_LE_INT),
        Instr::GtInt => out.push(OP_GT_INT),
        Instr::GeInt => out.push(OP_GE_INT),
        Instr::EqInt => out.push(OP_EQ_INT),
        Instr::NeInt => out.push(OP_NE_INT),
        Instr::EqBool => out.push(OP_EQ_BOOL),
        Instr::NeBool => out.push(OP_NE_BOOL),
        Instr::Native(NativeInstr::EqStr) => out.push(OP_EQ_STR),
        Instr::Native(NativeInstr::NeStr) => out.push(OP_NE_STR),
        Instr::Native(NativeInstr::StrByteLen) => out.push(OP_STR_BYTE_LEN),
        Instr::Native(NativeInstr::StrCharCount) => out.push(OP_STR_CHAR_COUNT),
        Instr::Native(NativeInstr::StrConcat) => out.push(OP_STR_CONCAT),
        Instr::Native(NativeInstr::StrStartsWith) => out.push(OP_STR_STARTS_WITH),
        Instr::Native(NativeInstr::StrEndsWith) => out.push(OP_STR_ENDS_WITH),
        Instr::Native(NativeInstr::StrContains) => out.push(OP_STR_CONTAINS),
        Instr::Native(NativeInstr::StrFindIndex) => out.push(OP_STR_FIND_INDEX),
        Instr::Native(NativeInstr::TextFindByteIndex) => out.push(OP_TEXT_FIND_BYTE_INDEX),
        Instr::Native(NativeInstr::TextAtByte) => out.push(OP_TEXT_AT_BYTE),
        Instr::Native(NativeInstr::TextTrim) => out.push(OP_TEXT_TRIM),
        Instr::Native(NativeInstr::TextTrimStart) => out.push(OP_TEXT_TRIM_START),
        Instr::Native(NativeInstr::TextTrimEnd) => out.push(OP_TEXT_TRIM_END),
        Instr::Native(NativeInstr::TextToLowerAscii) => out.push(OP_TEXT_TO_LOWER_ASCII),
        Instr::Native(NativeInstr::TextToUpperAscii) => out.push(OP_TEXT_TO_UPPER_ASCII),
        Instr::Native(NativeInstr::TextReplace) => out.push(OP_TEXT_REPLACE),
        Instr::Native(NativeInstr::TextParseIntStatus) => out.push(OP_TEXT_PARSE_INT_STATUS),
        Instr::Native(NativeInstr::TextParseIntValue) => out.push(OP_TEXT_PARSE_INT_VALUE),
        Instr::Native(NativeInstr::TextPadStart) => {
            out.push(OP_EXTENSION);
            out.push(EXT_TEXT_PAD_START);
        }
        Instr::Native(NativeInstr::TextPadEnd) => {
            out.push(OP_EXTENSION);
            out.push(EXT_TEXT_PAD_END);
        }
        Instr::Native(NativeInstr::BytesEndsWith) => out.push(OP_BYTES_ENDS_WITH),
        Instr::Native(NativeInstr::BytesContains) => out.push(OP_BYTES_CONTAINS),
        Instr::Native(NativeInstr::TextSplit) => out.push(OP_TEXT_SPLIT),
        Instr::Native(NativeInstr::TextLines) => out.push(OP_TEXT_LINES),
        Instr::Native(NativeInstr::TextAt) => out.push(OP_TEXT_AT),
        Instr::Native(NativeInstr::TextSlice) => out.push(OP_TEXT_SLICE),
        Instr::Native(NativeInstr::TextIsBoundary) => out.push(OP_TEXT_IS_BOUNDARY),
        Instr::Native(NativeInstr::TextSliceBytes) => out.push(OP_TEXT_SLICE_BYTES),
        Instr::Native(NativeInstr::TextBytes) => out.push(OP_TEXT_BYTES),
        Instr::Native(NativeInstr::TextLt) => out.push(OP_TEXT_LT),
        Instr::Native(NativeInstr::TextLe) => out.push(OP_TEXT_LE),
        Instr::Native(NativeInstr::TextGt) => out.push(OP_TEXT_GT),
        Instr::Native(NativeInstr::TextGe) => out.push(OP_TEXT_GE),
        Instr::Native(NativeInstr::TextToString) => out.push(OP_TEXT_TO_STRING),
        Instr::Native(NativeInstr::CharCodepoint) => out.push(OP_CHAR_CODEPOINT),
        Instr::Native(NativeInstr::CharUtf8Len) => out.push(OP_CHAR_UTF8_LEN),
        Instr::Native(NativeInstr::EqChar) => out.push(OP_EQ_CHAR),
        Instr::Native(NativeInstr::NeChar) => out.push(OP_NE_CHAR),
        Instr::Native(NativeInstr::LtChar) => out.push(OP_LT_CHAR),
        Instr::Native(NativeInstr::LeChar) => out.push(OP_LE_CHAR),
        Instr::Native(NativeInstr::GtChar) => out.push(OP_GT_CHAR),
        Instr::Native(NativeInstr::GeChar) => out.push(OP_GE_CHAR),
        Instr::EqRef => out.push(OP_EQ_REF),
        Instr::EqValue => out.push(OP_EQ_VALUE),
        Instr::NeValue => out.push(OP_NE_VALUE),
        Instr::CallInterface { site, recv_ty, app } => {
            out.push(OP_CALL_INTERFACE);
            write_u32(out, *site);
            write_u32(out, *recv_ty);
            encode_optional_index(out, *app);
        }
        Instr::Extended(instr) => encode_extended(out, *instr),
        Instr::NeRef => out.push(OP_NE_REF),
        Instr::Call(idx) => {
            out.push(OP_CALL);
            write_u32(out, *idx);
        }
        Instr::CallG { func, app } => {
            out.push(OP_CALL_G);
            write_u32(out, *func);
            write_u32(out, *app);
        }
        Instr::CallVirtual { selector, argc } => {
            out.push(OP_CALL_VIRTUAL);
            write_u32(out, *selector);
            write_u32(out, *argc);
        }
        Instr::CallVirtualG {
            selector,
            argc,
            app,
        } => {
            out.push(OP_CALL_VIRTUAL_G);
            write_u32(out, *selector);
            write_u32(out, *argc);
            write_u32(out, *app);
        }
        Instr::CallValue { argc } => {
            out.push(OP_CALL_VALUE);
            write_u32(out, *argc);
        }
        Instr::MakeClosure { func, captures } => {
            out.push(OP_MAKE_CLOSURE);
            write_u32(out, *func);
            write_u32(out, *captures);
        }
        Instr::LoadCapture(idx) => {
            out.push(OP_LOAD_CAPTURE);
            write_u32(out, *idx);
        }
        Instr::New(class) => {
            out.push(OP_NEW);
            write_u32(out, *class);
        }
        Instr::NewG { class, app } => {
            out.push(OP_NEW_G);
            write_u32(out, *class);
            write_u32(out, *app);
        }
        Instr::LoadField(field) => {
            out.push(OP_LOAD_FIELD);
            write_u32(out, *field);
        }
        Instr::StoreField(field) => {
            out.push(OP_STORE_FIELD);
            write_u32(out, *field);
        }
        Instr::TupleNew { ty, count } => {
            out.push(OP_TUPLE_NEW);
            write_u32(out, *ty);
            write_u32(out, *count);
        }
        Instr::TupleGet(index) => {
            out.push(OP_TUPLE_GET);
            write_u32(out, *index);
        }
        Instr::IsType(ty) => {
            out.push(OP_IS_TYPE);
            write_u32(out, *ty);
        }
        Instr::CastType(ty) => {
            out.push(OP_CAST_TYPE);
            write_u32(out, *ty);
        }
        Instr::ListNew { ty, count } => {
            out.push(OP_LIST_NEW);
            write_u32(out, *ty);
            write_u32(out, *count);
        }
        Instr::ListLen => out.push(OP_LIST_LEN),
        Instr::ListAt => out.push(OP_LIST_AT),
        Instr::ListPush => out.push(OP_LIST_PUSH),
        Instr::MapNew { ty, count } => {
            out.push(OP_MAP_NEW);
            write_u32(out, *ty);
            write_u32(out, *count);
        }
        Instr::MapLen => out.push(OP_MAP_LEN),
        Instr::MapHas => out.push(OP_MAP_HAS),
        Instr::MapAt => out.push(OP_MAP_AT),
        Instr::MapPut { ty, discard } => {
            out.push(OP_MAP_PUT);
            write_u32(out, *ty);
            out.push(u8::from(*discard));
        }
        Instr::Native(NativeInstr::SbNew) => out.push(OP_SB_NEW),
        Instr::Native(NativeInstr::SbAppendStr) => out.push(OP_SB_APPEND_STR),
        Instr::Native(NativeInstr::SbAppendInt) => out.push(OP_SB_APPEND_INT),
        Instr::Native(NativeInstr::SbAppendBool) => out.push(OP_SB_APPEND_BOOL),
        Instr::Native(NativeInstr::SbBuild) => out.push(OP_SB_BUILD),
        Instr::Native(NativeInstr::SbLen) => out.push(OP_SB_LEN),
        Instr::Native(NativeInstr::SbClear) => out.push(OP_SB_CLEAR),
        Instr::Native(NativeInstr::BbNew) => out.push(OP_BB_NEW),
        Instr::Native(NativeInstr::BbAppend) => out.push(OP_BB_APPEND),
        Instr::Native(NativeInstr::BbLen) => out.push(OP_BB_LEN),
        Instr::Native(NativeInstr::BbBuild) => out.push(OP_BB_BUILD),
        Instr::Native(NativeInstr::SbAppendChar) => out.push(OP_SB_APPEND_CHAR),
        Instr::Native(NativeInstr::SbByteLen) => out.push(OP_SB_BYTE_LEN),
        Instr::Native(NativeInstr::SbFinish) => out.push(OP_SB_FINISH),
        Instr::Native(NativeInstr::BbFinish) => out.push(OP_BB_FINISH),
        Instr::Native(NativeInstr::BytesCompact) => out.push(OP_BYTES_COMPACT),
        Instr::Native(NativeInstr::BytesTextView) => out.push(OP_BYTES_TEXT_VIEW),
        Instr::Native(NativeInstr::TextHash) => out.push(OP_TEXT_HASH),
        Instr::Native(NativeInstr::BytesHash) => out.push(OP_BYTES_HASH),
        Instr::Native(NativeInstr::HashCombine) => out.push(OP_HASH_COMBINE),
        Instr::Native(NativeInstr::HashUnorderedCombine) => out.push(OP_HASH_UNORDERED_COMBINE),
        Instr::Native(
            extended @ (NativeInstr::RegexCompileStatus
            | NativeInstr::RegexCompileValue
            | NativeInstr::RegexSource
            | NativeInstr::RegexIsMatch
            | NativeInstr::RegexCount
            | NativeInstr::RegexSplit
            | NativeInstr::RegexReplaceAll
            | NativeInstr::RegexMatchStart
            | NativeInstr::RegexMatchEnd
            | NativeInstr::RegexMatchText
            | NativeInstr::RegexMatchGroupCount
            | NativeInstr::BbSet
            | NativeInstr::BbCapacity
            | NativeInstr::BbTruncate
            | NativeInstr::BytesReadU32Be
            | NativeInstr::BytesReadU32Le),
        ) => {
            out.push(OP_EXTENSION);
            out.push(native_extension_tag(*extended).expect("an extended instruction has one tag"));
        }
        Instr::Native(NativeInstr::LtBytes) => out.push(OP_LT_BYTES),
        Instr::Native(NativeInstr::LeBytes) => out.push(OP_LE_BYTES),
        Instr::Native(NativeInstr::GtBytes) => out.push(OP_GT_BYTES),
        Instr::Native(NativeInstr::GeBytes) => out.push(OP_GE_BYTES),
        Instr::Native(NativeInstr::BbExtend) => out.push(OP_BB_EXTEND),
        Instr::Native(NativeInstr::BbReserve) => out.push(OP_BB_RESERVE),
        Instr::Native(NativeInstr::BbClear) => out.push(OP_BB_CLEAR),
        Instr::Native(NativeInstr::BbAt) => out.push(OP_BB_AT),
        Instr::Native(NativeInstr::BbFindFrom) => out.push(OP_BB_FIND_FROM),
        Instr::Native(NativeInstr::BytesNew) => out.push(OP_BYTES_NEW),
        Instr::Native(NativeInstr::BytesLen) => out.push(OP_BYTES_LEN),
        Instr::Native(NativeInstr::BytesText) => out.push(OP_BYTES_TEXT),
        Instr::Native(NativeInstr::BytesTextRange) => {
            out.push(OP_EXTENSION);
            out.push(EXT_BYTES_TEXT_RANGE);
        }
        Instr::Native(NativeInstr::BytesAt) => out.push(OP_BYTES_AT),
        Instr::Native(NativeInstr::BytesGet) => out.push(OP_BYTES_GET),
        Instr::Native(NativeInstr::BytesSlice) => out.push(OP_BYTES_SLICE),
        Instr::Native(NativeInstr::BytesConcat) => out.push(OP_BYTES_CONCAT),
        Instr::Native(NativeInstr::BytesStartsWith) => out.push(OP_BYTES_STARTS_WITH),
        Instr::Native(NativeInstr::BytesFindIndex) => out.push(OP_BYTES_FIND_INDEX),
        Instr::Native(NativeInstr::BytesHex) => out.push(OP_BYTES_HEX),
        Instr::Native(NativeInstr::BytesIsUtf8) => out.push(OP_BYTES_IS_UTF8),
        Instr::Native(NativeInstr::EqBytes) => out.push(OP_EQ_BYTES),
        Instr::Native(NativeInstr::NeBytes) => out.push(OP_NE_BYTES),
        Instr::Freeze => out.push(OP_FREEZE),
        Instr::Digest { ty } => {
            out.push(OP_DIGEST);
            write_u32(out, *ty);
        }
        Instr::EqDigest => out.push(OP_EQ_DIGEST),
        Instr::NeDigest => out.push(OP_NE_DIGEST),
        Instr::Jump(block) => {
            out.push(OP_JUMP);
            write_u32(out, *block);
        }
        Instr::JumpIfFalse(block) => {
            out.push(OP_JUMP_IF_FALSE);
            write_u32(out, *block);
        }
        Instr::JumpIfTrue(block) => {
            out.push(OP_JUMP_IF_TRUE);
            write_u32(out, *block);
        }
        Instr::Return => out.push(OP_RETURN),
        Instr::Perform { op, argc, reply_ty } => {
            out.push(OP_PERFORM);
            write_u32(out, *op);
            write_u32(out, *argc);
            write_u32(out, *reply_ty);
        }
        Instr::PerformValue { argc, reply_ty } => {
            out.push(OP_PERFORM_VALUE);
            write_u32(out, *argc);
            write_u32(out, *reply_ty);
        }
        Instr::OpConst(op) => {
            out.push(OP_OP_CONST);
            write_u32(out, *op);
        }
        Instr::TableEdit { action, kind, slot } => {
            out.push(OP_TABLE_EDIT);
            write_u32(out, *action);
            write_u32(out, *kind);
            write_u32(out, *slot);
        }
        Instr::AsCall { op, ty } => {
            out.push(OP_AS_CALL);
            write_u32(out, *op);
            write_u32(out, *ty);
        }
        Instr::CallArgs => out.push(OP_CALL_ARGS),
        Instr::FaultCode => out.push(OP_FAULT_CODE),
        Instr::FaultDenied => out.push(OP_FAULT_DENIED),
        Instr::RaiseUserPanic => out.push(OP_RAISE_USER_PANIC),
        Instr::RaiseAssertionFailed => out.push(OP_RAISE_ASSERTION_FAILED),
        Instr::RaiseFault => out.push(OP_RAISE_FAULT),
        Instr::RequestOp => out.push(OP_REQUEST_OP),
        Instr::Unreachable => out.push(OP_UNREACHABLE),
    }
}

fn encode_extended(out: &mut Vec<u8>, instr: ExtendedInstr) {
    match instr {
        ExtendedInstr::PrepareWait { op_argc, reply_ty } => {
            let (op, argc) = ExtendedInstr::wait_parts(op_argc);
            out.push(OP_PREPARE_WAIT);
            write_u32(out, op);
            write_u32(out, argc);
            write_u32(out, reply_ty);
        }
        ExtendedInstr::RegexCaptures { ty } => {
            out.push(OP_EXTENSION);
            out.push(EXT_REGEX_CAPTURES);
            write_u32(out, ty);
        }
        ExtendedInstr::RegexMatchGroup { ty } => {
            out.push(OP_EXTENSION);
            out.push(EXT_REGEX_MATCH_GROUP);
            write_u32(out, ty);
        }
        ExtendedInstr::RegexMatchNamed { ty } => {
            out.push(OP_EXTENSION);
            out.push(EXT_REGEX_MATCH_NAMED);
            write_u32(out, ty);
        }
        ExtendedInstr::MakeCallback { func, captures } => {
            out.push(OP_MAKE_CALLBACK);
            write_u32(out, func);
            write_u32(out, captures);
        }
        ExtendedInstr::AsCallback => out.push(OP_AS_CALLBACK),
        ExtendedInstr::OptionSome { ty } => {
            out.push(OP_OPTION_SOME);
            write_u32(out, ty);
        }
        ExtendedInstr::OptionNone { ty } => {
            out.push(OP_OPTION_NONE);
            write_u32(out, ty);
        }
        ExtendedInstr::OptionPayload { ty } => {
            out.push(OP_OPTION_PAYLOAD);
            write_u32(out, ty);
        }
        ExtendedInstr::ListGet { ty } => {
            out.push(OP_LIST_GET);
            write_u32(out, ty);
        }
        ExtendedInstr::MapGet { ty } => {
            out.push(OP_MAP_GET);
            write_u32(out, ty);
        }
        ExtendedInstr::MapPutText { ty, discard } => {
            out.push(OP_EXTENSION);
            out.push(EXT_MAP_PUT_TEXT);
            write_u32(out, ty);
            out.push(u8::from(discard));
        }
        ExtendedInstr::MapInternTextRange => {
            out.push(OP_EXTENSION);
            out.push(EXT_MAP_INTERN_TEXT_RANGE);
        }
        ExtendedInstr::ListEpoch => out.push(OP_LIST_EPOCH),
        ExtendedInstr::ListIterLen => out.push(OP_LIST_ITER_LEN),
        ExtendedInstr::MapEpoch => out.push(OP_MAP_EPOCH),
        ExtendedInstr::MapIterLen => out.push(OP_MAP_ITER_LEN),
        ExtendedInstr::MapNextIndex => out.push(OP_MAP_NEXT_INDEX),
        ExtendedInstr::SealInstance => out.push(OP_SEAL_INSTANCE),
        ExtendedInstr::MapKeyAt => out.push(OP_MAP_KEY_AT),
        ExtendedInstr::MapValueAt => out.push(OP_MAP_VALUE_AT),
        ExtendedInstr::ListCapacity => out.push(OP_LIST_CAPACITY),
        ExtendedInstr::ListSet => out.push(OP_LIST_SET),
        ExtendedInstr::ListPop { ty } => {
            out.push(OP_LIST_POP);
            write_u32(out, ty);
        }
        ExtendedInstr::ListInsert => out.push(OP_LIST_INSERT),
        ExtendedInstr::ListRemove => out.push(OP_LIST_REMOVE),
        ExtendedInstr::ListSwapRemove => out.push(OP_LIST_SWAP_REMOVE),
        ExtendedInstr::ListSwap => {
            out.push(OP_EXTENSION);
            out.push(EXT_LIST_SWAP);
        }
        ExtendedInstr::ListReserve => out.push(OP_LIST_RESERVE),
        ExtendedInstr::ListTruncate => out.push(OP_LIST_TRUNCATE),
        ExtendedInstr::ListContains => out.push(OP_LIST_CONTAINS),
        ExtendedInstr::ListReorder => out.push(OP_LIST_REORDER),
        ExtendedInstr::MapRemove { ty } => {
            out.push(OP_MAP_REMOVE);
            write_u32(out, ty);
        }
        ExtendedInstr::MapClear => out.push(OP_MAP_CLEAR),
        ExtendedInstr::MapReserve => out.push(OP_MAP_RESERVE),
        ExtendedInstr::CallSlot { slot, app } => {
            out.push(OP_CALL_SLOT);
            write_u32(out, slot);
            encode_optional_index(out, app);
        }
        ExtendedInstr::NewSlot { slot, app } => {
            out.push(OP_NEW_SLOT);
            write_u32(out, slot);
            encode_optional_index(out, app);
        }
        ExtendedInstr::LoadSlot { slot } => {
            out.push(OP_LOAD_SLOT);
            write_u32(out, slot);
        }
        ExtendedInstr::SendSlot { slot } => {
            out.push(OP_SEND_SLOT);
            write_u32(out, slot);
        }
        ExtendedInstr::SyntaxTreeRoot => out.push(OP_SYNTAX_TREE_ROOT),
        ExtendedInstr::SyntaxKind => out.push(OP_SYNTAX_KIND),
        ExtendedInstr::SyntaxCategory => out.push(OP_SYNTAX_CATEGORY),
        ExtendedInstr::SyntaxRangeStart => out.push(OP_SYNTAX_RANGE_START),
        ExtendedInstr::SyntaxRangeEnd => out.push(OP_SYNTAX_RANGE_END),
        ExtendedInstr::SyntaxText => out.push(OP_SYNTAX_TEXT),
        ExtendedInstr::SyntaxChildren => out.push(OP_SYNTAX_CHILDREN),
        ExtendedInstr::SyntaxDetach => out.push(OP_SYNTAX_DETACH),
        ExtendedInstr::DynPack { ty } => {
            out.push(OP_DYN_PACK);
            write_u32(out, ty);
        }
        ExtendedInstr::DynRender => out.push(OP_DYN_RENDER),
        ExtendedInstr::SyntaxBuildToken => out.push(OP_SYNTAX_BUILD_TOKEN),
        ExtendedInstr::SyntaxBuildTrivia => out.push(OP_SYNTAX_BUILD_TRIVIA),
        ExtendedInstr::SyntaxBuildNode => out.push(OP_SYNTAX_BUILD_NODE),
        ExtendedInstr::SyntaxToTree => out.push(OP_SYNTAX_TO_TREE),
        ExtendedInstr::FunctionCode { func } => {
            out.push(OP_FUNCTION_CODE);
            write_u32(out, func);
        }
        ExtendedInstr::ClassCode { class } => {
            out.push(OP_CLASS_CODE);
            write_u32(out, class);
        }
        ExtendedInstr::ModuleCode { module } => {
            out.push(OP_EXTENSION);
            out.push(EXT_MODULE_CODE);
            write_u32(out, module);
        }
        ExtendedInstr::ReflectionDeclarations => {
            out.push(OP_EXTENSION);
            out.push(EXT_REFLECTION_DECLARATIONS);
        }
        ExtendedInstr::ReflectionMembers => {
            out.push(OP_EXTENSION);
            out.push(EXT_REFLECTION_MEMBERS);
        }
        ExtendedInstr::ReflectionName => {
            out.push(OP_EXTENSION);
            out.push(EXT_REFLECTION_NAME);
        }
        ExtendedInstr::ReflectionDeclarationKind => {
            out.push(OP_EXTENSION);
            out.push(EXT_REFLECTION_DECLARATION_KIND);
        }
        ExtendedInstr::ReflectionMemberKind => {
            out.push(OP_EXTENSION);
            out.push(EXT_REFLECTION_MEMBER_KIND);
        }
        ExtendedInstr::CodeSource { ty } => {
            out.push(OP_CODE_SOURCE);
            write_u32(out, ty);
        }
        ExtendedInstr::CodeDefinition => out.push(OP_CODE_DEFINITION),
        ExtendedInstr::FaultSite { ty } => {
            out.push(OP_FAULT_SITE);
            write_u32(out, ty);
        }
        ExtendedInstr::FaultTrace { ty } => {
            out.push(OP_FAULT_TRACE);
            write_u32(out, ty);
        }
        ExtendedInstr::MapProbe => out.push(OP_MAP_PROBE),
        ExtendedInstr::MapProbeFound => out.push(OP_MAP_PROBE_FOUND),
        ExtendedInstr::MapProbeKey => out.push(OP_MAP_PROBE_KEY),
        ExtendedInstr::MapProbeValue => out.push(OP_MAP_PROBE_VALUE),
        ExtendedInstr::MapProbeSetValue => out.push(OP_MAP_PROBE_SET_VALUE),
        ExtendedInstr::MapProbeRemove => out.push(OP_MAP_PROBE_REMOVE),
        ExtendedInstr::MapInsertHashed => out.push(OP_MAP_INSERT_HASHED),
        ExtendedInstr::MapWriteGuard => out.push(OP_MAP_WRITE_GUARD),
    }
}

fn encode_optional_index(out: &mut Vec<u8>, value: u32) {
    if value == NO_APP {
        out.push(0);
    } else {
        out.push(1);
        write_u32(out, value);
    }
}

/// A structural decode failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The stream ended before the structure was complete.
    Truncated,
    BadMagic,
    BadVersion(u16),
    /// The artifact names another ABI bundle.
    BadBundle {
        expected: [u8; 32],
        found: [u8; 32],
    },
    BadOpcode(u8),
    BadTypeTag(u8),
    BadRowTag(u8),
    BadClassKind(u8),
    BadSlot,
    /// A `mut` flag byte is not 0 or 1.
    BadFlag(u8),
    BadUtf8,
    /// A character value is not one Unicode scalar value.
    BadCharacter,
    /// A table length field is larger than the remaining input allows.
    BadLength,
    /// Extra bytes follow the encoded module.
    TrailingBytes,
    /// The section table does not describe the input exactly: a wrong
    /// offset, an overlap, a gap, or a size past the input end.
    BadSectionTable,
    /// The export-section name counts do not equal the semantic
    /// definition counts.
    ExportCountMismatch,
    /// An export entry names an unknown kind or an index outside the
    /// definition tables.
    BadExport,
    /// A function binding names a function outside the function table.
    BadBinding,
    /// An import slot names an index outside the definition tables.
    BadImport,
    /// A `mut` marker vector does not hold one marker per parameter.
    MutMarkerCount,
    /// A core role slot names a class outside the table, or two roles
    /// name one class.
    BadCoreRole,
    /// The optional debug section is malformed.
    BadDebug(debug::DebugError),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Truncated => write!(f, "the byte stream is truncated"),
            DecodeError::BadMagic => write!(f, "the magic header is not `LMBC`"),
            DecodeError::BadVersion(v) => write!(f, "unsupported bytecode version {v}"),
            DecodeError::BadBundle { expected, found } => write!(
                f,
                "the artifact uses ABI bundle {}, but this loader uses {}",
                digest_text(found),
                digest_text(expected)
            ),
            DecodeError::BadOpcode(op) => write!(f, "unknown opcode byte 0x{op:02x}"),
            DecodeError::BadTypeTag(t) => write!(f, "unknown type tag {t}"),
            DecodeError::BadRowTag(t) => write!(f, "unknown row element tag {t}"),
            DecodeError::BadClassKind(t) => write!(f, "unknown class kind tag {t}"),
            DecodeError::BadSlot => write!(f, "a slot declaration is invalid"),
            DecodeError::BadFlag(v) => write!(f, "invalid flag byte {v}"),
            DecodeError::BadUtf8 => write!(f, "a string is not valid UTF-8"),
            DecodeError::BadCharacter => write!(f, "a character is not a Unicode scalar value"),
            DecodeError::BadLength => write!(f, "a length field exceeds the input size"),
            DecodeError::TrailingBytes => write!(f, "extra bytes follow the module"),
            DecodeError::BadSectionTable => {
                write!(f, "the section table does not describe the input exactly")
            }
            DecodeError::ExportCountMismatch => {
                write!(f, "the export names do not match the definition counts")
            }
            DecodeError::BadExport => write!(f, "an export entry is out of range"),
            DecodeError::BadBinding => {
                write!(f, "a function binding names a function out of range")
            }
            DecodeError::BadImport => write!(f, "an import slot is out of range"),
            DecodeError::MutMarkerCount => {
                write!(f, "a mut marker vector does not match its parameter count")
            }
            DecodeError::BadCoreRole => write!(f, "a core role slot is out of range"),
            DecodeError::BadDebug(error) => write!(f, "the debug section is invalid: {error}"),
        }
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

const MAX_DECODE_VECTOR_BYTES: usize = 64 * 1024 * 1024;

/// Reserve one decoded vector without invoking the infallible allocator.
fn decode_vec<T>(count: usize) -> Result<Vec<T>, DecodeError> {
    let bytes = count
        .checked_mul(std::mem::size_of::<T>().max(1))
        .ok_or(DecodeError::BadLength)?;
    if bytes > MAX_DECODE_VECTOR_BYTES {
        return Err(DecodeError::BadLength);
    }
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| DecodeError::BadLength)?;
    Ok(values)
}

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(len).ok_or(DecodeError::Truncated)?;
        if end > self.bytes.len() {
            return Err(DecodeError::Truncated);
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// The bytes left to read.
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    /// Read a table length and reject counts the input cannot contain.
    /// Each counted element needs at least one byte. A table whose
    /// element is larger must check `remaining` against the real cost
    /// before it sizes an allocation.
    fn len(&mut self) -> Result<usize, DecodeError> {
        let count = self.u32()? as usize;
        if count > self.bytes.len().saturating_sub(self.pos) {
            return Err(DecodeError::BadLength);
        }
        Ok(count)
    }

    fn i64(&mut self) -> Result<i64, DecodeError> {
        let b = self.take(8)?;
        let mut buf = [0u8; 8];
        buf.copy_from_slice(b);
        Ok(i64::from_le_bytes(buf))
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        let b = self.take(8)?;
        let mut buf = [0u8; 8];
        buf.copy_from_slice(b);
        Ok(u64::from_le_bytes(buf))
    }

    fn string(&mut self) -> Result<String, DecodeError> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::BadUtf8)
    }

    /// Read one `mut` flag byte. Only 0 and 1 are valid.
    fn flag(&mut self) -> Result<bool, DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(DecodeError::BadFlag(other)),
        }
    }
}

fn decode_row(cur: &mut Cursor<'_>) -> Result<Vec<BcRow>, DecodeError> {
    let count = cur.len()?;
    let mut row = decode_vec(count)?;
    for _ in 0..count {
        let tag = cur.u8()?;
        let elem = match tag {
            ROW_OP => BcRow::Op(cur.u32()?),
            ROW_GROUP => BcRow::Group(cur.u32()?),
            ROW_VAR => BcRow::Var(cur.u32()?),
            other => return Err(DecodeError::BadRowTag(other)),
        };
        row.push(elem);
    }
    Ok(row)
}

fn decode_callable_contract(cur: &mut Cursor<'_>) -> Result<BcCallableContract, DecodeError> {
    let type_params = cur.u32()?;
    let effect_params = cur.u32()?;
    let type_bounds = decode_type_bounds(cur)?;
    let count = cur.len()?;
    if count > cur.remaining() / 4 {
        return Err(DecodeError::BadLength);
    }
    let mut params = decode_vec(count)?;
    for _ in 0..count {
        params.push(cur.u32()?);
    }
    let marker_count = cur.len()?;
    if marker_count != count {
        return Err(DecodeError::MutMarkerCount);
    }
    let mut param_muts = decode_vec(marker_count)?;
    for _ in 0..marker_count {
        param_muts.push(cur.flag()?);
    }
    let ret = cur.u32()?;
    let row = decode_row(cur)?;
    Ok(BcCallableContract {
        type_params,
        effect_params,
        type_bounds,
        params,
        param_muts,
        ret,
        row,
    })
}

fn decode_slot_contract(cur: &mut Cursor<'_>) -> Result<SlotContract, DecodeError> {
    Ok(match cur.u8()? {
        SLOT_FUNCTION => SlotContract::Function(decode_callable_contract(cur)?),
        SLOT_METHOD => SlotContract::Method(decode_callable_contract(cur)?),
        SLOT_CLASS => {
            let type_params = cur.u32()?;
            let mut abi = [0u8; 32];
            abi.copy_from_slice(cur.take(32)?);
            SlotContract::Class {
                type_params,
                abi,
                ty: cur.u32()?,
                constructor: decode_callable_contract(cur)?,
            }
        }
        SLOT_VALUE => SlotContract::Value { ty: cur.u32()? },
        SLOT_PROCESS => SlotContract::Process {
            message: cur.u32()?,
            result: cur.u32()?,
        },
        _ => return Err(DecodeError::BadSlot),
    })
}

fn decode_interface_use(cur: &mut Cursor<'_>) -> Result<BcInterfaceUse, DecodeError> {
    let interface = cur.u32()?;
    let type_count = cur.len()?;
    if type_count > cur.remaining() / 4 {
        return Err(DecodeError::BadLength);
    }
    let mut types = decode_vec(type_count)?;
    for _ in 0..type_count {
        types.push(cur.u32()?);
    }
    let row_count = cur.len()?;
    let mut rows = decode_vec(row_count)?;
    for _ in 0..row_count {
        rows.push(decode_row(cur)?);
    }
    Ok(BcInterfaceUse {
        interface,
        types,
        rows,
    })
}

fn decode_type_bounds(cur: &mut Cursor<'_>) -> Result<Vec<Vec<BcInterfaceUse>>, DecodeError> {
    let parameter_count = cur.len()?;
    let mut bounds = decode_vec(parameter_count)?;
    for _ in 0..parameter_count {
        let bound_count = cur.len()?;
        let mut parameter = decode_vec(bound_count)?;
        for _ in 0..bound_count {
            parameter.push(decode_interface_use(cur)?);
        }
        bounds.push(parameter);
    }
    Ok(bounds)
}

/// Decode a serialized container. This checks structure only.
///
/// The section table is validated with plain arithmetic before any
/// section is read, so a claimed size that disagrees with the actual
/// byte count rejects before any allocation is sized from it.
pub fn decode(bytes: &[u8]) -> Result<Module, DecodeError> {
    decode_with_bundle(bytes, &lm_abi::standard_bundle())
}

/// Decode a serialized container for one exact ABI bundle.
pub fn decode_with_bundle(bytes: &[u8], bundle: &lm_abi::AbiBundle) -> Result<Module, DecodeError> {
    let mut cur = Cursor { bytes, pos: 0 };
    if cur.take(4)? != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let version = cur.u16()?;
    if version != VERSION {
        return Err(DecodeError::BadVersion(version));
    }
    let mut found = [0u8; 32];
    found.copy_from_slice(cur.take(32)?);
    let expected = bundle.digest();
    if found != expected {
        return Err(DecodeError::BadBundle { expected, found });
    }
    // Read the section table: three (offset, length) pairs. The
    // sections must be contiguous, in order, and cover the input
    // exactly.
    let mut sections = [(0usize, 0usize); 3];
    let mut expected = HEADER_LEN as u64;
    for slot in &mut sections {
        let offset = cur.u32()? as u64;
        let len = cur.u32()? as u64;
        if offset != expected {
            return Err(DecodeError::BadSectionTable);
        }
        expected += len;
        if expected > bytes.len() as u64 {
            return Err(DecodeError::BadSectionTable);
        }
        *slot = (offset as usize, len as usize);
    }
    if expected != bytes.len() as u64 {
        return Err(DecodeError::BadSectionTable);
    }
    let (sem_at, sem_len) = sections[0];
    let (exp_at, exp_len) = sections[1];
    let (debug_at, debug_len) = sections[2];
    let mut module = decode_semantic(&bytes[sem_at..sem_at + sem_len])?;
    decode_exports(&bytes[exp_at..exp_at + exp_len], &mut module)?;
    let debug_bytes = &bytes[debug_at..debug_at + debug_len];
    let info = debug::decode(debug_bytes).map_err(DecodeError::BadDebug)?;
    debug::validate(&info, &module).map_err(DecodeError::BadDebug)?;
    if debug::encode(&info) != debug_bytes {
        return Err(DecodeError::BadDebug(debug::DebugError::NonCanonical));
    }
    module.debug = debug_bytes.to_vec();
    Ok(module)
}

fn digest_text(digest: &[u8; 32]) -> String {
    let mut text = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Decode the export section: the definition names, the function
/// bindings, and the exported top-level definitions. Every index is
/// checked against the tables the semantic region already produced.
fn decode_exports(bytes: &[u8], module: &mut Module) -> Result<(), DecodeError> {
    let mut cur = Cursor { bytes, pos: 0 };
    let interface_count = cur.len()?;
    if interface_count != module.interfaces.len() {
        return Err(DecodeError::ExportCountMismatch);
    }
    for interface in &mut module.interfaces {
        interface.name = cur.string()?;
        interface.key = cur.string()?;
        for method in &mut interface.methods {
            let name_count = cur.len()?;
            if name_count != method.params.len() {
                return Err(DecodeError::ExportCountMismatch);
            }
            let mut names = decode_vec(name_count)?;
            for _ in 0..name_count {
                names.push(cur.string()?);
            }
            method.param_names = names;
        }
    }
    let class_count = cur.len()?;
    if class_count != module.classes.len() {
        return Err(DecodeError::ExportCountMismatch);
    }
    for class in &mut module.classes {
        class.name = cur.string()?;
        class.key = cur.string()?;
        let default_count = cur.len()?;
        if default_count != class.fields.len() {
            return Err(DecodeError::ExportCountMismatch);
        }
        let mut defaults = decode_vec(default_count)?;
        for _ in 0..default_count {
            defaults.push(cur.flag()?);
        }
        class.field_defaults = defaults;
        class.own_start = cur.u32()?;
        class.has_init = cur.flag()?;
    }
    let func_count = cur.len()?;
    if func_count != module.funcs.len() {
        return Err(DecodeError::ExportCountMismatch);
    }
    for func in &mut module.funcs {
        func.name = cur.string()?;
        let name_count = cur.len()?;
        if name_count != 0 && name_count != func.params.len() {
            return Err(DecodeError::ExportCountMismatch);
        }
        let mut names = decode_vec(name_count)?;
        for _ in 0..name_count {
            names.push(cur.string()?);
        }
        func.param_names = names;
    }
    let slot_count = cur.len()?;
    if slot_count != module.slots.len() {
        return Err(DecodeError::ExportCountMismatch);
    }
    for slot in &mut module.slots {
        slot.binding = cur.string()?;
        slot.late = cur.flag()?;
    }
    // One encoded binding needs at least twelve bytes: the key
    // length, the function index, and the class index. `len` bounds a
    // count at one byte per entry, which is not enough to size this
    // allocation. Check the real cost before the reserve.
    let binding_count = cur.len()?;
    if binding_count > cur.remaining() / 12 {
        return Err(DecodeError::BadLength);
    }
    let mut bindings = decode_vec(binding_count)?;
    for _ in 0..binding_count {
        let key = cur.string()?;
        let func = cur.u32()?;
        let class = cur.u32()?;
        if func as usize >= module.funcs.len() {
            return Err(DecodeError::BadBinding);
        }
        if class != NO_CLASS && class as usize >= module.classes.len() {
            return Err(DecodeError::BadBinding);
        }
        bindings.push(FuncBinding { key, func, class });
    }
    module.bindings = bindings;
    let export_count = cur.len()?;
    let mut exports = decode_vec(export_count)?;
    let mut const_allocation = 0usize;
    for _ in 0..export_count {
        let kind = ExportKind::from_tag(cur.u8()?).ok_or(DecodeError::BadExport)?;
        let name = cur.string()?;
        let source = cur.flag()?;
        let def = cur.u32()?;
        let ctor = cur.u32()?;
        let constant = match cur.u8()? {
            0 => None,
            1 => {
                let ty = cur.u32()?;
                if ty as usize >= module.types.len() {
                    return Err(DecodeError::BadExport);
                }
                Some(Constant {
                    ty,
                    value: interface::decode_const_value(&mut cur, 0, &mut const_allocation)?,
                })
            }
            _ => return Err(DecodeError::BadExport),
        };
        if kind.is_constant() != constant.is_some() {
            return Err(DecodeError::BadExport);
        }
        if kind.is_constant() {
            if def != NO_CTOR || ctor != NO_CTOR {
                return Err(DecodeError::BadExport);
            }
            exports.push(Export {
                kind,
                name,
                source,
                def,
                ctor,
                constant,
            });
            continue;
        }
        let limit = if kind.is_class() {
            module.classes.len()
        } else if kind.is_interface() {
            module.interfaces.len()
        } else {
            module.funcs.len()
        };
        if def as usize >= limit {
            return Err(DecodeError::BadExport);
        }
        if ctor != NO_CTOR && ctor as usize >= module.funcs.len() {
            return Err(DecodeError::BadExport);
        }
        if !kind.is_class() && ctor != NO_CTOR {
            return Err(DecodeError::BadExport);
        }
        exports.push(Export {
            kind,
            name,
            source,
            def,
            ctor,
            constant,
        });
    }
    module.exports = exports;
    if cur.pos != bytes.len() {
        return Err(DecodeError::TrailingBytes);
    }
    Ok(())
}

/// Decode the semantic region. The names stay empty; the export
/// section fills them.
fn decode_semantic(bytes: &[u8]) -> Result<Module, DecodeError> {
    let mut cur = Cursor { bytes, pos: 0 };
    let string_count = cur.len()?;
    let mut strings = decode_vec(string_count)?;
    for _ in 0..string_count {
        strings.push(cur.string()?);
    }
    let byte_count = cur.len()?;
    if byte_count > cur.remaining() / 4 {
        return Err(DecodeError::BadLength);
    }
    let mut literal_bytes = decode_vec(byte_count)?;
    for _ in 0..byte_count {
        let len = cur.u32()? as usize;
        literal_bytes.push(cur.take(len)?.to_vec());
    }
    let type_count = cur.len()?;
    let mut types = decode_vec(type_count)?;
    for _ in 0..type_count {
        types.push(decode_type(&mut cur)?);
    }
    let selector_count = cur.len()?;
    let mut selectors = decode_vec(selector_count)?;
    for _ in 0..selector_count {
        selectors.push(cur.string()?);
    }
    let app_count = cur.len()?;
    let mut apps = decode_vec(app_count)?;
    for _ in 0..app_count {
        let ty_count = cur.len()?;
        let mut app_types = decode_vec(ty_count)?;
        for _ in 0..ty_count {
            app_types.push(cur.u32()?);
        }
        let row_count = cur.len()?;
        let mut rows = decode_vec(row_count)?;
        for _ in 0..row_count {
            rows.push(decode_row(&mut cur)?);
        }
        apps.push(TypeApp {
            types: app_types,
            rows,
        });
    }
    let interface_count = cur.len()?;
    let mut interfaces = decode_vec(interface_count)?;
    for _ in 0..interface_count {
        let type_params = cur.u32()?;
        let effect_params = cur.u32()?;
        let generic_count = cur.len()?;
        let mut generic_is_effect = decode_vec(generic_count)?;
        for _ in 0..generic_count {
            generic_is_effect.push(cur.flag()?);
        }
        let parent_count = cur.len()?;
        let mut parents = decode_vec(parent_count)?;
        for _ in 0..parent_count {
            parents.push(decode_interface_use(&mut cur)?);
        }
        let type_bounds = decode_type_bounds(&mut cur)?;
        let associated_count = cur.len()?;
        let mut associated = decode_vec(associated_count)?;
        for _ in 0..associated_count {
            let name = cur.string()?;
            let bound_count = cur.len()?;
            if bound_count > cur.remaining() / 12 {
                return Err(DecodeError::BadLength);
            }
            let mut bounds = decode_vec(bound_count)?;
            for _ in 0..bound_count {
                bounds.push(decode_interface_use(&mut cur)?);
            }
            associated.push(BcAssociated { name, bounds });
        }
        let method_count = cur.len()?;
        let mut methods = decode_vec(method_count)?;
        for _ in 0..method_count {
            let selector = cur.u32()?;
            let mut_self = cur.flag()?;
            let type_params = cur.u32()?;
            let type_bounds = decode_type_bounds(&mut cur)?;
            let effect_params = cur.u32()?;
            let premise_count = cur.len()?;
            let mut premises = decode_vec(premise_count)?;
            for _ in 0..premise_count {
                let subject = cur.u32()?;
                let bound_count = cur.len()?;
                let mut bounds = decode_vec(bound_count)?;
                for _ in 0..bound_count {
                    bounds.push(decode_interface_use(&mut cur)?);
                }
                premises.push(BcTypePremise { subject, bounds });
            }
            let param_count = cur.len()?;
            if param_count > cur.remaining() / 4 {
                return Err(DecodeError::BadLength);
            }
            let mut params = decode_vec(param_count)?;
            for _ in 0..param_count {
                params.push(cur.u32()?);
            }
            let marker_count = cur.len()?;
            if marker_count != param_count {
                return Err(DecodeError::MutMarkerCount);
            }
            let mut param_muts = decode_vec(marker_count)?;
            for _ in 0..marker_count {
                param_muts.push(cur.flag()?);
            }
            let ret = cur.u32()?;
            let row = decode_row(&mut cur)?;
            let default = cur.u32()?;
            methods.push(BcInterfaceMethod {
                selector,
                mut_self,
                type_params,
                type_bounds,
                effect_params,
                premises,
                params,
                param_muts,
                param_names: Vec::new(),
                ret,
                row,
                default,
            });
        }
        interfaces.push(BcInterface {
            name: String::new(),
            key: String::new(),
            type_params,
            effect_params,
            generic_is_effect,
            parents,
            type_bounds,
            associated,
            methods,
        });
    }
    let conformance_count = cur.len()?;
    let mut conformances = decode_vec(conformance_count)?;
    for _ in 0..conformance_count {
        let class = cur.u32()?;
        let application = decode_interface_use(&mut cur)?;
        let premise_count = cur.len()?;
        if premise_count > cur.remaining() / 8 {
            return Err(DecodeError::BadLength);
        }
        let mut premises = decode_vec(premise_count)?;
        for _ in 0..premise_count {
            let param = cur.u32()?;
            let bound_count = cur.len()?;
            if bound_count > cur.remaining() / 12 {
                return Err(DecodeError::BadLength);
            }
            let mut bounds = decode_vec(bound_count)?;
            for _ in 0..bound_count {
                bounds.push(decode_interface_use(&mut cur)?);
            }
            premises.push(BcConformancePremise { param, bounds });
        }
        let associated_count = cur.len()?;
        if associated_count > cur.remaining() / 4 {
            return Err(DecodeError::BadLength);
        }
        let mut associated = decode_vec(associated_count)?;
        for _ in 0..associated_count {
            associated.push(cur.u32()?);
        }
        let method_count = cur.len()?;
        if method_count > cur.remaining() {
            return Err(DecodeError::BadLength);
        }
        let mut method_overrides = decode_vec(method_count)?;
        for _ in 0..method_count {
            method_overrides.push(cur.flag()?);
        }
        conformances.push(BcConformance {
            class,
            application,
            premises,
            associated,
            method_overrides,
        });
    }
    let class_bound_count = cur.len()?;
    let mut class_bounds = decode_vec(class_bound_count)?;
    for _ in 0..class_bound_count {
        class_bounds.push(decode_type_bounds(&mut cur)?);
    }
    let function_bound_count = cur.len()?;
    let mut func_bounds = decode_vec(function_bound_count)?;
    for _ in 0..function_bound_count {
        func_bounds.push(decode_type_bounds(&mut cur)?);
    }
    let import_count = cur.len()?;
    let mut imports = decode_vec(import_count)?;
    for _ in 0..import_count {
        let module_path = cur.string()?;
        let name = cur.string()?;
        let kind = match cur.u8()? {
            0 => ImportKind::Class,
            1 => ImportKind::Ctor,
            2 => ImportKind::Method,
            3 => ImportKind::Func,
            4 => ImportKind::Constant,
            _ => return Err(DecodeError::BadImport),
        };
        let def = cur.u32()?;
        let mut hash = [0u8; 32];
        hash.copy_from_slice(cur.take(32)?);
        imports.push(Import {
            module: module_path,
            name,
            kind,
            def,
            hash,
        });
    }
    let slot_count = cur.len()?;
    if slot_count > cur.remaining() / 66 {
        return Err(DecodeError::BadLength);
    }
    let mut slots = decode_vec(slot_count)?;
    for _ in 0..slot_count {
        let mut key = [0u8; 32];
        key.copy_from_slice(cur.take(32)?);
        let mut contract_hash = [0u8; 32];
        contract_hash.copy_from_slice(cur.take(32)?);
        let contract = decode_slot_contract(&mut cur)?;
        let initial = match cur.u8()? {
            0 => None,
            1 => Some(SlotTarget::Function(cur.u32()?)),
            2 => Some(SlotTarget::Class {
                class: cur.u32()?,
                constructor: cur.u32()?,
            }),
            _ => return Err(DecodeError::BadSlot),
        };
        slots.push(SlotSpec {
            binding: String::new(),
            late: false,
            key,
            contract_hash,
            contract,
            initial,
        });
    }
    let mut core_roles = [NO_ROLE; CORE_ROLE_COUNT];
    for slot in &mut core_roles {
        *slot = cur.u32()?;
    }
    let class_count = cur.len()?;
    let mut classes = decode_vec(class_count)?;
    for _ in 0..class_count {
        let parent = cur.u32()?;
        let parent_arg_count = cur.len()?;
        let mut parent_args = decode_vec(parent_arg_count)?;
        for _ in 0..parent_arg_count {
            parent_args.push(cur.u32()?);
        }
        let type_params = cur.u32()?;
        let kind = match cur.u8()? {
            KIND_NORMAL => BcClassKind::Normal,
            KIND_ABSTRACT => BcClassKind::Abstract,
            KIND_CASE => BcClassKind::Case,
            other => return Err(DecodeError::BadClassKind(other)),
        };
        let is_final = cur.flag()?;
        let is_frozen = cur.flag()?;
        let field_count = cur.len()?;
        let mut fields = decode_vec(field_count)?;
        for _ in 0..field_count {
            let fname = cur.string()?;
            let fty = cur.u32()?;
            fields.push((fname, fty));
        }
        let method_count = cur.len()?;
        let mut methods = decode_vec(method_count)?;
        for _ in 0..method_count {
            let sel = cur.u32()?;
            let func = cur.u32()?;
            methods.push((sel, func));
        }
        classes.push(BcClass {
            name: String::new(),
            key: String::new(),
            is_final,
            is_frozen,
            parent,
            parent_args,
            type_params,
            kind,
            fields,
            field_defaults: Vec::new(),
            own_start: 0,
            has_init: false,
            methods,
        });
    }
    let reflection_count = cur.len()?;
    let mut reflections = decode_vec(reflection_count)?;
    for _ in 0..reflection_count {
        let name = cur.string()?;
        let declaration_count = cur.len()?;
        let mut declarations = decode_vec(declaration_count)?;
        for _ in 0..declaration_count {
            let kind = ExportKind::from_tag(cur.u8()?).ok_or(DecodeError::BadExport)?;
            let name = cur.string()?;
            let def = cur.u32()?;
            let callable = cur.u32()?;
            declarations.push(ReflectionDeclaration {
                kind,
                name,
                def,
                callable,
            });
        }
        reflections.push(ReflectionModule { name, declarations });
    }
    let func_count = cur.len()?;
    let mut funcs = decode_vec(func_count)?;
    for _ in 0..func_count {
        let type_params = cur.u32()?;
        let effect_params = cur.u32()?;
        let param_count = cur.len()?;
        let mut params = decode_vec(param_count)?;
        for _ in 0..param_count {
            params.push(cur.u32()?);
        }
        // The marker vector carries its own count. The decoder forces
        // it equal to the parameter count, so no later reader must
        // guess how many markers the stream holds.
        let mut_count = cur.len()?;
        if mut_count != param_count {
            return Err(DecodeError::MutMarkerCount);
        }
        let mut param_muts = decode_vec(mut_count)?;
        for _ in 0..mut_count {
            param_muts.push(cur.flag()?);
        }
        let ret = cur.u32()?;
        let row = decode_row(&mut cur)?;
        let capture_count = cur.len()?;
        let mut captures = decode_vec(capture_count)?;
        for _ in 0..capture_count {
            captures.push(cur.u32()?);
        }
        // The local-type table count passes the length guard, so the
        // allocation is bounded by the input size.
        let local_count = cur.len()?;
        if local_count > cur.remaining() / 4 {
            return Err(DecodeError::BadLength);
        }
        let mut local_types = decode_vec(local_count)?;
        for _ in 0..local_count {
            local_types.push(cur.u32()?);
        }
        let block_count = cur.len()?;
        let mut blocks = decode_vec(block_count)?;
        for _ in 0..block_count {
            let instr_count = cur.len()?;
            let mut block = decode_vec(instr_count)?;
            for _ in 0..instr_count {
                block.push(decode_instr(&mut cur)?);
            }
            blocks.push(block);
        }
        funcs.push(Func {
            name: String::new(),
            type_params,
            effect_params,
            params,
            param_muts,
            param_names: Vec::new(),
            ret,
            row,
            captures,
            local_types,
            blocks,
        });
    }
    if class_bounds.len() != classes.len() || func_bounds.len() != funcs.len() {
        return Err(DecodeError::BadLength);
    }
    for reflection in &reflections {
        for declaration in &reflection.declarations {
            let valid = match declaration.kind {
                ExportKind::Function => {
                    (declaration.def as usize) < funcs.len()
                        && declaration.callable == declaration.def
                }
                ExportKind::Class | ExportKind::Enum => {
                    (declaration.def as usize) < classes.len()
                        && (declaration.callable == NO_REFLECTION_DEF
                            || (declaration.callable as usize) < funcs.len())
                }
                ExportKind::Interface => {
                    (declaration.def as usize) < interfaces.len()
                        && declaration.callable == NO_REFLECTION_DEF
                }
                ExportKind::Constant => {
                    declaration.def == NO_REFLECTION_DEF
                        && declaration.callable == NO_REFLECTION_DEF
                }
                ExportKind::EnumCase => false,
            };
            if !valid {
                return Err(DecodeError::BadExport);
            }
        }
    }
    let entry = cur.u32()?;
    if cur.pos != bytes.len() {
        return Err(DecodeError::TrailingBytes);
    }
    // Every import slot must name a definition of its own kind, and
    // each definition takes at most one slot. The check runs here, so
    // no later pass reads a slot index it did not validate.
    let mut claimed_classes = vec![false; classes.len()];
    let mut claimed_funcs = vec![false; funcs.len()];
    for import in &imports {
        if import.kind == ImportKind::Constant {
            if import.def != NO_IMPORT_DEF {
                return Err(DecodeError::BadImport);
            }
            continue;
        }
        let claimed = if import.kind.is_func() {
            &mut claimed_funcs
        } else {
            &mut claimed_classes
        };
        let idx = import.def as usize;
        if idx >= claimed.len() || claimed[idx] {
            return Err(DecodeError::BadImport);
        }
        claimed[idx] = true;
    }
    // Every filled core role slot names a class of this module, and no
    // two roles name one class. The verifier proves the shape; the
    // decoder proves the index.
    let mut taken: Vec<u32> = Vec::new();
    for slot in &core_roles {
        if *slot == NO_ROLE {
            continue;
        }
        if *slot as usize >= classes.len() || taken.contains(slot) {
            return Err(DecodeError::BadCoreRole);
        }
        taken.push(*slot);
    }
    Ok(Module {
        strings,
        bytes: literal_bytes,
        types,
        selectors,
        apps,
        interfaces,
        conformances,
        class_bounds,
        func_bounds,
        imports,
        slots,
        core_roles,
        reflections,
        classes,
        funcs,
        entry,
        exports: Vec::new(),
        bindings: Vec::new(),
        debug: Vec::new(),
    })
}

fn decode_type(cur: &mut Cursor<'_>) -> Result<BcType, DecodeError> {
    let tag = cur.u8()?;
    let ty = match tag {
        TY_UNIT => BcType::Unit,
        TY_NEVER => BcType::Never,
        TY_BOOL => BcType::Bool,
        TY_INT => BcType::Int,
        TY_FLOAT => BcType::Float,
        TY_STR => BcType::Str,
        TY_CLASS => BcType::Class(cur.u32()?),
        TY_INST => {
            let class = cur.u32()?;
            let count = cur.len()?;
            let mut args = decode_vec(count)?;
            for _ in 0..count {
                args.push(cur.u32()?);
            }
            BcType::Inst(class, args)
        }
        TY_LIST => BcType::List(cur.u32()?),
        TY_MAP => BcType::Map(cur.u32()?, cur.u32()?),
        TY_TUPLE => {
            let count = cur.len()?;
            let mut elems = decode_vec(count)?;
            for _ in 0..count {
                elems.push(cur.u32()?);
            }
            BcType::Tuple(elems)
        }
        TY_FN => {
            let count = cur.len()?;
            let mut params = decode_vec(count)?;
            for _ in 0..count {
                params.push(cur.u32()?);
            }
            let mut_count = cur.len()?;
            if mut_count != count {
                return Err(DecodeError::MutMarkerCount);
            }
            let mut muts = decode_vec(mut_count)?;
            for _ in 0..mut_count {
                muts.push(cur.flag()?);
            }
            let ret = cur.u32()?;
            let row = decode_row(cur)?;
            BcType::Fn(params, muts, ret, row)
        }
        TY_CALLBACK => {
            let count = cur.len()?;
            let mut params = decode_vec(count)?;
            for _ in 0..count {
                params.push(cur.u32()?);
            }
            let mut_count = cur.len()?;
            if mut_count != count {
                return Err(DecodeError::MutMarkerCount);
            }
            let mut muts = decode_vec(mut_count)?;
            for _ in 0..mut_count {
                muts.push(cur.flag()?);
            }
            let ret = cur.u32()?;
            let row = decode_row(cur)?;
            BcType::Callback(params, muts, ret, row)
        }
        TY_VAR => BcType::Var(cur.u32()?),
        TY_PROJECTION => BcType::Projection {
            base: cur.u32()?,
            interface: cur.u32()?,
            assoc: cur.u32()?,
        },
        TY_DIGEST => BcType::Digest,
        TY_FAULT => BcType::Fault,
        TY_REQUEST => BcType::Request,
        TY_POLICY_TABLE => BcType::PolicyTable,
        TY_VM => BcType::Vm,
        TY_RUN => BcType::Run(cur.u32()?),
        TY_WAIT => BcType::Wait(cur.u32()?),
        TY_PENDING_CALL => BcType::PendingCall(cur.u32()?, cur.u32()?),
        TY_HANDLE => BcType::Handle(cur.u32()?, cur.u32()?),
        TY_VM_SNAPSHOT => BcType::VmSnapshot,
        TY_RUN_SNAPSHOT => BcType::RunSnapshot(cur.u32()?),
        TY_BYTES => BcType::Bytes,
        TY_FILE_HANDLE => BcType::FileHandle,
        TY_RESOURCE_HANDLE => BcType::ResourceHandle,
        TY_HOST_RESOURCE => BcType::HostResource,
        TY_OP => BcType::Op(cur.u32()?, cur.u32()?),
        other => return Err(DecodeError::BadTypeTag(other)),
    };
    Ok(ty)
}

fn decode_instr(cur: &mut Cursor<'_>) -> Result<Instr, DecodeError> {
    let op = cur.u8()?;
    let instr = match op {
        OP_CONST_UNIT => Instr::ConstUnit,
        OP_CONST_BOOL => Instr::ConstBool(cur.u8()? != 0),
        OP_CONST_INT => Instr::ConstInt(cur.i64()?),
        OP_CONST_FLOAT => Instr::ConstFloat(cur.u64()?),
        OP_CONST_CHAR => Instr::ConstChar(cur.u32()?),
        OP_CONST_STR => Instr::ConstStr(cur.u32()?),
        OP_CONST_BYTES => Instr::ConstBytes(cur.u32()?),
        OP_NUMERIC => Instr::Numeric(
            NumericInstr::from_tag(cur.u8()?).ok_or(DecodeError::BadOpcode(OP_NUMERIC))?,
        ),
        OP_LOAD_LOCAL => Instr::LoadLocal(cur.u32()?),
        OP_STORE_LOCAL => Instr::StoreLocal(cur.u32()?),
        OP_POP => Instr::Pop,
        OP_ADD => Instr::Add,
        OP_SUB => Instr::Sub,
        OP_MUL => Instr::Mul,
        OP_DIV => Instr::Div,
        OP_REM => Instr::Rem,
        OP_NEG => Instr::Neg,
        OP_NOT => Instr::Not,
        OP_LT_INT => Instr::LtInt,
        OP_LE_INT => Instr::LeInt,
        OP_GT_INT => Instr::GtInt,
        OP_GE_INT => Instr::GeInt,
        OP_EQ_INT => Instr::EqInt,
        OP_NE_INT => Instr::NeInt,
        OP_EQ_BOOL => Instr::EqBool,
        OP_NE_BOOL => Instr::NeBool,
        OP_EQ_STR => Instr::Native(NativeInstr::EqStr),
        OP_NE_STR => Instr::Native(NativeInstr::NeStr),
        OP_STR_BYTE_LEN => Instr::Native(NativeInstr::StrByteLen),
        OP_STR_CHAR_COUNT => Instr::Native(NativeInstr::StrCharCount),
        OP_STR_CONCAT => Instr::Native(NativeInstr::StrConcat),
        OP_STR_STARTS_WITH => Instr::Native(NativeInstr::StrStartsWith),
        OP_STR_ENDS_WITH => Instr::Native(NativeInstr::StrEndsWith),
        OP_STR_CONTAINS => Instr::Native(NativeInstr::StrContains),
        OP_STR_FIND_INDEX => Instr::Native(NativeInstr::StrFindIndex),
        OP_TEXT_FIND_BYTE_INDEX => Instr::Native(NativeInstr::TextFindByteIndex),
        OP_TEXT_AT_BYTE => Instr::Native(NativeInstr::TextAtByte),
        OP_TEXT_TRIM => Instr::Native(NativeInstr::TextTrim),
        OP_TEXT_TRIM_START => Instr::Native(NativeInstr::TextTrimStart),
        OP_TEXT_TRIM_END => Instr::Native(NativeInstr::TextTrimEnd),
        OP_TEXT_TO_LOWER_ASCII => Instr::Native(NativeInstr::TextToLowerAscii),
        OP_TEXT_TO_UPPER_ASCII => Instr::Native(NativeInstr::TextToUpperAscii),
        OP_TEXT_REPLACE => Instr::Native(NativeInstr::TextReplace),
        OP_TEXT_PARSE_INT_STATUS => Instr::Native(NativeInstr::TextParseIntStatus),
        OP_TEXT_PARSE_INT_VALUE => Instr::Native(NativeInstr::TextParseIntValue),
        OP_EXTENSION => match cur.u8()? {
            EXT_TEXT_PAD_START => Instr::Native(NativeInstr::TextPadStart),
            EXT_TEXT_PAD_END => Instr::Native(NativeInstr::TextPadEnd),
            EXT_MAP_PUT_TEXT => Instr::Extended(ExtendedInstr::MapPutText {
                ty: cur.u32()?,
                discard: cur.flag()?,
            }),
            EXT_BYTES_TEXT_RANGE => Instr::Native(NativeInstr::BytesTextRange),
            EXT_MAP_INTERN_TEXT_RANGE => Instr::Extended(ExtendedInstr::MapInternTextRange),
            EXT_CONST_REGEX => Instr::ConstRegex(cur.u32()?),
            EXT_REGEX_COMPILE_STATUS => Instr::Native(NativeInstr::RegexCompileStatus),
            EXT_REGEX_COMPILE_VALUE => Instr::Native(NativeInstr::RegexCompileValue),
            EXT_REGEX_SOURCE => Instr::Native(NativeInstr::RegexSource),
            EXT_REGEX_IS_MATCH => Instr::Native(NativeInstr::RegexIsMatch),
            EXT_REGEX_COUNT => Instr::Native(NativeInstr::RegexCount),
            EXT_REGEX_SPLIT => Instr::Native(NativeInstr::RegexSplit),
            EXT_REGEX_REPLACE_ALL => Instr::Native(NativeInstr::RegexReplaceAll),
            EXT_REGEX_MATCH_START => Instr::Native(NativeInstr::RegexMatchStart),
            EXT_REGEX_MATCH_END => Instr::Native(NativeInstr::RegexMatchEnd),
            EXT_REGEX_MATCH_TEXT => Instr::Native(NativeInstr::RegexMatchText),
            EXT_REGEX_MATCH_GROUP_COUNT => Instr::Native(NativeInstr::RegexMatchGroupCount),
            EXT_REGEX_CAPTURES => Instr::Extended(ExtendedInstr::RegexCaptures { ty: cur.u32()? }),
            EXT_REGEX_MATCH_GROUP => {
                Instr::Extended(ExtendedInstr::RegexMatchGroup { ty: cur.u32()? })
            }
            EXT_REGEX_MATCH_NAMED => {
                Instr::Extended(ExtendedInstr::RegexMatchNamed { ty: cur.u32()? })
            }
            EXT_LIST_SWAP => Instr::Extended(ExtendedInstr::ListSwap),
            EXT_BB_SET => Instr::Native(NativeInstr::BbSet),
            EXT_BB_CAPACITY => Instr::Native(NativeInstr::BbCapacity),
            EXT_BB_TRUNCATE => Instr::Native(NativeInstr::BbTruncate),
            EXT_BYTES_READ_U32_BE => Instr::Native(NativeInstr::BytesReadU32Be),
            EXT_BYTES_READ_U32_LE => Instr::Native(NativeInstr::BytesReadU32Le),
            EXT_MODULE_CODE => Instr::Extended(ExtendedInstr::ModuleCode { module: cur.u32()? }),
            EXT_REFLECTION_DECLARATIONS => Instr::Extended(ExtendedInstr::ReflectionDeclarations),
            EXT_REFLECTION_MEMBERS => Instr::Extended(ExtendedInstr::ReflectionMembers),
            EXT_REFLECTION_NAME => Instr::Extended(ExtendedInstr::ReflectionName),
            EXT_REFLECTION_DECLARATION_KIND => {
                Instr::Extended(ExtendedInstr::ReflectionDeclarationKind)
            }
            EXT_REFLECTION_MEMBER_KIND => Instr::Extended(ExtendedInstr::ReflectionMemberKind),
            _ => return Err(DecodeError::BadOpcode(OP_EXTENSION)),
        },
        OP_BYTES_ENDS_WITH => Instr::Native(NativeInstr::BytesEndsWith),
        OP_BYTES_CONTAINS => Instr::Native(NativeInstr::BytesContains),
        OP_TEXT_SPLIT => Instr::Native(NativeInstr::TextSplit),
        OP_TEXT_LINES => Instr::Native(NativeInstr::TextLines),
        OP_TEXT_AT => Instr::Native(NativeInstr::TextAt),
        OP_TEXT_SLICE => Instr::Native(NativeInstr::TextSlice),
        OP_TEXT_IS_BOUNDARY => Instr::Native(NativeInstr::TextIsBoundary),
        OP_TEXT_SLICE_BYTES => Instr::Native(NativeInstr::TextSliceBytes),
        OP_TEXT_BYTES => Instr::Native(NativeInstr::TextBytes),
        OP_TEXT_LT => Instr::Native(NativeInstr::TextLt),
        OP_TEXT_LE => Instr::Native(NativeInstr::TextLe),
        OP_TEXT_GT => Instr::Native(NativeInstr::TextGt),
        OP_TEXT_GE => Instr::Native(NativeInstr::TextGe),
        OP_TEXT_TO_STRING => Instr::Native(NativeInstr::TextToString),
        OP_CHAR_CODEPOINT => Instr::Native(NativeInstr::CharCodepoint),
        OP_CHAR_UTF8_LEN => Instr::Native(NativeInstr::CharUtf8Len),
        OP_EQ_CHAR => Instr::Native(NativeInstr::EqChar),
        OP_NE_CHAR => Instr::Native(NativeInstr::NeChar),
        OP_LT_CHAR => Instr::Native(NativeInstr::LtChar),
        OP_LE_CHAR => Instr::Native(NativeInstr::LeChar),
        OP_GT_CHAR => Instr::Native(NativeInstr::GtChar),
        OP_GE_CHAR => Instr::Native(NativeInstr::GeChar),
        OP_EQ_REF => Instr::EqRef,
        OP_EQ_VALUE => Instr::EqValue,
        OP_NE_VALUE => Instr::NeValue,
        OP_NE_REF => Instr::NeRef,
        OP_CALL => Instr::Call(cur.u32()?),
        OP_CALL_G => Instr::CallG {
            func: cur.u32()?,
            app: cur.u32()?,
        },
        OP_CALL_VIRTUAL => Instr::CallVirtual {
            selector: cur.u32()?,
            argc: cur.u32()?,
        },
        OP_CALL_VIRTUAL_G => Instr::CallVirtualG {
            selector: cur.u32()?,
            argc: cur.u32()?,
            app: cur.u32()?,
        },
        OP_CALL_INTERFACE => Instr::CallInterface {
            site: cur.u32()?,
            recv_ty: cur.u32()?,
            app: decode_optional_index(cur)?,
        },
        OP_CALL_VALUE => Instr::CallValue { argc: cur.u32()? },
        OP_MAKE_CLOSURE => Instr::MakeClosure {
            func: cur.u32()?,
            captures: cur.u32()?,
        },
        OP_MAKE_CALLBACK => Instr::Extended(ExtendedInstr::MakeCallback {
            func: cur.u32()?,
            captures: cur.u32()?,
        }),
        OP_AS_CALLBACK => Instr::Extended(ExtendedInstr::AsCallback),
        OP_LOAD_CAPTURE => Instr::LoadCapture(cur.u32()?),
        OP_NEW => Instr::New(cur.u32()?),
        OP_NEW_G => Instr::NewG {
            class: cur.u32()?,
            app: cur.u32()?,
        },
        OP_LOAD_FIELD => Instr::LoadField(cur.u32()?),
        OP_STORE_FIELD => Instr::StoreField(cur.u32()?),
        OP_TUPLE_NEW => Instr::TupleNew {
            ty: cur.u32()?,
            count: cur.u32()?,
        },
        OP_TUPLE_GET => Instr::TupleGet(cur.u32()?),
        OP_IS_TYPE => Instr::IsType(cur.u32()?),
        OP_CAST_TYPE => Instr::CastType(cur.u32()?),
        OP_LIST_NEW => Instr::ListNew {
            ty: cur.u32()?,
            count: cur.u32()?,
        },
        OP_LIST_LEN => Instr::ListLen,
        OP_LIST_AT => Instr::ListAt,
        OP_LIST_PUSH => Instr::ListPush,
        OP_MAP_NEW => Instr::MapNew {
            ty: cur.u32()?,
            count: cur.u32()?,
        },
        OP_MAP_LEN => Instr::MapLen,
        OP_MAP_HAS => Instr::MapHas,
        OP_MAP_AT => Instr::MapAt,
        OP_MAP_PUT => Instr::MapPut {
            ty: cur.u32()?,
            discard: cur.flag()?,
        },
        OP_OPTION_SOME => Instr::Extended(ExtendedInstr::OptionSome { ty: cur.u32()? }),
        OP_OPTION_NONE => Instr::Extended(ExtendedInstr::OptionNone { ty: cur.u32()? }),
        OP_OPTION_PAYLOAD => Instr::Extended(ExtendedInstr::OptionPayload { ty: cur.u32()? }),
        OP_LIST_GET => Instr::Extended(ExtendedInstr::ListGet { ty: cur.u32()? }),
        OP_MAP_GET => Instr::Extended(ExtendedInstr::MapGet { ty: cur.u32()? }),
        OP_LIST_EPOCH => Instr::Extended(ExtendedInstr::ListEpoch),
        OP_LIST_ITER_LEN => Instr::Extended(ExtendedInstr::ListIterLen),
        OP_MAP_EPOCH => Instr::Extended(ExtendedInstr::MapEpoch),
        OP_MAP_ITER_LEN => Instr::Extended(ExtendedInstr::MapIterLen),
        OP_MAP_NEXT_INDEX => Instr::Extended(ExtendedInstr::MapNextIndex),
        OP_SEAL_INSTANCE => Instr::Extended(ExtendedInstr::SealInstance),
        OP_MAP_KEY_AT => Instr::Extended(ExtendedInstr::MapKeyAt),
        OP_MAP_VALUE_AT => Instr::Extended(ExtendedInstr::MapValueAt),
        OP_LIST_CAPACITY => Instr::Extended(ExtendedInstr::ListCapacity),
        OP_LIST_SET => Instr::Extended(ExtendedInstr::ListSet),
        OP_LIST_POP => Instr::Extended(ExtendedInstr::ListPop { ty: cur.u32()? }),
        OP_LIST_INSERT => Instr::Extended(ExtendedInstr::ListInsert),
        OP_LIST_REMOVE => Instr::Extended(ExtendedInstr::ListRemove),
        OP_LIST_SWAP_REMOVE => Instr::Extended(ExtendedInstr::ListSwapRemove),
        OP_LIST_RESERVE => Instr::Extended(ExtendedInstr::ListReserve),
        OP_LIST_TRUNCATE => Instr::Extended(ExtendedInstr::ListTruncate),
        OP_LIST_CONTAINS => Instr::Extended(ExtendedInstr::ListContains),
        OP_LIST_REORDER => Instr::Extended(ExtendedInstr::ListReorder),
        OP_MAP_REMOVE => Instr::Extended(ExtendedInstr::MapRemove { ty: cur.u32()? }),
        OP_MAP_CLEAR => Instr::Extended(ExtendedInstr::MapClear),
        OP_MAP_RESERVE => Instr::Extended(ExtendedInstr::MapReserve),
        OP_CALL_SLOT => Instr::Extended(ExtendedInstr::CallSlot {
            slot: cur.u32()?,
            app: decode_optional_index(cur)?,
        }),
        OP_NEW_SLOT => Instr::Extended(ExtendedInstr::NewSlot {
            slot: cur.u32()?,
            app: decode_optional_index(cur)?,
        }),
        OP_LOAD_SLOT => Instr::Extended(ExtendedInstr::LoadSlot { slot: cur.u32()? }),
        OP_SEND_SLOT => Instr::Extended(ExtendedInstr::SendSlot { slot: cur.u32()? }),
        OP_SYNTAX_TREE_ROOT => Instr::Extended(ExtendedInstr::SyntaxTreeRoot),
        OP_SYNTAX_KIND => Instr::Extended(ExtendedInstr::SyntaxKind),
        OP_SYNTAX_CATEGORY => Instr::Extended(ExtendedInstr::SyntaxCategory),
        OP_SYNTAX_RANGE_START => Instr::Extended(ExtendedInstr::SyntaxRangeStart),
        OP_SYNTAX_RANGE_END => Instr::Extended(ExtendedInstr::SyntaxRangeEnd),
        OP_SYNTAX_TEXT => Instr::Extended(ExtendedInstr::SyntaxText),
        OP_SYNTAX_CHILDREN => Instr::Extended(ExtendedInstr::SyntaxChildren),
        OP_SYNTAX_DETACH => Instr::Extended(ExtendedInstr::SyntaxDetach),
        OP_DYN_PACK => Instr::Extended(ExtendedInstr::DynPack { ty: cur.u32()? }),
        OP_DYN_RENDER => Instr::Extended(ExtendedInstr::DynRender),
        OP_SYNTAX_BUILD_TOKEN => Instr::Extended(ExtendedInstr::SyntaxBuildToken),
        OP_SYNTAX_BUILD_TRIVIA => Instr::Extended(ExtendedInstr::SyntaxBuildTrivia),
        OP_SYNTAX_BUILD_NODE => Instr::Extended(ExtendedInstr::SyntaxBuildNode),
        OP_SYNTAX_TO_TREE => Instr::Extended(ExtendedInstr::SyntaxToTree),
        OP_FUNCTION_CODE => Instr::Extended(ExtendedInstr::FunctionCode { func: cur.u32()? }),
        OP_CLASS_CODE => Instr::Extended(ExtendedInstr::ClassCode { class: cur.u32()? }),
        OP_CODE_SOURCE => Instr::Extended(ExtendedInstr::CodeSource { ty: cur.u32()? }),
        OP_CODE_DEFINITION => Instr::Extended(ExtendedInstr::CodeDefinition),
        OP_FAULT_SITE => Instr::Extended(ExtendedInstr::FaultSite { ty: cur.u32()? }),
        OP_FAULT_TRACE => Instr::Extended(ExtendedInstr::FaultTrace { ty: cur.u32()? }),
        OP_MAP_PROBE => Instr::Extended(ExtendedInstr::MapProbe),
        OP_MAP_PROBE_FOUND => Instr::Extended(ExtendedInstr::MapProbeFound),
        OP_MAP_PROBE_KEY => Instr::Extended(ExtendedInstr::MapProbeKey),
        OP_MAP_PROBE_VALUE => Instr::Extended(ExtendedInstr::MapProbeValue),
        OP_MAP_PROBE_SET_VALUE => Instr::Extended(ExtendedInstr::MapProbeSetValue),
        OP_MAP_PROBE_REMOVE => Instr::Extended(ExtendedInstr::MapProbeRemove),
        OP_MAP_INSERT_HASHED => Instr::Extended(ExtendedInstr::MapInsertHashed),
        OP_MAP_WRITE_GUARD => Instr::Extended(ExtendedInstr::MapWriteGuard),
        OP_SB_NEW => Instr::Native(NativeInstr::SbNew),
        OP_SB_APPEND_STR => Instr::Native(NativeInstr::SbAppendStr),
        OP_SB_APPEND_INT => Instr::Native(NativeInstr::SbAppendInt),
        OP_SB_APPEND_BOOL => Instr::Native(NativeInstr::SbAppendBool),
        OP_SB_BUILD => Instr::Native(NativeInstr::SbBuild),
        OP_SB_LEN => Instr::Native(NativeInstr::SbLen),
        OP_SB_CLEAR => Instr::Native(NativeInstr::SbClear),
        OP_BB_NEW => Instr::Native(NativeInstr::BbNew),
        OP_BB_APPEND => Instr::Native(NativeInstr::BbAppend),
        OP_BB_LEN => Instr::Native(NativeInstr::BbLen),
        OP_BB_BUILD => Instr::Native(NativeInstr::BbBuild),
        OP_SB_APPEND_CHAR => Instr::Native(NativeInstr::SbAppendChar),
        OP_SB_BYTE_LEN => Instr::Native(NativeInstr::SbByteLen),
        OP_SB_FINISH => Instr::Native(NativeInstr::SbFinish),
        OP_BB_FINISH => Instr::Native(NativeInstr::BbFinish),
        OP_BYTES_COMPACT => Instr::Native(NativeInstr::BytesCompact),
        OP_BYTES_TEXT_VIEW => Instr::Native(NativeInstr::BytesTextView),
        OP_TEXT_HASH => Instr::Native(NativeInstr::TextHash),
        OP_BYTES_HASH => Instr::Native(NativeInstr::BytesHash),
        OP_HASH_COMBINE => Instr::Native(NativeInstr::HashCombine),
        OP_HASH_UNORDERED_COMBINE => Instr::Native(NativeInstr::HashUnorderedCombine),
        OP_LT_BYTES => Instr::Native(NativeInstr::LtBytes),
        OP_LE_BYTES => Instr::Native(NativeInstr::LeBytes),
        OP_GT_BYTES => Instr::Native(NativeInstr::GtBytes),
        OP_GE_BYTES => Instr::Native(NativeInstr::GeBytes),
        OP_BB_EXTEND => Instr::Native(NativeInstr::BbExtend),
        OP_BB_RESERVE => Instr::Native(NativeInstr::BbReserve),
        OP_BB_CLEAR => Instr::Native(NativeInstr::BbClear),
        OP_BB_AT => Instr::Native(NativeInstr::BbAt),
        OP_BB_FIND_FROM => Instr::Native(NativeInstr::BbFindFrom),
        OP_BYTES_NEW => Instr::Native(NativeInstr::BytesNew),
        OP_BYTES_LEN => Instr::Native(NativeInstr::BytesLen),
        OP_BYTES_TEXT => Instr::Native(NativeInstr::BytesText),
        OP_BYTES_AT => Instr::Native(NativeInstr::BytesAt),
        OP_BYTES_GET => Instr::Native(NativeInstr::BytesGet),
        OP_BYTES_SLICE => Instr::Native(NativeInstr::BytesSlice),
        OP_BYTES_CONCAT => Instr::Native(NativeInstr::BytesConcat),
        OP_BYTES_STARTS_WITH => Instr::Native(NativeInstr::BytesStartsWith),
        OP_BYTES_FIND_INDEX => Instr::Native(NativeInstr::BytesFindIndex),
        OP_BYTES_HEX => Instr::Native(NativeInstr::BytesHex),
        OP_BYTES_IS_UTF8 => Instr::Native(NativeInstr::BytesIsUtf8),
        OP_EQ_BYTES => Instr::Native(NativeInstr::EqBytes),
        OP_NE_BYTES => Instr::Native(NativeInstr::NeBytes),
        OP_FREEZE => Instr::Freeze,
        OP_DIGEST => Instr::Digest { ty: cur.u32()? },
        OP_EQ_DIGEST => Instr::EqDigest,
        OP_NE_DIGEST => Instr::NeDigest,
        OP_JUMP => Instr::Jump(cur.u32()?),
        OP_JUMP_IF_FALSE => Instr::JumpIfFalse(cur.u32()?),
        OP_JUMP_IF_TRUE => Instr::JumpIfTrue(cur.u32()?),
        OP_RETURN => Instr::Return,
        OP_PERFORM => Instr::Perform {
            op: cur.u32()?,
            argc: cur.u32()?,
            reply_ty: cur.u32()?,
        },
        OP_PERFORM_VALUE => Instr::PerformValue {
            argc: cur.u32()?,
            reply_ty: cur.u32()?,
        },
        OP_OP_CONST => Instr::OpConst(cur.u32()?),
        OP_PREPARE_WAIT => {
            let op = cur.u32()?;
            let argc = cur.u32()?;
            let reply_ty = cur.u32()?;
            let instruction =
                ExtendedInstr::prepare_wait(op, argc, reply_ty).ok_or(DecodeError::BadLength)?;
            Instr::Extended(instruction)
        }
        OP_TABLE_EDIT => Instr::TableEdit {
            action: cur.u32()?,
            kind: cur.u32()?,
            slot: cur.u32()?,
        },
        OP_AS_CALL => Instr::AsCall {
            op: cur.u32()?,
            ty: cur.u32()?,
        },
        OP_CALL_ARGS => Instr::CallArgs,
        OP_FAULT_CODE => Instr::FaultCode,
        OP_FAULT_DENIED => Instr::FaultDenied,
        OP_RAISE_USER_PANIC => Instr::RaiseUserPanic,
        OP_RAISE_ASSERTION_FAILED => Instr::RaiseAssertionFailed,
        OP_RAISE_FAULT => Instr::RaiseFault,
        OP_REQUEST_OP => Instr::RequestOp,
        OP_UNREACHABLE => Instr::Unreachable,
        other => return Err(DecodeError::BadOpcode(other)),
    };
    Ok(instr)
}

fn decode_optional_index(cur: &mut Cursor<'_>) -> Result<u32, DecodeError> {
    if cur.flag()? {
        Ok(cur.u32()?)
    } else {
        Ok(NO_APP)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_table_revisions_keep_their_prefix() {
        let mut current = CodeTable::from(vec![10, 20]);
        let earlier = current.clone();

        current.push(30);

        assert_eq!(earlier.to_vec(), vec![10, 20]);
        assert_eq!(current.to_vec(), vec![10, 20, 30]);
        assert_eq!(earlier.chunk_count(), 1);
        assert_eq!(current.chunk_count(), 2);
    }

    fn plain_func(name: &str, ret: u32, blocks: Vec<Vec<Instr>>) -> Func {
        Func {
            name: name.to_string(),
            param_names: vec![],
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret,
            row: vec![],
            captures: vec![],
            local_types: vec![0],
            blocks,
        }
    }

    fn sample_module() -> Module {
        Module {
            strings: vec!["hello".to_string(), "Io.Write".to_string()],
            bytes: vec![vec![0, 255]],
            types: vec![
                BcType::Unit,
                BcType::Int,
                BcType::Str,
                BcType::Class(0),
                BcType::List(1),
                BcType::Map(2, 1),
                BcType::Fn(vec![1], vec![false], 1, vec![BcRow::Op(1)]),
                BcType::Class(0),
                BcType::Class(0),
                BcType::Var(0),
                BcType::Inst(1, vec![1]),
                BcType::Tuple(vec![1, 2]),
            ],
            selectors: vec!["add".to_string()],
            apps: vec![TypeApp {
                types: vec![1],
                rows: vec![vec![BcRow::Op(1), BcRow::Var(0)]],
            }],
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![vec![], vec![]],
            func_bounds: vec![vec![], vec![]],
            reflections: vec![ReflectionModule {
                name: "sample".to_string(),
                declarations: vec![
                    ReflectionDeclaration {
                        kind: ExportKind::Function,
                        name: "main".to_string(),
                        def: 0,
                        callable: 0,
                    },
                    ReflectionDeclaration {
                        kind: ExportKind::Class,
                        name: "Counter".to_string(),
                        def: 0,
                        callable: NO_REFLECTION_DEF,
                    },
                ],
            }],
            classes: vec![
                BcClass {
                    name: "Counter".to_string(),
                    parent_args: Vec::new(),
                    key: "Counter".to_string(),
                    is_final: false,
                    is_frozen: false,
                    parent: NO_PARENT,
                    type_params: 0,
                    kind: BcClassKind::Normal,
                    fields: vec![("value".to_string(), 1)],
                    field_defaults: vec![false],
                    own_start: 0,
                    has_init: false,
                    methods: vec![(0, 1)],
                },
                BcClass {
                    name: "Box".to_string(),
                    parent_args: Vec::new(),
                    key: "Box".to_string(),
                    is_final: false,
                    is_frozen: false,
                    parent: NO_PARENT,
                    type_params: 1,
                    kind: BcClassKind::Normal,
                    fields: vec![("value".to_string(), 9)],
                    field_defaults: vec![false],
                    own_start: 0,
                    has_init: false,
                    methods: vec![],
                },
            ],
            funcs: vec![
                plain_func(
                    "main",
                    1,
                    vec![vec![
                        Instr::ConstInt(41),
                        Instr::ConstInt(1),
                        Instr::Add,
                        Instr::Return,
                    ]],
                ),
                Func {
                    name: "add".to_string(),
                    param_names: vec!["self".to_string(), "value".to_string()],
                    type_params: 0,
                    effect_params: 1,
                    params: vec![3, 1],
                    param_muts: vec![false, false],
                    ret: 1,
                    row: vec![BcRow::Op(1), BcRow::Var(0)],
                    captures: vec![],
                    local_types: vec![3, 1],
                    blocks: vec![vec![Instr::LoadLocal(1), Instr::Return]],
                },
            ],
            imports: vec![],
            slots: vec![],
            core_roles: [NO_ROLE; CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![
                FuncBinding {
                    key: "Counter.add".to_string(),
                    func: 1,
                    class: NO_CLASS,
                },
                FuncBinding {
                    key: "main".to_string(),
                    func: 0,
                    class: NO_CLASS,
                },
            ],
            debug: Vec::new(),
        }
    }

    #[test]
    fn a_binding_outside_the_function_table_is_rejected() {
        let mut module = sample_module();
        module.bindings = vec![FuncBinding {
            key: "gone".to_string(),
            func: 7,
            class: NO_CLASS,
        }];
        let bytes = encode(&module);
        assert!(matches!(decode(&bytes), Err(DecodeError::BadBinding)));
    }

    #[test]
    fn decoded_instruction_form_is_fixed_size() {
        assert_eq!(std::mem::size_of::<Instr>(), 16);
    }

    #[test]
    fn decoder_rejects_a_table_that_exceeds_its_allocation_limit() {
        let mut bytes = Vec::new();
        for _ in 0..11 {
            bytes.extend_from_slice(&0u32.to_le_bytes());
        }
        for _ in 0..CORE_ROLE_COUNT {
            bytes.extend_from_slice(&NO_ROLE.to_le_bytes());
        }
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        let count = MAX_DECODE_VECTOR_BYTES / std::mem::size_of::<Func>() + 1;
        bytes.extend_from_slice(&(count as u32).to_le_bytes());
        bytes.resize(bytes.len() + count, 0);

        assert_eq!(decode_semantic(&bytes), Err(DecodeError::BadLength));
    }

    #[test]
    fn interface_call_site_uses_two_sixteen_bit_indices() {
        let site = pack_interface_call_site(MAX_INTERFACE_CALL_INDEX, MAX_INTERFACE_CALL_INDEX)
            .expect("both indices fit");
        assert_eq!(
            unpack_interface_call_site(site),
            (MAX_INTERFACE_CALL_INDEX, MAX_INTERFACE_CALL_INDEX)
        );
        assert!(pack_interface_call_site(MAX_INTERFACE_CALL_INDEX + 1, 0).is_none());
        assert!(pack_interface_call_site(0, MAX_INTERFACE_CALL_INDEX + 1).is_none());
    }

    #[test]
    fn a_prepared_wait_round_trips_in_its_compact_form() {
        let instruction =
            ExtendedInstr::prepare_wait(7, 3, 1).expect("the prepared wait fields fit");
        let mut module = sample_module();
        module.funcs[0].blocks[0] = vec![Instr::Extended(instruction), Instr::Return];
        let bytes = encode(&module);
        assert_eq!(decode(&bytes).expect("the artifact decodes"), module);
        assert!(ExtendedInstr::prepare_wait(u32::from(u16::MAX) + 1, 0, 1).is_none());
        assert!(ExtendedInstr::prepare_wait(0, u32::from(u16::MAX) + 1, 1).is_none());
    }

    #[test]
    fn encode_decode_round_trip() {
        let module = sample_module();
        let bytes = encode(&module);
        assert_eq!(decode(&bytes).unwrap(), module);
    }

    #[test]
    fn an_abi_group_row_round_trips() {
        let mut module = sample_module();
        let io = lm_abi::standard_bundle()
            .group_by_name("Io")
            .expect("the standard ABI has the Io group");
        module.funcs[0].row = vec![BcRow::Group(io)];
        let bytes = encode(&module);
        assert_eq!(decode(&bytes).expect("the module decodes"), module);
    }

    #[test]
    fn an_artifact_rejects_another_abi_bundle() {
        let module = sample_module();
        let bytes = encode(&module);
        let mut builder = lm_abi::AbiBundle::builder();
        builder.add_group(lm_abi::GroupSpec::namespace("Telemetry"));
        builder.add_operation(lm_abi::OperationSpec::fixed(
            "Telemetry",
            "Event",
            vec![lm_abi::AbiType::STR],
            lm_abi::AbiType::UNIT,
        ));
        let bundle = builder.build().expect("the extension bundle is valid");
        assert!(matches!(
            decode_with_bundle(&bytes, &bundle),
            Err(DecodeError::BadBundle { .. })
        ));
    }

    #[test]
    fn debug_data_changes_only_the_container_hash() {
        let plain = sample_module();
        let mut attached = plain.clone();
        let built = lm_abi::syntax::build_syntax_node(lm_abi::syntax::KIND_FUNCTION, &[])
            .expect("the syntax encodes");
        attached.debug = debug::encode(&debug::DebugInfo {
            sources: vec![debug::DebugSource {
                path: "sample.lm".to_string(),
                text: built.source,
                syntax: built.records,
            }],
            definitions: vec![debug::DebugDefinition {
                kind: debug::DefinitionKind::Function,
                target: 0,
                source: 0,
                lo: 0,
                hi: 0,
                syntax: 0,
                origin: debug::definition_origin(
                    "sample.lm",
                    "",
                    debug::DefinitionKind::Function,
                    0,
                    0,
                )
                .expect("the origin hashes"),
            }],
            functions: vec![debug::DebugFunction {
                function: 0,
                source: 0,
                lo: 0,
                hi: 0,
            }],
            code_origins: Vec::new(),
        });
        let plain_identity = identity::module_identity(&plain).expect("the module hashes");
        let attached_identity =
            identity::module_identity(&attached).expect("the attached module hashes");
        assert_eq!(
            plain_identity.semantic_hash,
            attached_identity.semantic_hash
        );
        assert_eq!(
            identity::verification_hash(&plain),
            identity::verification_hash(&attached)
        );
        let plain_bytes = encode(&plain);
        let attached_bytes = encode(&attached);
        assert_ne!(
            identity::container_hash(&plain_bytes),
            identity::container_hash(&attached_bytes)
        );
        assert_eq!(decode(&attached_bytes), Ok(attached));
    }

    #[test]
    fn every_slot_contract_round_trips() {
        let callable = BcCallableContract {
            type_params: 0,
            effect_params: 0,
            type_bounds: vec![],
            params: vec![1],
            param_muts: vec![false],
            ret: 1,
            row: vec![],
        };
        let mut module = sample_module();
        let constructor = BcCallableContract {
            ret: 3,
            params: vec![],
            param_muts: vec![],
            ..callable.clone()
        };
        module.slots = vec![
            SlotSpec {
                key: [1; 32],
                binding: "function".to_string(),
                late: true,
                contract_hash: [11; 32],
                contract: SlotContract::Function(callable.clone()),
                initial: Some(SlotTarget::Function(0)),
            },
            SlotSpec {
                key: [2; 32],
                binding: "method".to_string(),
                late: true,
                contract_hash: [12; 32],
                contract: SlotContract::Method(callable),
                initial: Some(SlotTarget::Function(1)),
            },
            SlotSpec {
                key: [3; 32],
                binding: "class".to_string(),
                late: true,
                contract_hash: [13; 32],
                contract: SlotContract::Class {
                    type_params: 0,
                    abi: [7; 32],
                    ty: 3,
                    constructor,
                },
                initial: Some(SlotTarget::Class {
                    class: 0,
                    constructor: 0,
                }),
            },
            SlotSpec {
                key: [4; 32],
                binding: "value".to_string(),
                late: true,
                contract_hash: [14; 32],
                contract: SlotContract::Value { ty: 1 },
                initial: None,
            },
            SlotSpec {
                key: [5; 32],
                binding: "process".to_string(),
                late: true,
                contract_hash: [15; 32],
                contract: SlotContract::Process {
                    message: 2,
                    result: 1,
                },
                initial: None,
            },
        ];
        assert_eq!(decode(&encode(&module)).unwrap(), module);
    }

    #[test]
    fn round_trips_every_instruction() {
        let instrs = vec![
            Instr::ConstUnit,
            Instr::ConstBool(true),
            Instr::ConstInt(-5),
            Instr::ConstFloat(1.5f64.to_bits()),
            Instr::ConstChar('猫' as u32),
            Instr::ConstStr(0),
            Instr::ConstBytes(0),
            Instr::LoadLocal(1),
            Instr::StoreLocal(1),
            Instr::Pop,
            Instr::Add,
            Instr::Native(NativeInstr::EqStr),
            Instr::Native(NativeInstr::StrByteLen),
            Instr::Native(NativeInstr::StrCharCount),
            Instr::Native(NativeInstr::StrConcat),
            Instr::Native(NativeInstr::StrStartsWith),
            Instr::Native(NativeInstr::StrEndsWith),
            Instr::Native(NativeInstr::StrContains),
            Instr::Native(NativeInstr::StrFindIndex),
            Instr::Native(NativeInstr::TextFindByteIndex),
            Instr::Native(NativeInstr::TextAtByte),
            Instr::Native(NativeInstr::TextTrim),
            Instr::Native(NativeInstr::TextTrimStart),
            Instr::Native(NativeInstr::TextTrimEnd),
            Instr::Native(NativeInstr::TextToLowerAscii),
            Instr::Native(NativeInstr::TextToUpperAscii),
            Instr::Native(NativeInstr::TextReplace),
            Instr::Native(NativeInstr::TextParseIntStatus),
            Instr::Native(NativeInstr::TextParseIntValue),
            Instr::Native(NativeInstr::BytesEndsWith),
            Instr::Native(NativeInstr::BytesContains),
            Instr::Native(NativeInstr::TextSplit),
            Instr::Native(NativeInstr::TextLines),
            Instr::EqRef,
            Instr::EqValue,
            Instr::NeValue,
            Instr::NeRef,
            Instr::Call(0),
            Instr::CallG { func: 0, app: 0 },
            Instr::CallVirtual {
                selector: 0,
                argc: 1,
            },
            Instr::CallVirtualG {
                selector: 0,
                argc: 1,
                app: 0,
            },
            Instr::CallInterface {
                site: pack_interface_call_site(0, 0).expect("the call site fits"),
                recv_ty: 4,
                app: NO_APP,
            },
            Instr::CallValue { argc: 2 },
            Instr::MakeClosure {
                func: 1,
                captures: 0,
            },
            Instr::Extended(ExtendedInstr::MakeCallback {
                func: 1,
                captures: 0,
            }),
            Instr::Extended(ExtendedInstr::AsCallback),
            Instr::LoadCapture(0),
            Instr::New(0),
            Instr::NewG { class: 1, app: 0 },
            Instr::LoadField(0),
            Instr::StoreField(0),
            Instr::TupleNew { ty: 11, count: 2 },
            Instr::TupleGet(1),
            Instr::IsType(3),
            Instr::CastType(3),
            Instr::ListNew { ty: 4, count: 2 },
            Instr::ListLen,
            Instr::ListAt,
            Instr::ListPush,
            Instr::MapNew { ty: 5, count: 1 },
            Instr::MapLen,
            Instr::MapHas,
            Instr::MapAt,
            Instr::MapPut {
                ty: 0,
                discard: false,
            },
            Instr::MapPut {
                ty: 0,
                discard: true,
            },
            Instr::Extended(ExtendedInstr::OptionSome { ty: 0 }),
            Instr::Extended(ExtendedInstr::OptionNone { ty: 0 }),
            Instr::Extended(ExtendedInstr::OptionPayload { ty: 0 }),
            Instr::Extended(ExtendedInstr::ListGet { ty: 0 }),
            Instr::Extended(ExtendedInstr::MapGet { ty: 0 }),
            Instr::Extended(ExtendedInstr::MapPutText {
                ty: 0,
                discard: false,
            }),
            Instr::Extended(ExtendedInstr::MapPutText {
                ty: 0,
                discard: true,
            }),
            Instr::Native(NativeInstr::BytesTextRange),
            Instr::Extended(ExtendedInstr::MapInternTextRange),
            Instr::Extended(ExtendedInstr::ListEpoch),
            Instr::Extended(ExtendedInstr::ListIterLen),
            Instr::Extended(ExtendedInstr::MapEpoch),
            Instr::Extended(ExtendedInstr::MapIterLen),
            Instr::Extended(ExtendedInstr::MapNextIndex),
            Instr::Extended(ExtendedInstr::SealInstance),
            Instr::Extended(ExtendedInstr::MapKeyAt),
            Instr::Extended(ExtendedInstr::MapValueAt),
            Instr::Extended(ExtendedInstr::ListCapacity),
            Instr::Extended(ExtendedInstr::ListSet),
            Instr::Extended(ExtendedInstr::ListPop { ty: 0 }),
            Instr::Extended(ExtendedInstr::ListInsert),
            Instr::Extended(ExtendedInstr::ListRemove),
            Instr::Extended(ExtendedInstr::ListSwapRemove),
            Instr::Extended(ExtendedInstr::ListSwap),
            Instr::Extended(ExtendedInstr::ListReserve),
            Instr::Extended(ExtendedInstr::ListTruncate),
            Instr::Extended(ExtendedInstr::ListContains),
            Instr::Extended(ExtendedInstr::ListReorder),
            Instr::Extended(ExtendedInstr::MapRemove { ty: 0 }),
            Instr::Extended(ExtendedInstr::MapClear),
            Instr::Extended(ExtendedInstr::MapReserve),
            Instr::Extended(ExtendedInstr::CallSlot {
                slot: 0,
                app: NO_APP,
            }),
            Instr::Extended(ExtendedInstr::CallSlot { slot: 0, app: 0 }),
            Instr::Extended(ExtendedInstr::NewSlot {
                slot: 1,
                app: NO_APP,
            }),
            Instr::Extended(ExtendedInstr::NewSlot { slot: 1, app: 0 }),
            Instr::Extended(ExtendedInstr::LoadSlot { slot: 2 }),
            Instr::Extended(ExtendedInstr::SendSlot { slot: 3 }),
            Instr::Extended(ExtendedInstr::SyntaxTreeRoot),
            Instr::Extended(ExtendedInstr::SyntaxKind),
            Instr::Extended(ExtendedInstr::SyntaxCategory),
            Instr::Extended(ExtendedInstr::SyntaxRangeStart),
            Instr::Extended(ExtendedInstr::SyntaxRangeEnd),
            Instr::Extended(ExtendedInstr::SyntaxText),
            Instr::Extended(ExtendedInstr::SyntaxChildren),
            Instr::Extended(ExtendedInstr::SyntaxDetach),
            Instr::Extended(ExtendedInstr::DynPack { ty: 0 }),
            Instr::Extended(ExtendedInstr::DynRender),
            Instr::Extended(ExtendedInstr::SyntaxBuildToken),
            Instr::Extended(ExtendedInstr::SyntaxBuildTrivia),
            Instr::Extended(ExtendedInstr::SyntaxBuildNode),
            Instr::Extended(ExtendedInstr::SyntaxToTree),
            Instr::Extended(ExtendedInstr::FunctionCode { func: 0 }),
            Instr::Extended(ExtendedInstr::ClassCode { class: 0 }),
            Instr::Extended(ExtendedInstr::ModuleCode { module: 0 }),
            Instr::Extended(ExtendedInstr::ReflectionDeclarations),
            Instr::Extended(ExtendedInstr::ReflectionMembers),
            Instr::Extended(ExtendedInstr::ReflectionName),
            Instr::Extended(ExtendedInstr::ReflectionDeclarationKind),
            Instr::Extended(ExtendedInstr::ReflectionMemberKind),
            Instr::Extended(ExtendedInstr::CodeSource { ty: 0 }),
            Instr::Extended(ExtendedInstr::CodeDefinition),
            Instr::Extended(ExtendedInstr::FaultSite { ty: 0 }),
            Instr::Extended(ExtendedInstr::FaultTrace { ty: 0 }),
            Instr::Native(NativeInstr::SbNew),
            Instr::Native(NativeInstr::SbAppendStr),
            Instr::Native(NativeInstr::SbAppendInt),
            Instr::Native(NativeInstr::SbAppendBool),
            Instr::Native(NativeInstr::SbBuild),
            Instr::Native(NativeInstr::BbNew),
            Instr::Native(NativeInstr::BbAppend),
            Instr::Native(NativeInstr::BbLen),
            Instr::Native(NativeInstr::BbBuild),
            Instr::Native(NativeInstr::BbSet),
            Instr::Native(NativeInstr::BbCapacity),
            Instr::Native(NativeInstr::BbTruncate),
            Instr::Native(NativeInstr::BytesReadU32Be),
            Instr::Native(NativeInstr::BytesReadU32Le),
            Instr::Native(NativeInstr::HashCombine),
            Instr::Native(NativeInstr::HashUnorderedCombine),
            Instr::Numeric(NumericInstr::IntBitAnd),
            Instr::Numeric(NumericInstr::IntBitOr),
            Instr::Numeric(NumericInstr::IntBitXor),
            Instr::Numeric(NumericInstr::IntBitNot),
            Instr::Numeric(NumericInstr::IntShl),
            Instr::Numeric(NumericInstr::IntShr),
            Instr::Numeric(NumericInstr::IntUshr),
            Instr::Numeric(NumericInstr::IntWrappingAdd),
            Instr::Numeric(NumericInstr::IntWrappingSub),
            Instr::Numeric(NumericInstr::IntWrappingMul),
            Instr::Numeric(NumericInstr::IntRotateLeft),
            Instr::Numeric(NumericInstr::IntRotateRight),
            Instr::Numeric(NumericInstr::IntRotateLeft32),
            Instr::Numeric(NumericInstr::IntRotateRight32),
            Instr::Numeric(NumericInstr::IntToFloat),
            Instr::Numeric(NumericInstr::FloatNeg),
            Instr::Numeric(NumericInstr::FloatAdd),
            Instr::Numeric(NumericInstr::FloatSub),
            Instr::Numeric(NumericInstr::FloatMul),
            Instr::Numeric(NumericInstr::FloatDiv),
            Instr::Numeric(NumericInstr::FloatEq),
            Instr::Numeric(NumericInstr::FloatNe),
            Instr::Numeric(NumericInstr::FloatLt),
            Instr::Numeric(NumericInstr::FloatLe),
            Instr::Numeric(NumericInstr::FloatGt),
            Instr::Numeric(NumericInstr::FloatGe),
            Instr::Numeric(NumericInstr::FloatIsNan),
            Instr::Numeric(NumericInstr::FloatHash),
            Instr::Numeric(NumericInstr::FloatBits),
            Instr::Numeric(NumericInstr::FloatFromBits),
            Instr::Numeric(NumericInstr::FloatToIntStatus),
            Instr::Numeric(NumericInstr::FloatToIntValue),
            Instr::Numeric(NumericInstr::SbAppendFloat),
            Instr::Numeric(NumericInstr::BytesBitAnd),
            Instr::Numeric(NumericInstr::BytesBitOr),
            Instr::Numeric(NumericInstr::BytesBitXor),
            Instr::Numeric(NumericInstr::BytesBitNot),
            Instr::Numeric(NumericInstr::TextParseFloatStatus),
            Instr::Numeric(NumericInstr::TextParseFloatValue),
            Instr::Numeric(NumericInstr::FloatFixed),
            Instr::Numeric(NumericInstr::IntCountOnes),
            Instr::Numeric(NumericInstr::IntLeadingZeros),
            Instr::Numeric(NumericInstr::IntTrailingZeros),
            Instr::Numeric(NumericInstr::IntSignum),
            Instr::Numeric(NumericInstr::FloatAbs),
            Instr::Numeric(NumericInstr::FloatMin),
            Instr::Numeric(NumericInstr::FloatMax),
            Instr::Numeric(NumericInstr::FloatSqrt),
            Instr::Numeric(NumericInstr::FloatFloor),
            Instr::Numeric(NumericInstr::FloatCeil),
            Instr::Numeric(NumericInstr::FloatRound),
            Instr::Numeric(NumericInstr::FloatTrunc),
            Instr::Numeric(NumericInstr::FloatIsFinite),
            Instr::Numeric(NumericInstr::FloatIsInfinite),
            Instr::Numeric(NumericInstr::FloatRem),
            Instr::Numeric(NumericInstr::FloatCopySign),
            Instr::Numeric(NumericInstr::FloatMulAdd),
            Instr::Numeric(NumericInstr::FloatPow),
            Instr::Numeric(NumericInstr::FloatExp),
            Instr::Numeric(NumericInstr::FloatExp2),
            Instr::Numeric(NumericInstr::FloatExpM1),
            Instr::Numeric(NumericInstr::FloatLn),
            Instr::Numeric(NumericInstr::FloatLog2),
            Instr::Numeric(NumericInstr::FloatLog10),
            Instr::Numeric(NumericInstr::FloatLn1P),
            Instr::Numeric(NumericInstr::FloatCbrt),
            Instr::Numeric(NumericInstr::FloatHypot),
            Instr::Numeric(NumericInstr::FloatSin),
            Instr::Numeric(NumericInstr::FloatCos),
            Instr::Numeric(NumericInstr::FloatTan),
            Instr::Numeric(NumericInstr::FloatAsin),
            Instr::Numeric(NumericInstr::FloatAcos),
            Instr::Numeric(NumericInstr::FloatAtan),
            Instr::Numeric(NumericInstr::FloatAtan2),
            Instr::Numeric(NumericInstr::FloatSinh),
            Instr::Numeric(NumericInstr::FloatCosh),
            Instr::Numeric(NumericInstr::FloatTanh),
            Instr::Numeric(NumericInstr::FloatAsinh),
            Instr::Numeric(NumericInstr::FloatAcosh),
            Instr::Numeric(NumericInstr::FloatAtanh),
            Instr::Native(NativeInstr::TextPadStart),
            Instr::Native(NativeInstr::TextPadEnd),
            Instr::Freeze,
            Instr::FaultCode,
            Instr::FaultDenied,
            Instr::RequestOp,
            Instr::Jump(0),
            Instr::JumpIfFalse(0),
            Instr::JumpIfTrue(0),
            Instr::Return,
        ];
        let mut module = sample_module();
        module.funcs[0].blocks = vec![instrs.clone()];
        let bytes = encode(&module);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.funcs[0].blocks[0], instrs);
    }

    #[test]
    fn encode_is_deterministic() {
        assert_eq!(encode(&sample_module()), encode(&sample_module()));
    }

    #[test]
    fn every_truncation_is_rejected() {
        let bytes = encode(&sample_module());
        for len in 0..bytes.len() {
            let result = decode(&bytes[..len]);
            assert!(result.is_err(), "prefix length {len} was accepted");
        }
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut bytes = encode(&sample_module());
        bytes[0] = b'X';
        assert_eq!(decode(&bytes), Err(DecodeError::BadMagic));
    }

    #[test]
    fn old_version_is_rejected() {
        let mut bytes = encode(&sample_module());
        bytes[4] = 2;
        bytes[5] = 0;
        assert_eq!(decode(&bytes), Err(DecodeError::BadVersion(2)));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        // Bytes past the described sections fail the section table.
        let mut bytes = encode(&sample_module());
        bytes.push(0);
        assert_eq!(decode(&bytes), Err(DecodeError::BadSectionTable));
    }

    #[test]
    fn huge_length_field_is_rejected() {
        let mut bytes = encode(&sample_module());
        // The string count starts the semantic region.
        bytes[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&bytes), Err(DecodeError::BadLength));
    }

    #[test]
    fn bad_type_tag_is_rejected() {
        let module = Module {
            strings: vec![],
            bytes: vec![],
            types: vec![BcType::Unit],
            selectors: vec![],
            apps: vec![],
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![],
            func_bounds: vec![],
            reflections: vec![],
            classes: vec![],
            funcs: vec![],
            imports: vec![],
            slots: vec![],
            core_roles: [NO_ROLE; CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: Vec::new(),
        };
        let mut bytes = encode(&module);
        // The type tag follows the string, byte, and type counts.
        let pos = HEADER_LEN + 4 + 4 + 4;
        assert_eq!(bytes[pos], TY_UNIT);
        bytes[pos] = 0xee;
        assert_eq!(decode(&bytes), Err(DecodeError::BadTypeTag(0xee)));
    }

    #[test]
    fn section_offsets_must_be_contiguous() {
        let bytes = encode(&sample_module());
        // Shift the semantic offset forward by one.
        let mut corrupt = bytes.clone();
        let at = SECTION_TABLE_AT;
        let offset = u32::from_le_bytes(corrupt[at..at + 4].try_into().unwrap());
        corrupt[at..at + 4].copy_from_slice(&(offset + 1).to_le_bytes());
        assert_eq!(decode(&corrupt), Err(DecodeError::BadSectionTable));
        // Grow the semantic length so the sections overlap the input
        // end.
        let mut corrupt = bytes.clone();
        let len = u32::from_le_bytes(corrupt[at + 4..at + 8].try_into().unwrap());
        corrupt[at + 4..at + 8].copy_from_slice(&(len + 1).to_le_bytes());
        assert_eq!(decode(&corrupt), Err(DecodeError::BadSectionTable));
        // Shrink the semantic length: the export offset no longer
        // lines up.
        let mut corrupt = bytes;
        let len = u32::from_le_bytes(corrupt[at + 4..at + 8].try_into().unwrap());
        corrupt[at + 4..at + 8].copy_from_slice(&(len - 1).to_le_bytes());
        assert_eq!(decode(&corrupt), Err(DecodeError::BadSectionTable));
    }

    #[test]
    fn every_section_boundary_truncation_is_rejected() {
        let bytes = encode(&sample_module());
        // Read the section table for the boundary positions.
        let mut boundaries = vec![HEADER_LEN];
        for i in 0..3 {
            let at = SECTION_TABLE_AT + i * 8;
            let offset = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            boundaries.push(offset + len);
        }
        for boundary in boundaries {
            for cut in [boundary.saturating_sub(1), boundary] {
                if cut >= bytes.len() {
                    continue;
                }
                assert!(
                    decode(&bytes[..cut]).is_err(),
                    "truncation at {cut} was accepted"
                );
            }
        }
    }

    #[test]
    fn export_count_mismatch_is_rejected() {
        let module = sample_module();
        let bytes = encode(&module);
        // The export section starts with the interface-name count.
        let at = SECTION_TABLE_AT + 8;
        let exp_at = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let mut corrupt = bytes.clone();
        corrupt[exp_at..exp_at + 4].copy_from_slice(&1u32.to_le_bytes());
        assert!(matches!(
            decode(&corrupt),
            Err(DecodeError::ExportCountMismatch) | Err(DecodeError::BadLength)
        ));
    }

    #[test]
    fn only_reflected_names_live_in_the_semantic_region() {
        let module = sample_module();
        let bytes = encode(&module);
        let at = SECTION_TABLE_AT;
        let sem_at = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let sem_len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let semantic = &bytes[sem_at..sem_at + sem_len];
        for name in ["sample", "Counter", "main"] {
            let found = semantic.windows(name.len()).any(|w| w == name.as_bytes());
            assert!(found, "the semantic region omits the reflected name {name}");
        }
        let found = semantic.windows(3).any(|window| window == b"Box");
        assert!(!found, "the semantic region contains an unreflected name");
    }

    #[test]
    fn bad_row_tag_is_rejected() {
        let module = sample_module();
        let bytes = encode(&module);
        // Find the app row element tag: it follows the app tables.
        // Corrupt every ROW_OP tag candidate and require at least one
        // rejection with BadRowTag.
        let mut rejected = false;
        for pos in 0..bytes.len() {
            if bytes[pos] != ROW_OP && bytes[pos] != ROW_VAR {
                continue;
            }
            let mut corrupt = bytes.clone();
            corrupt[pos] = 0x77;
            if decode(&corrupt) == Err(DecodeError::BadRowTag(0x77)) {
                rejected = true;
                break;
            }
        }
        assert!(rejected, "no row tag rejection was observed");
    }

    #[test]
    fn bad_class_kind_is_rejected() {
        let module = sample_module();
        let bytes = encode(&module);
        let mut rejected = false;
        for pos in 0..bytes.len() {
            let mut corrupt = bytes.clone();
            corrupt[pos] = 0x99;
            if decode(&corrupt) == Err(DecodeError::BadClassKind(0x99)) {
                rejected = true;
                break;
            }
        }
        assert!(rejected, "no class kind rejection was observed");
    }

    #[test]
    fn bad_opcode_is_rejected() {
        let module = sample_module();
        let bytes = encode(&module);
        // Find the `Add` opcode byte and replace it.
        let pos = bytes
            .iter()
            .position(|b| *b == OP_ADD)
            .expect("sample has Add");
        let mut corrupt = bytes.clone();
        corrupt[pos] = 0xff;
        assert!(decode(&corrupt).is_err());
    }
}
