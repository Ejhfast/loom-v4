//! Bytecode formats for the week-3 language slice.
//!
//! This crate defines two forms:
//! - a compact serialized byte format for storage and transfer;
//! - a fixed-size decoded instruction form for the verifier and the VM.
//!
//! The decoder validates structure only. The independent verifier in
//! `lm-verify` validates tables, types, rows, type applications,
//! jumps, calls, and stack shapes.

pub mod closed;
pub mod corepin;
pub mod hash;
pub mod identity;
pub mod interface;

use std::fmt;

/// The sentinel that encodes "no parent class".
pub const NO_PARENT: u32 = u32::MAX;

/// The reserved module path of the pinned core image.
///
/// Every module embeds one copy of the core, and every copy carries
/// the same qualified keys. A source module path never equals this
/// value, so a user class never takes a core key.
pub const CORE_MODULE: &str = "core";

/// The sentinel for an unfilled core role slot.
pub const NO_ROLE: u32 = u32::MAX;

/// The number of stable core role slots. The order is
/// `corepin::PINNED_LABELS`.
pub const CORE_ROLE_COUNT: usize = 68;

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

/// One element of an effect row in the serialized module.
///
/// `Op` names an operation or group through the module string table.
/// `Var` names one effect parameter of the enclosing function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BcRow {
    Op(u32),
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

/// One entry in the module type table.
///
/// Types reference other types by index. A canonical table only
/// references earlier entries and holds no duplicate entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BcType {
    Unit,
    Bool,
    Int,
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
    /// One type parameter of the enclosing generic function.
    Var(u32),
    /// The frozen machine `Fault` value type.
    Fault,
    /// The opaque pending-request token type.
    Request,
    /// The holder-local policy-table handle type.
    PolicyTable,
    /// The unloaded virtual machine handle type.
    EmptyVm,
    /// A loaded virtual machine typed by its terminal result index.
    Vm(u32),
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
    /// One verified snapshot image with no checked result type.
    SnapshotImage,
    /// One snapshot of a machine world, typed by the terminal result
    /// type index of its root machine.
    Snapshot(u32),
    /// Immutable binary data.
    Bytes,
    /// A typed file resource designator.
    FileHandle,
    /// A holder-local resource-management designator.
    ResourceHandle,
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
pub enum Instr {
    /// Push the unit value.
    ConstUnit,
    /// Push a Bool constant.
    ConstBool(bool),
    /// Push an Int constant.
    ConstInt(i64),
    /// Allocate the module string with this pool index and push it.
    ConstStr(u32),
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
    /// Reference identity equality for heap objects.
    EqRef,
    /// Structural equality for a sealed enum value: the same arm and
    /// equal fields. The walk keeps its own stack.
    EqValue,
    NeValue,
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
    /// Pop a value, a key, and a map, then insert or replace. Pushes unit.
    MapPut,
    /// Pop an object reference, freeze its graph, push the same reference.
    Freeze,
    /// Pop a frozen object reference and push its canonical digest.
    Digest,
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
    AsCall(u32),
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
    /// The runtime backstop behind a proven-exhaustive `case`. It
    /// faults if executed. Ends the block.
    Unreachable,
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
    /// Pop a Substring and push a bounded String.
    SubstringToString,
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
    /// Pop an index and bytes, then push the byte as an Int.
    BytesAt,
    /// Pop an index and bytes, then push the byte or -1.
    BytesGet,
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
}

impl Instr {
    /// Return true when the instruction ends a basic block.
    pub fn is_terminator(&self) -> bool {
        matches!(self, Instr::Jump(_) | Instr::Return | Instr::Unreachable)
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
}

impl ImportKind {
    fn tag(self) -> u8 {
        match self {
            ImportKind::Class => 0,
            ImportKind::Ctor => 1,
            ImportKind::Method => 2,
            ImportKind::Func => 3,
        }
    }

    /// True when the slot declares a function, not a class.
    pub fn is_func(self) -> bool {
        !matches!(self, ImportKind::Class)
    }
}

/// One named import slot.
///
/// A slot declares a definition that another module provides. The
/// local definition it names carries the signature only: an imported
/// function has no blocks, and an imported class has no method
/// bodies. The pinned hash is the interface hash of the provider
/// export. The linker resolves the slot by module path and name, and
/// rejects a provider whose interface hash differs from the pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// The providing module path, for example `mathlib.matrix`.
    pub module: String,
    /// The exported name, for example `Matrix` or `Matrix.scale`.
    pub name: String,
    pub kind: ImportKind,
    /// The local class or function index this slot declares.
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
}

impl ExportKind {
    pub fn tag(self) -> u8 {
        match self {
            ExportKind::Function => 0,
            ExportKind::Class => 1,
            ExportKind::Enum => 2,
            ExportKind::EnumCase => 3,
        }
    }

    pub fn from_tag(tag: u8) -> Option<ExportKind> {
        match tag {
            0 => Some(ExportKind::Function),
            1 => Some(ExportKind::Class),
            2 => Some(ExportKind::Enum),
            3 => Some(ExportKind::EnumCase),
            _ => None,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            ExportKind::Function => "fn",
            ExportKind::Class => "class",
            ExportKind::Enum => "enum",
            ExportKind::EnumCase => "case",
        }
    }

    /// True when the export names a class-like definition.
    pub fn is_class(self) -> bool {
        !matches!(self, ExportKind::Function)
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
    /// A key alone ties a constructor to nothing. An earlier rule
    /// proved only that a binding with the constructor key existed, so
    /// the binding named any function of the module, an import slot
    /// included. Two providers then hid two constructors behind one
    /// harmless binding, and the conflict rule never fired. This field
    /// makes the tie explicit, and the linker proves it.
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

/// One exported top-level definition of the source module.
///
/// The table names the definitions another module may import. It
/// excludes the embedded core copy and every imported declaration, so
/// the linker never resolves an import to a definition the module did
/// not define.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub kind: ExportKind,
    pub name: String,
    /// The class index or the function index.
    pub def: u32,
    /// The construction function index of a class export, or
    /// `NO_CTOR`.
    pub ctor: u32,
}

/// One decoded module.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub strings: Vec<String>,
    pub types: Vec<BcType>,
    /// Global selector names in first-encounter order.
    pub selectors: Vec<String>,
    /// Type applications referenced by generic call and allocation
    /// sites.
    pub apps: Vec<TypeApp>,
    /// The import slots, in declaration order. An empty table marks a
    /// linked module, which is the only kind the loader admits.
    pub imports: Vec<Import>,
    /// The stable core role slots: one class index per role, or
    /// `NO_ROLE`. The compiler fills the table, the linker relocates
    /// it, and the verifier proves the shape of every filled slot.
    /// The verifier and the VM then read slots, never a source name
    /// and never a definition hash.
    pub core_roles: [u32; CORE_ROLE_COUNT],
    pub classes: Vec<BcClass>,
    pub funcs: Vec<Func>,
    /// Index of the entry function.
    pub entry: u32,
    /// The exported top-level definitions. The export section holds
    /// this table, so it stays outside the semantic region.
    pub exports: Vec<Export>,
    /// The named function bindings. Each entry maps a qualified name
    /// to a function value. The export section holds this table too,
    /// so a binding key never reaches the verifier and never enters a
    /// structural hash.
    pub bindings: Vec<FuncBinding>,
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
/// Version 11 adds the `Digest` type and the three digest
/// instructions. Version 13 adds the `SnapshotImage` and `Snapshot`
/// types. Version 14 adds the reply type index of the two perform
/// instructions. Version 15 adds bytes, file handles, resource
/// controls, and three byte instructions. Every earlier tag keeps its
/// byte, so each change adds encodings and moves none. Version 16 adds
/// final class flags. Version 17 adds the `Int` core role. Version 18
/// adds the `Bool` core role. Version 19 adds the String core role and
/// immutable String instructions. Version 20 adds Bytes and builder
/// core roles. It also adds their native instructions. Version 21
/// adds Text, Substring, Char, shared storage, and move instructions.
/// Version 22 adds the text extraction and parsing instructions and
/// the two structural enum equality instructions.
pub const VERSION: u16 = 22;

/// The byte length of the container header: the magic, the version,
/// and the three section-table entries (offset and length each).
const HEADER_LEN: usize = 4 + 2 + 3 * 8;

// Opcode bytes for the serialized form.
const OP_CONST_UNIT: u8 = 0x00;
const OP_CONST_BOOL: u8 = 0x01;
const OP_CONST_INT: u8 = 0x02;
const OP_CONST_STR: u8 = 0x03;
const OP_LOAD_LOCAL: u8 = 0x04;
const OP_STORE_LOCAL: u8 = 0x05;
const OP_POP: u8 = 0x06;
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
const OP_EQ_VALUE: u8 = 0xb4;
const OP_NE_VALUE: u8 = 0xb5;
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
const OP_SUBSTRING_TO_STRING: u8 = 0x93;
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
const TY_EMPTY_VM: u8 = 16;
const TY_VM: u8 = 17;
const TY_PENDING_CALL: u8 = 18;
const TY_OP: u8 = 19;
const TY_DIGEST: u8 = 20;
const TY_HANDLE: u8 = 21;
const TY_SNAPSHOT_IMAGE: u8 = 22;
const TY_SNAPSHOT: u8 = 23;
const TY_BYTES: u8 = 24;
const TY_FILE_HANDLE: u8 = 25;
const TY_RESOURCE_HANDLE: u8 = 26;
const TY_WAIT: u8 = 27;

// Row element tags.
const ROW_OP: u8 = 0;
const ROW_VAR: u8 = 1;

// Class kind tags.
const KIND_NORMAL: u8 = 0;
const KIND_ABSTRACT: u8 = 1;
const KIND_CASE: u8 = 2;

/// Encode a module into the sectioned container form.
///
/// The container holds the magic and version header, a section
/// table, the semantic region, the export section with the
/// definition names and the function bindings, and an empty reserved
/// debug section. The semantic bytes of a definition do not contain
/// its own name.
pub fn encode(module: &Module) -> Vec<u8> {
    let semantic = encode_semantic(module);
    let exports = encode_exports(module);
    let debug: Vec<u8> = Vec::new();
    let mut out = Vec::with_capacity(HEADER_LEN + semantic.len() + exports.len() + debug.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    let mut offset = HEADER_LEN as u32;
    for section in [&semantic, &exports, &debug] {
        write_u32(&mut out, offset);
        write_u32(&mut out, section.len() as u32);
        offset += section.len() as u32;
    }
    out.extend_from_slice(&semantic);
    out.extend_from_slice(&exports);
    out.extend_from_slice(&debug);
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

/// Encode the semantic region: every table except the definition
/// names.
fn encode_semantic(module: &Module) -> Vec<u8> {
    let mut out = Vec::new();
    write_u32(&mut out, module.strings.len() as u32);
    for s in &module.strings {
        write_bytes(&mut out, s.as_bytes());
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
    write_u32(&mut out, module.imports.len() as u32);
    for import in &module.imports {
        write_bytes(&mut out, import.module.as_bytes());
        write_bytes(&mut out, import.name.as_bytes());
        out.push(import.kind.tag());
        write_u32(&mut out, import.def);
        out.extend_from_slice(&import.hash);
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

/// Encode the export section: the definition names and the class
/// qualified keys in definition index order, classes first, then
/// functions, and then the exported top-level definitions.
fn encode_exports(module: &Module) -> Vec<u8> {
    let mut out = Vec::new();
    write_u32(&mut out, module.classes.len() as u32);
    for class in &module.classes {
        write_bytes(&mut out, class.name.as_bytes());
        write_bytes(&mut out, class.key.as_bytes());
    }
    write_u32(&mut out, module.funcs.len() as u32);
    for func in &module.funcs {
        write_bytes(&mut out, func.name.as_bytes());
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
        write_u32(&mut out, export.def);
        write_u32(&mut out, export.ctor);
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
            BcRow::Var(idx) => {
                out.push(ROW_VAR);
                write_u32(out, *idx);
            }
        }
    }
}

fn encode_type(out: &mut Vec<u8>, ty: &BcType) {
    match ty {
        BcType::Unit => out.push(TY_UNIT),
        BcType::Bool => out.push(TY_BOOL),
        BcType::Int => out.push(TY_INT),
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
        BcType::Var(i) => {
            out.push(TY_VAR);
            write_u32(out, *i);
        }
        BcType::Digest => out.push(TY_DIGEST),
        BcType::Fault => out.push(TY_FAULT),
        BcType::Request => out.push(TY_REQUEST),
        BcType::PolicyTable => out.push(TY_POLICY_TABLE),
        BcType::EmptyVm => out.push(TY_EMPTY_VM),
        BcType::Vm(t) => {
            out.push(TY_VM);
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
        BcType::SnapshotImage => out.push(TY_SNAPSHOT_IMAGE),
        BcType::Snapshot(t) => {
            out.push(TY_SNAPSHOT);
            write_u32(out, *t);
        }
        BcType::Bytes => out.push(TY_BYTES),
        BcType::FileHandle => out.push(TY_FILE_HANDLE),
        BcType::ResourceHandle => out.push(TY_RESOURCE_HANDLE),
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
        Instr::ConstStr(idx) => {
            out.push(OP_CONST_STR);
            write_u32(out, *idx);
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
        Instr::Native(NativeInstr::SubstringToString) => out.push(OP_SUBSTRING_TO_STRING),
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
        Instr::MapPut => out.push(OP_MAP_PUT),
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
        Instr::Native(NativeInstr::LtBytes) => out.push(OP_LT_BYTES),
        Instr::Native(NativeInstr::LeBytes) => out.push(OP_LE_BYTES),
        Instr::Native(NativeInstr::GtBytes) => out.push(OP_GT_BYTES),
        Instr::Native(NativeInstr::GeBytes) => out.push(OP_GE_BYTES),
        Instr::Native(NativeInstr::BbExtend) => out.push(OP_BB_EXTEND),
        Instr::Native(NativeInstr::BbReserve) => out.push(OP_BB_RESERVE),
        Instr::Native(NativeInstr::BbClear) => out.push(OP_BB_CLEAR),
        Instr::Native(NativeInstr::BytesNew) => out.push(OP_BYTES_NEW),
        Instr::Native(NativeInstr::BytesLen) => out.push(OP_BYTES_LEN),
        Instr::Native(NativeInstr::BytesText) => out.push(OP_BYTES_TEXT),
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
        Instr::Digest => out.push(OP_DIGEST),
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
        Instr::AsCall(op) => {
            out.push(OP_AS_CALL);
            write_u32(out, *op);
        }
        Instr::CallArgs => out.push(OP_CALL_ARGS),
        Instr::FaultCode => out.push(OP_FAULT_CODE),
        Instr::FaultDenied => out.push(OP_FAULT_DENIED),
        Instr::RequestOp => out.push(OP_REQUEST_OP),
        Instr::Unreachable => out.push(OP_UNREACHABLE),
    }
}

/// A structural decode failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The stream ended before the structure was complete.
    Truncated,
    BadMagic,
    BadVersion(u16),
    BadOpcode(u8),
    BadTypeTag(u8),
    BadRowTag(u8),
    BadClassKind(u8),
    /// A `mut` flag byte is not 0 or 1.
    BadFlag(u8),
    BadUtf8,
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
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Truncated => write!(f, "the byte stream is truncated"),
            DecodeError::BadMagic => write!(f, "the magic header is not `LMBC`"),
            DecodeError::BadVersion(v) => write!(f, "unsupported bytecode version {v}"),
            DecodeError::BadOpcode(op) => write!(f, "unknown opcode byte 0x{op:02x}"),
            DecodeError::BadTypeTag(t) => write!(f, "unknown type tag {t}"),
            DecodeError::BadRowTag(t) => write!(f, "unknown row element tag {t}"),
            DecodeError::BadClassKind(t) => write!(f, "unknown class kind tag {t}"),
            DecodeError::BadFlag(v) => write!(f, "invalid flag byte {v}"),
            DecodeError::BadUtf8 => write!(f, "a string is not valid UTF-8"),
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
        }
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
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
    let mut row = Vec::with_capacity(count);
    for _ in 0..count {
        let tag = cur.u8()?;
        let elem = match tag {
            ROW_OP => BcRow::Op(cur.u32()?),
            ROW_VAR => BcRow::Var(cur.u32()?),
            other => return Err(DecodeError::BadRowTag(other)),
        };
        row.push(elem);
    }
    Ok(row)
}

/// Decode a serialized container. This checks structure only.
///
/// The section table is validated with plain arithmetic before any
/// section is read, so a claimed size that disagrees with the actual
/// byte count rejects before any allocation is sized from it.
pub fn decode(bytes: &[u8]) -> Result<Module, DecodeError> {
    let mut cur = Cursor { bytes, pos: 0 };
    if cur.take(4)? != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let version = cur.u16()?;
    if version != VERSION {
        return Err(DecodeError::BadVersion(version));
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
    // The debug section is reserved. Its content is ignored.
    let mut module = decode_semantic(&bytes[sem_at..sem_at + sem_len])?;
    decode_exports(&bytes[exp_at..exp_at + exp_len], &mut module)?;
    Ok(module)
}

/// Decode the export section: the definition names, the function
/// bindings, and the exported top-level definitions. Every index is
/// checked against the tables the semantic region already produced.
fn decode_exports(bytes: &[u8], module: &mut Module) -> Result<(), DecodeError> {
    let mut cur = Cursor { bytes, pos: 0 };
    let class_count = cur.len()?;
    if class_count != module.classes.len() {
        return Err(DecodeError::ExportCountMismatch);
    }
    for class in &mut module.classes {
        class.name = cur.string()?;
        class.key = cur.string()?;
    }
    let func_count = cur.len()?;
    if func_count != module.funcs.len() {
        return Err(DecodeError::ExportCountMismatch);
    }
    for func in &mut module.funcs {
        func.name = cur.string()?;
    }
    // One encoded binding needs at least twelve bytes: the key
    // length, the function index, and the class index. `len` bounds a
    // count at one byte per entry, which is not enough to size this
    // allocation. Check the real cost before the reserve.
    let binding_count = cur.len()?;
    if binding_count > cur.remaining() / 12 {
        return Err(DecodeError::BadLength);
    }
    let mut bindings = Vec::with_capacity(binding_count);
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
    let mut exports = Vec::with_capacity(export_count);
    for _ in 0..export_count {
        let kind = ExportKind::from_tag(cur.u8()?).ok_or(DecodeError::BadExport)?;
        let name = cur.string()?;
        let def = cur.u32()?;
        let ctor = cur.u32()?;
        let limit = if kind.is_class() {
            module.classes.len()
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
            def,
            ctor,
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
    let mut strings = Vec::with_capacity(string_count);
    for _ in 0..string_count {
        strings.push(cur.string()?);
    }
    let type_count = cur.len()?;
    let mut types = Vec::with_capacity(type_count);
    for _ in 0..type_count {
        types.push(decode_type(&mut cur)?);
    }
    let selector_count = cur.len()?;
    let mut selectors = Vec::with_capacity(selector_count);
    for _ in 0..selector_count {
        selectors.push(cur.string()?);
    }
    let app_count = cur.len()?;
    let mut apps = Vec::with_capacity(app_count);
    for _ in 0..app_count {
        let ty_count = cur.len()?;
        let mut app_types = Vec::with_capacity(ty_count);
        for _ in 0..ty_count {
            app_types.push(cur.u32()?);
        }
        let row_count = cur.len()?;
        let mut rows = Vec::with_capacity(row_count);
        for _ in 0..row_count {
            rows.push(decode_row(&mut cur)?);
        }
        apps.push(TypeApp {
            types: app_types,
            rows,
        });
    }
    let import_count = cur.len()?;
    let mut imports = Vec::with_capacity(import_count);
    for _ in 0..import_count {
        let module_path = cur.string()?;
        let name = cur.string()?;
        let kind = match cur.u8()? {
            0 => ImportKind::Class,
            1 => ImportKind::Ctor,
            2 => ImportKind::Method,
            3 => ImportKind::Func,
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
    let mut core_roles = [NO_ROLE; CORE_ROLE_COUNT];
    for slot in &mut core_roles {
        *slot = cur.u32()?;
    }
    let class_count = cur.len()?;
    let mut classes = Vec::with_capacity(class_count);
    for _ in 0..class_count {
        let parent = cur.u32()?;
        let parent_arg_count = cur.len()?;
        let mut parent_args = Vec::with_capacity(parent_arg_count);
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
        let field_count = cur.len()?;
        let mut fields = Vec::with_capacity(field_count);
        for _ in 0..field_count {
            let fname = cur.string()?;
            let fty = cur.u32()?;
            fields.push((fname, fty));
        }
        let method_count = cur.len()?;
        let mut methods = Vec::with_capacity(method_count);
        for _ in 0..method_count {
            let sel = cur.u32()?;
            let func = cur.u32()?;
            methods.push((sel, func));
        }
        classes.push(BcClass {
            name: String::new(),
            key: String::new(),
            is_final,
            parent,
            parent_args,
            type_params,
            kind,
            fields,
            methods,
        });
    }
    let func_count = cur.len()?;
    let mut funcs = Vec::with_capacity(func_count);
    for _ in 0..func_count {
        let type_params = cur.u32()?;
        let effect_params = cur.u32()?;
        let param_count = cur.len()?;
        let mut params = Vec::with_capacity(param_count);
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
        let mut param_muts = Vec::with_capacity(mut_count);
        for _ in 0..mut_count {
            param_muts.push(cur.flag()?);
        }
        let ret = cur.u32()?;
        let row = decode_row(&mut cur)?;
        let capture_count = cur.len()?;
        let mut captures = Vec::with_capacity(capture_count);
        for _ in 0..capture_count {
            captures.push(cur.u32()?);
        }
        // The local-type table count passes the length guard, so the
        // allocation is bounded by the input size.
        let local_count = cur.len()?;
        let mut local_types = Vec::with_capacity(local_count);
        for _ in 0..local_count {
            local_types.push(cur.u32()?);
        }
        let block_count = cur.len()?;
        let mut blocks = Vec::with_capacity(block_count);
        for _ in 0..block_count {
            let instr_count = cur.len()?;
            let mut block = Vec::with_capacity(instr_count);
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
            ret,
            row,
            captures,
            local_types,
            blocks,
        });
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
        types,
        selectors,
        apps,
        imports,
        core_roles,
        classes,
        funcs,
        entry,
        exports: Vec::new(),
        bindings: Vec::new(),
    })
}

fn decode_type(cur: &mut Cursor<'_>) -> Result<BcType, DecodeError> {
    let tag = cur.u8()?;
    let ty = match tag {
        TY_UNIT => BcType::Unit,
        TY_BOOL => BcType::Bool,
        TY_INT => BcType::Int,
        TY_STR => BcType::Str,
        TY_CLASS => BcType::Class(cur.u32()?),
        TY_INST => {
            let class = cur.u32()?;
            let count = cur.len()?;
            let mut args = Vec::with_capacity(count);
            for _ in 0..count {
                args.push(cur.u32()?);
            }
            BcType::Inst(class, args)
        }
        TY_LIST => BcType::List(cur.u32()?),
        TY_MAP => BcType::Map(cur.u32()?, cur.u32()?),
        TY_TUPLE => {
            let count = cur.len()?;
            let mut elems = Vec::with_capacity(count);
            for _ in 0..count {
                elems.push(cur.u32()?);
            }
            BcType::Tuple(elems)
        }
        TY_FN => {
            let count = cur.len()?;
            let mut params = Vec::with_capacity(count);
            for _ in 0..count {
                params.push(cur.u32()?);
            }
            let mut_count = cur.len()?;
            if mut_count != count {
                return Err(DecodeError::MutMarkerCount);
            }
            let mut muts = Vec::with_capacity(mut_count);
            for _ in 0..mut_count {
                muts.push(cur.flag()?);
            }
            let ret = cur.u32()?;
            let row = decode_row(cur)?;
            BcType::Fn(params, muts, ret, row)
        }
        TY_VAR => BcType::Var(cur.u32()?),
        TY_DIGEST => BcType::Digest,
        TY_FAULT => BcType::Fault,
        TY_REQUEST => BcType::Request,
        TY_POLICY_TABLE => BcType::PolicyTable,
        TY_EMPTY_VM => BcType::EmptyVm,
        TY_VM => BcType::Vm(cur.u32()?),
        TY_WAIT => BcType::Wait(cur.u32()?),
        TY_PENDING_CALL => BcType::PendingCall(cur.u32()?, cur.u32()?),
        TY_HANDLE => BcType::Handle(cur.u32()?, cur.u32()?),
        TY_SNAPSHOT_IMAGE => BcType::SnapshotImage,
        TY_SNAPSHOT => BcType::Snapshot(cur.u32()?),
        TY_BYTES => BcType::Bytes,
        TY_FILE_HANDLE => BcType::FileHandle,
        TY_RESOURCE_HANDLE => BcType::ResourceHandle,
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
        OP_CONST_STR => Instr::ConstStr(cur.u32()?),
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
        OP_SUBSTRING_TO_STRING => Instr::Native(NativeInstr::SubstringToString),
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
        OP_CALL_VALUE => Instr::CallValue { argc: cur.u32()? },
        OP_MAKE_CLOSURE => Instr::MakeClosure {
            func: cur.u32()?,
            captures: cur.u32()?,
        },
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
        OP_MAP_PUT => Instr::MapPut,
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
        OP_LT_BYTES => Instr::Native(NativeInstr::LtBytes),
        OP_LE_BYTES => Instr::Native(NativeInstr::LeBytes),
        OP_GT_BYTES => Instr::Native(NativeInstr::GtBytes),
        OP_GE_BYTES => Instr::Native(NativeInstr::GeBytes),
        OP_BB_EXTEND => Instr::Native(NativeInstr::BbExtend),
        OP_BB_RESERVE => Instr::Native(NativeInstr::BbReserve),
        OP_BB_CLEAR => Instr::Native(NativeInstr::BbClear),
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
        OP_DIGEST => Instr::Digest,
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
        OP_TABLE_EDIT => Instr::TableEdit {
            action: cur.u32()?,
            kind: cur.u32()?,
            slot: cur.u32()?,
        },
        OP_AS_CALL => Instr::AsCall(cur.u32()?),
        OP_CALL_ARGS => Instr::CallArgs,
        OP_FAULT_CODE => Instr::FaultCode,
        OP_FAULT_DENIED => Instr::FaultDenied,
        OP_REQUEST_OP => Instr::RequestOp,
        OP_UNREACHABLE => Instr::Unreachable,
        other => return Err(DecodeError::BadOpcode(other)),
    };
    Ok(instr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_func(name: &str, ret: u32, blocks: Vec<Vec<Instr>>) -> Func {
        Func {
            name: name.to_string(),
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
            strings: vec!["hello".to_string(), "Io.Print".to_string()],
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
            classes: vec![
                BcClass {
                    name: "Counter".to_string(),
                    parent_args: Vec::new(),
                    key: "Counter".to_string(),
                    is_final: false,
                    parent: NO_PARENT,
                    type_params: 0,
                    kind: BcClassKind::Normal,
                    fields: vec![("value".to_string(), 1)],
                    methods: vec![(0, 1)],
                },
                BcClass {
                    name: "Box".to_string(),
                    parent_args: Vec::new(),
                    key: "Box".to_string(),
                    is_final: false,
                    parent: NO_PARENT,
                    type_params: 1,
                    kind: BcClassKind::Normal,
                    fields: vec![("value".to_string(), 9)],
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
    fn encode_decode_round_trip() {
        let module = sample_module();
        let bytes = encode(&module);
        assert_eq!(decode(&bytes).unwrap(), module);
    }

    #[test]
    fn round_trips_every_instruction() {
        let instrs = vec![
            Instr::ConstUnit,
            Instr::ConstBool(true),
            Instr::ConstInt(-5),
            Instr::ConstStr(0),
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
            Instr::CallValue { argc: 2 },
            Instr::MakeClosure {
                func: 1,
                captures: 0,
            },
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
            Instr::MapPut,
            Instr::Native(NativeInstr::SbNew),
            Instr::Native(NativeInstr::SbAppendStr),
            Instr::Native(NativeInstr::SbAppendInt),
            Instr::Native(NativeInstr::SbAppendBool),
            Instr::Native(NativeInstr::SbBuild),
            Instr::Native(NativeInstr::BbNew),
            Instr::Native(NativeInstr::BbAppend),
            Instr::Native(NativeInstr::BbLen),
            Instr::Native(NativeInstr::BbBuild),
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
            types: vec![BcType::Unit],
            selectors: vec![],
            apps: vec![],
            classes: vec![],
            funcs: vec![],
            imports: vec![],
            core_roles: [NO_ROLE; CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
        };
        let mut bytes = encode(&module);
        // The single type tag sits directly after the string count
        // and the type count at the semantic region start.
        let pos = HEADER_LEN + 4 + 4;
        assert_eq!(bytes[pos], TY_UNIT);
        bytes[pos] = 0xee;
        assert_eq!(decode(&bytes), Err(DecodeError::BadTypeTag(0xee)));
    }

    #[test]
    fn section_offsets_must_be_contiguous() {
        let bytes = encode(&sample_module());
        // Shift the semantic offset forward by one.
        let mut corrupt = bytes.clone();
        let offset = u32::from_le_bytes(corrupt[6..10].try_into().unwrap());
        corrupt[6..10].copy_from_slice(&(offset + 1).to_le_bytes());
        assert_eq!(decode(&corrupt), Err(DecodeError::BadSectionTable));
        // Grow the semantic length so the sections overlap the input
        // end.
        let mut corrupt = bytes.clone();
        let len = u32::from_le_bytes(corrupt[10..14].try_into().unwrap());
        corrupt[10..14].copy_from_slice(&(len + 1).to_le_bytes());
        assert_eq!(decode(&corrupt), Err(DecodeError::BadSectionTable));
        // Shrink the semantic length: the export offset no longer
        // lines up.
        let mut corrupt = bytes;
        let len = u32::from_le_bytes(corrupt[10..14].try_into().unwrap());
        corrupt[10..14].copy_from_slice(&(len - 1).to_le_bytes());
        assert_eq!(decode(&corrupt), Err(DecodeError::BadSectionTable));
    }

    #[test]
    fn every_section_boundary_truncation_is_rejected() {
        let bytes = encode(&sample_module());
        // Read the section table for the boundary positions.
        let mut boundaries = vec![HEADER_LEN];
        for i in 0..3 {
            let at = 6 + i * 8;
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
        // The export section starts with the class-name count.
        let exp_at = u32::from_le_bytes(bytes[14..18].try_into().unwrap()) as usize;
        let mut corrupt = bytes.clone();
        corrupt[exp_at..exp_at + 4].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            decode(&corrupt),
            Err(DecodeError::ExportCountMismatch) | Err(DecodeError::BadLength)
        ));
    }

    #[test]
    fn names_live_in_the_export_section_only() {
        // The semantic region must not contain the definition names.
        let module = sample_module();
        let bytes = encode(&module);
        let sem_at = u32::from_le_bytes(bytes[6..10].try_into().unwrap()) as usize;
        let sem_len = u32::from_le_bytes(bytes[10..14].try_into().unwrap()) as usize;
        let semantic = &bytes[sem_at..sem_at + sem_len];
        for name in ["Counter", "Box", "main"] {
            let found = semantic.windows(name.len()).any(|w| w == name.as_bytes());
            assert!(!found, "the semantic region contains the name {name}");
        }
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
