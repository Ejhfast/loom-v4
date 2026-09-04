//! Typed HIR for Loom programs.
//!
//! Every expression carries its checked type and its reference
//! capability. Names are resolved to dense local slots, capture
//! indices, function indices, class indices, and field layout
//! indices. Generic calls carry their resolved type and row
//! arguments. Later phases never repeat a textual lookup, except for
//! method selectors, which the lowering pass interns into dense
//! selector slots.

use lm_source::ast::BinOp;
use lm_types::{ClassKind, Row, TypeId, TypeStore};
use std::collections::BTreeSet;

/// The pinned core definition indices inside one checked module.
///
/// The core image is ordinary source. The compiler records each core
/// index so `List.get` can name the pinned `Option` identity.
#[derive(Debug, Clone, Copy)]
pub struct CoreIds {
    /// Class index of the `Option` enum parent.
    pub option_class: u32,
    /// Class index of the `Option.Some` case.
    pub some_class: u32,
    /// Class index of the `Option.None` case.
    pub none_class: u32,
    /// Interface index of `PartialEq`.
    pub partial_eq_interface: u32,
    /// Method index of `PartialEq.__eq__`.
    pub partial_eq_method: u32,
    /// Interface index of `Hashable`.
    pub hashable_interface: u32,
    /// Method index of `Hashable.__hash__`.
    pub hashable_method: u32,
}

/// One exported top-level definition of the source module.
#[derive(Clone)]
pub struct HirExport {
    pub kind: lm_bytecode::ExportKind,
    pub name: String,
    /// True when this entry describes one top-level source declaration.
    pub source: bool,
    /// The class index for a class-like export, the function index
    /// otherwise.
    pub def: u32,
}

/// One checked module constant.
#[derive(Clone)]
pub struct HirConst {
    pub name: String,
    pub ty: TypeId,
    pub value: HExpr,
}

/// The definition one import slot declares. The lowering pass turns
/// it into a bytecode index; a constructor index follows the function
/// table, so only the lowering knows it.
#[derive(Debug, Clone, Copy)]
pub enum HirImportDef {
    Class(u32),
    Func(u32),
    /// The construction function of the given class.
    Ctor(u32),
    /// A compile-time constant has no runtime definition.
    Constant,
}

/// One relocated definition in a reflected source module.
#[derive(Debug, Clone, Copy)]
pub enum HirReflectionDef {
    Function(u32),
    Class(u32),
    Interface(u32),
}

/// One source declaration in a reflected module.
#[derive(Clone)]
pub struct HirReflectionDeclaration {
    pub kind: lm_bytecode::ExportKind,
    pub name: String,
    pub def: Option<HirReflectionDef>,
    pub callable: Option<u32>,
    pub constant: Option<HirConst>,
}

/// One exact source module surface.
#[derive(Clone)]
pub struct HirReflectionModule {
    pub name: String,
    pub declarations: Vec<HirReflectionDeclaration>,
}

/// One import slot the module needs.
#[derive(Debug, Clone)]
pub struct HirImport {
    pub module: String,
    pub name: String,
    pub kind: lm_bytecode::ImportKind,
    pub def: HirImportDef,
    /// The pinned interface hash of the provider export.
    pub hash: [u8; 32],
}

/// A checked module. The entry statements form one function.
pub struct HirModule {
    /// The exact operation bundle used during checking.
    pub bundle: std::sync::Arc<lm_abi::AbiBundle>,
    pub store: TypeStore,
    pub interfaces: Vec<HirInterface>,
    pub conformances: Vec<HirConformance>,
    pub classes: Vec<HirClass>,
    pub funcs: Vec<HirFunc>,
    /// Literal constants that the compiler adds to the interface.
    pub constants: Vec<HirConst>,
    /// Index of the entry function inside `funcs`.
    pub entry: usize,
    /// Pinned core definition indices.
    pub core: CoreIds,
    /// The stable core role slots: one class index per role, in
    /// `lm_bytecode::corepin::PINNED_LABELS` order.
    pub core_roles: [u32; lm_bytecode::CORE_ROLE_COUNT],
    /// The exported top-level definitions, in declaration order.
    pub exports: Vec<HirExport>,
    /// The definitions that the separate core unit exports.
    pub core_exports: Vec<HirExport>,
    /// The import slots, in slot order.
    pub imports: Vec<HirImport>,
    /// The named function bindings of the declared functions, the
    /// methods, and the initializers. Lowering appends one binding per
    /// generated constructor.
    pub bindings: Vec<lm_bytecode::FuncBinding>,
    /// Local functions that enter the portable installation surface.
    pub reified_functions: BTreeSet<u32>,
    /// Local classes that enter the portable installation surface.
    pub reified_classes: BTreeSet<u32>,
    /// Source module surfaces used by `codeof` expressions.
    pub reflections: Vec<HirReflectionModule>,
}

/// One applied nominal interface before bytecode type interning.
#[derive(Debug, Clone)]
pub struct HirInterfaceUse {
    pub interface: u32,
    pub types: Vec<TypeId>,
    pub rows: Vec<Row>,
}

/// One associated type requirement before bytecode lowering.
pub struct HirAssociated {
    pub name: String,
    pub bounds: Vec<HirInterfaceUse>,
}

/// One interface method requirement before bytecode lowering.
pub struct HirInterfaceMethod {
    pub selector: String,
    pub mut_self: bool,
    pub type_params: u32,
    pub type_bounds: Vec<Vec<HirInterfaceUse>>,
    pub effect_params: u32,
    pub premises: Vec<HirTypePremise>,
    pub params: Vec<TypeId>,
    pub param_muts: Vec<bool>,
    pub param_names: Vec<String>,
    pub ret: TypeId,
    pub row: Row,
    pub default: Option<u32>,
    pub default_binding: Option<String>,
}

/// One checked type premise in a bytecode-ready interface method.
pub struct HirTypePremise {
    pub subject: TypeId,
    pub bounds: Vec<HirInterfaceUse>,
}

/// One nominal interface before bytecode lowering.
pub struct HirInterface {
    pub name: String,
    pub key: String,
    pub type_params: u32,
    pub effect_params: u32,
    pub generic_is_effect: Vec<bool>,
    pub parents: Vec<HirInterfaceUse>,
    pub type_bounds: Vec<Vec<HirInterfaceUse>>,
    pub associated: Vec<HirAssociated>,
    pub methods: Vec<HirInterfaceMethod>,
}

/// One class-owned conformance before bytecode lowering.
pub struct HirConformancePremise {
    pub param: u32,
    pub bounds: Vec<HirInterfaceUse>,
}

/// One class-owned conformance before bytecode lowering.
pub struct HirConformance {
    pub class: u32,
    pub application: HirInterfaceUse,
    pub premises: Vec<HirConformancePremise>,
    pub associated: Vec<TypeId>,
    /// One entry per interface method.
    /// True selects compatible class dispatch. False selects the default.
    pub method_overrides: Vec<bool>,
}

/// How instances of one class are constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtorKind {
    /// Defaults only; no `init`.
    Defaults,
    /// Defaults, then a call of the declared `init`.
    Init,
    /// An enum case: constructor arguments store the fields directly.
    CaseFields,
}

/// The native value representation of one core class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeRepr {
    Unit,
    Int,
    Float,
    Bool,
    Text,
    String,
    Substring,
    Char,
    Bytes,
    StringBuilder,
    ByteBuffer,
    List,
    Map,
    Tuple(u8),
    FileHandle,
    TcpResource,
    TcpStream,
    TcpListener,
    TlsStream,
    UdpSocket,
    Artifact,
    VerifiedModule,
    FunctionCode,
    ClassCode,
    SlotSpec,
    CodeInstance,
    Slot,
    FunctionDef,
    ClassDef,
    FunctionBinding,
    ClassBinding,
    DynValue,
    ModuleCode,
    DeclarationCode,
    MemberCode,
    Regex,
    RegexMatch,
}

/// One checked class with its full field layout.
pub struct HirClass {
    /// True for an imported declaration: a shape with no method body
    /// and no construction body.
    pub imported: bool,
    /// The source definition range for optional debug data.
    pub source_span: Option<lm_source::Span>,
    /// True when the class cannot have a subclass.
    pub is_final: bool,
    /// True when completed instances are always frozen.
    pub is_frozen: bool,
    /// The primitive representation of a native core class.
    pub native_repr: Option<NativeRepr>,
    pub name: String,
    /// The qualified key: the nominal identity of the class. A local
    /// class takes the module path, a core class takes the reserved
    /// path `core`, and an imported class takes the path and the
    /// export name of its provider.
    pub key: String,
    /// Parent class index.
    pub parent: Option<u32>,
    /// Type arguments of a generic parent. Empty for a plain parent.
    pub parent_args: Vec<TypeId>,
    /// The number of generic type parameters.
    pub type_params: u32,
    /// Interface bounds for each class type parameter.
    pub type_bounds: Vec<Vec<HirInterfaceUse>>,
    pub kind: ClassKind,
    pub ctor_kind: CtorKind,
    /// Full layout: inherited fields first, own fields after them.
    pub field_names: Vec<String>,
    pub field_tys: Vec<TypeId>,
    /// Field default markers, aligned with the full layout.
    pub field_defaults: Vec<bool>,
    /// The first field declared by this class.
    pub own_start: u32,
    /// Default expressions aligned with the layout. `None` marks a
    /// required field.
    pub defaults: Vec<Option<HExpr>>,
    /// The checker local-slot types of each default expression,
    /// aligned with `defaults`. Lowering moves these temporaries into
    /// scratch slots of the `<new>` function and needs their types.
    pub default_locals: Vec<Vec<TypeId>>,
    /// Own method table: `(selector name, function index)`.
    pub methods: Vec<(String, u32)>,
    /// The `init` function index, when declared.
    pub init: Option<u32>,
    /// Constructor parameter names, without `self`.
    pub ctor_param_names: Vec<String>,
    /// Constructor parameter types, without `self`.
    pub ctor_params: Vec<TypeId>,
    /// Constructor parameter `mut` markers, aligned with `ctor_params`.
    pub ctor_param_muts: Vec<bool>,
    /// The row charged by construction (the `init` row).
    pub ctor_row: Row,
}

pub struct HirFunc {
    /// True for an imported declaration: a signature with no body.
    pub imported: bool,
    /// True when the pinned core source owns this function.
    pub core: bool,
    /// The source definition range for optional debug data.
    pub source_span: Option<lm_source::Span>,
    pub name: String,
    /// The number of generic type parameters in scope for the body.
    pub type_params: u32,
    pub type_bounds: Vec<Vec<HirInterfaceUse>>,
    /// The number of effect parameters in scope for the body.
    pub effect_params: u32,
    /// Parameter types. Parameters use the first local slots. A method
    /// receives `self` as parameter zero.
    pub params: Vec<TypeId>,
    /// Parameter `mut` markers, aligned with `params`.
    pub param_muts: Vec<bool>,
    /// Declared parameter names, aligned with `params` when present.
    pub param_names: Vec<String>,
    pub ret: TypeId,
    /// The declared effect row in canonical order.
    pub row: Row,
    /// Capture types. Only a closure body has captures.
    pub captures: Vec<TypeId>,
    /// All local slot types, parameters included.
    pub locals: Vec<TypeId>,
    pub body: Vec<HStmt>,
}

#[derive(Clone)]
pub enum HStmt {
    Assign {
        slot: u32,
        value: HExpr,
    },
    /// `receiver.field = value` with a resolved layout index.
    AssignField {
        recv: HExpr,
        field: u32,
        value: HExpr,
    },
    While {
        cond: HExpr,
        body: Vec<HStmt>,
    },
    For {
        source: HExpr,
        bindings: Vec<u32>,
        kind: HForKind,
        body: Vec<HStmt>,
    },
    Return {
        value: Option<HExpr>,
    },
    Break {
        value: Option<HExpr>,
    },
    Continue,
    Expr(HExpr),
}

/// One checked traversal strategy for a `for` statement.
#[derive(Clone)]
pub enum HForKind {
    List {
        source_slot: u32,
        index_slot: u32,
        epoch_slot: u32,
        element: TypeId,
    },
    Map {
        source_slot: u32,
        index_slot: u32,
        epoch_slot: u32,
        key: TypeId,
        value: TypeId,
        pair: TypeId,
    },
    Text {
        source_slot: u32,
        cursor_slot: u32,
        item: TypeId,
    },
    Range {
        source_slot: u32,
        cursor_slot: u32,
        stop_slot: u32,
    },
    Generic {
        source_slot: u32,
        iterator_slot: u32,
        option_slot: u32,
        item_slot: Option<u32>,
        iterator: HExpr,
        next: Box<HExpr>,
        some_ty: TypeId,
        item: TypeId,
    },
}

#[derive(Clone)]
pub struct HExpr {
    /// The expression's normal-completion state.
    pub flow: Flow,
    pub ty: TypeId,
    /// True when the expression yields a mutable reference.
    pub mutable: bool,
    pub kind: HExprKind,
}

/// Whether evaluation can complete normally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Normal,
    Never,
}

impl Flow {
    fn strict(items: impl IntoIterator<Item = Flow>) -> Flow {
        if items.into_iter().any(|item| item == Flow::Never) {
            Flow::Never
        } else {
            Flow::Normal
        }
    }
}

/// A native operation on a built-in collection or builder. The
/// receiver is the first argument.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NativeOp {
    ListLen,
    ListAt,
    ListPush,
    /// Non-faulting element access returning core `Option[T]`.
    ListGet,
    MapLen,
    MapHas,
    MapAt,
    MapPut,
    /// Insert through the closed Text-to-String relation.
    MapPutText,
    /// Non-faulting lookup returning core `Option[V]`.
    MapGet,
    /// Remove one entry through a borrowed lookup key.
    MapRemove,
    BytesNew,
    Freeze,
    /// The canonical digest of one frozen graph.
    Digest,
}

/// One piece of an interpolated string.
#[derive(Clone)]
pub enum HInterpPart {
    Lit(String),
    Native {
        value: HExpr,
        kind: HInterpNative,
    },
    Display {
        value: HExpr,
        interface: u32,
        method: u32,
        builder: u32,
        selector: String,
    },
}

/// One allocation-free native interpolation path.
#[derive(Clone, Copy)]
pub enum HInterpNative {
    Text,
    Int,
    Float,
    Bool,
    Char,
}

/// One policy-table edit action.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TableAction {
    Pass,
    Block,
    Mock,
    Clear,
}

/// The kind of one policy target.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TargetKind {
    /// An exact operation slot.
    Exact,
    /// A group slot.
    Group,
}

/// One native projection a pattern may read.
#[derive(Debug, Clone, PartialEq)]
pub enum Projection {
    /// The operation identity test of a request. It answers
    /// `Option[PendingCall[A, R]]`.
    AsCall(u32),
    /// `PendingCall.args()`: answers the argument tuple.
    CallArgs,
}

/// One resolved pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum HPattern {
    /// Matches anything, binds nothing.
    Wildcard,
    /// Matches anything and stores the value in a local slot.
    Bind(u32),
    Int(i64),
    Bool(bool),
    Char(char),
    Str(String),
    /// Reads one native projection of the scrutinee, then matches
    /// `inner` against the result. `ty` types the scratch slot.
    Project {
        projection: Projection,
        ty: TypeId,
        inner: Box<HPattern>,
    },
    /// Matches every sub-pattern against the same value. A request
    /// pattern uses it to bind the call and read its arguments.
    And(Vec<HPattern>),
    /// Destructures one tuple. `elem_tys` types the scratch slots.
    Tuple {
        elems: Vec<HPattern>,
        elem_tys: Vec<TypeId>,
    },
    /// Tests the final case class and destructures its fields. `ty`
    /// is the instantiated case type used by the test and the cast.
    /// `field_tys` holds the instantiated field types, aligned with
    /// `args`; lowering types the destructuring scratch slots with
    /// them.
    Ctor {
        class: u32,
        ty: TypeId,
        args: Vec<HPattern>,
        field_tys: Vec<TypeId>,
    },
}

/// One checked `case` arm.
#[derive(Clone)]
pub struct HArm {
    pub pattern: HPattern,
    pub body: Vec<HStmt>,
}

/// The declaration family accepted by one reflection arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReflectKind {
    Class,
    Function,
    Method,
    Constant,
}

/// One checked reflection arm with scoped generic parameters.
#[derive(Clone)]
pub struct HReflectArm {
    pub kind: ReflectKind,
    /// A metadata function holds the refined type and its bounds.
    pub pattern: u32,
    pub type_base: u32,
    pub effect_base: u32,
    /// The refined value enters this local slot.
    pub binding: Option<u32>,
    pub body: Vec<HStmt>,
}

#[derive(Clone)]
pub enum HExprKind {
    Unit,
    Int(i64),
    Float(u64),
    Char(char),
    Str(String),
    Bytes(Vec<u8>),
    Regex(String),
    Bool(bool),
    Local(u32),
    /// One captured value of the enclosing closure.
    Capture(u32),
    Not(Box<HExpr>),
    Neg(Box<HExpr>),
    Binary {
        op: BinOp,
        /// The shared operand type. Equality needs it to select an opcode.
        operand_ty: TypeId,
        left: Box<HExpr>,
        right: Box<HExpr>,
    },
    And(Box<HExpr>, Box<HExpr>),
    Or(Box<HExpr>, Box<HExpr>),
    /// A direct call: a top-level function, an `init`, or a
    /// superclass method. Generic calls carry their arguments.
    Call {
        func: u32,
        targs: Vec<TypeId>,
        rowargs: Vec<Row>,
        args: Vec<HExpr>,
    },
    /// Construction of a class instance.
    Construct {
        class: u32,
        targs: Vec<TypeId>,
        args: Vec<HExpr>,
    },
    /// A virtual method call through the runtime class. `own_targs`
    /// and `own_rowargs` hold the method's own generic arguments;
    /// class arguments come from the receiver type.
    MethodCall {
        recv: Box<HExpr>,
        selector: String,
        /// True when the declaring class has type parameters. The
        /// call then needs the generic instruction form, because the
        /// verifier reads the owner arguments from the class table.
        generic_owner: bool,
        own_targs: Vec<TypeId>,
        own_rowargs: Vec<Row>,
        args: Vec<HExpr>,
    },
    /// A method call selected through one nominal interface bound.
    InterfaceCall {
        recv: Box<HExpr>,
        interface: u32,
        method: u32,
        selector: String,
        own_targs: Vec<TypeId>,
        own_rowargs: Vec<Row>,
        args: Vec<HExpr>,
    },
    /// `receiver.field` with a resolved layout index.
    FieldGet {
        recv: Box<HExpr>,
        field: u32,
    },
    /// Closure creation. Captures are evaluated in the outer frame.
    /// `Class.spawn(args...)`, the sugar of specification 18.3.
    ///
    /// Lowering expands it into what a user would write: the
    /// construction function of the proc class, the `on_spawn`
    /// function, the typed argument tuple, and one `Proc.Spawn`
    /// perform.
    Spawn {
        /// The proc class. Lowering resolves its construction
        /// function index.
        class: u32,
        /// The `on_spawn` function of the proc class.
        body: u32,
        /// The function type of the construction function.
        ctor_ty: TypeId,
        /// The function type of the `on_spawn` function.
        body_ty: TypeId,
        /// The checked constructor arguments.
        args: Vec<HExpr>,
    },
    MakeClosure {
        func: u32,
        captures: Vec<HExpr>,
    },
    /// A portable view of one named function definition.
    FunctionCode {
        func: u32,
    },
    /// A portable view of one named class definition.
    ClassCode {
        class: u32,
    },
    /// Describe one exact source module surface.
    ModuleCode {
        module: u32,
    },
    /// List one module's source declarations.
    ReflectionDeclarations {
        module: Box<HExpr>,
    },
    /// List one declaration's effective methods.
    ReflectionMembers {
        declaration: Box<HExpr>,
    },
    /// Read one reflection descriptor's source name.
    ReflectionName {
        descriptor: Box<HExpr>,
    },
    /// Read one declaration descriptor's kind.
    ReflectionDeclarationKind {
        declaration: Box<HExpr>,
    },
    /// Read one member descriptor's kind.
    ReflectionMemberKind {
        member: Box<HExpr>,
    },
    /// Read optional source metadata from portable definition code.
    CodeSource {
        code: Box<HExpr>,
        element: TypeId,
    },
    /// Read stable binding data from portable definition code.
    CodeDefinition {
        code: Box<HExpr>,
    },
    /// A stack callback descriptor with a bounded lifetime.
    MakeCallback {
        func: u32,
        captures: Vec<HExpr>,
    },
    /// Convert an existing heap closure to a nonescaping callback.
    AsCallback(Box<HExpr>),
    /// A call of a closure value.
    CallValue {
        callee: Box<HExpr>,
        args: Vec<HExpr>,
    },
    /// A tuple literal. The expression type is the tuple type.
    TupleLit(Vec<HExpr>),
    /// A tuple element read at a compile-time position.
    TupleGet {
        tuple: Box<HExpr>,
        index: u32,
    },
    /// `value is Type` on a nominal instance type.
    IsType {
        value: Box<HExpr>,
        ty: TypeId,
    },
    /// `value as Type` with a runtime `BadCast` fault.
    CastType {
        value: Box<HExpr>,
        ty: TypeId,
    },
    /// A list literal. The expression type is the list type.
    ListLit(Vec<HExpr>),
    /// A map literal in source order.
    MapLit(Vec<(HExpr, HExpr)>),
    /// A native collection or builder operation.
    Native {
        op: NativeOp,
        args: Vec<HExpr>,
    },
    /// One pure operation from the intrinsic manifest.
    Intrinsic {
        intrinsic: lm_abi::IntrinsicSlot,
        args: Vec<HExpr>,
    },
    /// An interpolated string.
    Interp(Vec<HInterpPart>),
    /// One source expression lowered through internal statements.
    Block(Vec<HStmt>),
    /// A value-producing `loop` expression.
    Loop {
        body: Vec<HStmt>,
        result_slot: Option<u32>,
    },
    If {
        /// Condition and body for `if` and each `elsif`.
        arms: Vec<(HExpr, Vec<HStmt>)>,
        else_body: Option<Vec<HStmt>>,
    },
    /// A checked `case`. The scrutinee is stored in `scrut_slot`
    /// before the arm tests run.
    Case {
        scrut: Box<HExpr>,
        scrut_slot: u32,
        arms: Vec<HArm>,
    },
    /// A descriptor case with scoped type and effect binders.
    ReflectCase {
        scrut: Box<HExpr>,
        scrut_slot: u32,
        arms: Vec<HReflectArm>,
        fallback: Vec<HStmt>,
    },
    /// One `PERFORM` of an exact manifest operation. The receiver of
    /// a VM control operation is the first argument.
    Perform {
        op: u32,
        args: Vec<HExpr>,
    },
    /// Prepare one selectable source for an exact host operation.
    PrepareWait {
        op: u32,
        args: Vec<HExpr>,
    },
    /// A first-class operation value, for example `sys.io.write`.
    OpConst(u32),
    /// A policy-table edit intrinsic on a table handle.
    TableEdit {
        action: TableAction,
        kind: TargetKind,
        slot: u32,
        table: Box<HExpr>,
        /// The handler closure of a `mock` edit.
        mock: Option<Box<HExpr>>,
    },
    /// `call.args()` on a typed pending call.
    CallArgs {
        call: Box<HExpr>,
    },
    /// `fault.code()` on a fault value.
    FaultCodeGet {
        fault: Box<HExpr>,
    },
    /// `fault.site()` on a fault value.
    FaultSiteGet {
        fault: Box<HExpr>,
    },
    /// `fault.trace()` on a fault value.
    FaultTraceGet {
        fault: Box<HExpr>,
    },
    /// `request.op_name()` on a live request token.
    RequestOpName {
        request: Box<HExpr>,
    },
    /// `Fault.denied(reason)`: build one frozen `PolicyDenied` fault.
    FaultDenied {
        reason: Box<HExpr>,
    },
}

impl HExpr {
    /// Compute flow from the checked child expressions.
    pub fn finish_flow(mut self) -> HExpr {
        self.flow = self.derived_flow();
        self
    }

    /// Follow the runtime evaluation order of each operand.
    /// A mandatory operand with `Never` flow prevents normal completion.
    fn derived_flow(&self) -> Flow {
        if self.ty == lm_types::NEVER {
            return Flow::Never;
        }
        match &self.kind {
            HExprKind::Unit
            | HExprKind::Int(_)
            | HExprKind::Float(_)
            | HExprKind::Char(_)
            | HExprKind::Str(_)
            | HExprKind::Bytes(_)
            | HExprKind::Regex(_)
            | HExprKind::Bool(_)
            | HExprKind::Local(_)
            | HExprKind::Capture(_)
            | HExprKind::FunctionCode { .. }
            | HExprKind::ClassCode { .. }
            | HExprKind::ModuleCode { .. }
            | HExprKind::OpConst(_) => Flow::Normal,
            HExprKind::Not(value)
            | HExprKind::Neg(value)
            | HExprKind::AsCallback(value)
            | HExprKind::CodeSource { code: value, .. }
            | HExprKind::CodeDefinition { code: value }
            | HExprKind::ReflectionDeclarations { module: value }
            | HExprKind::ReflectionMembers { declaration: value }
            | HExprKind::ReflectionName { descriptor: value }
            | HExprKind::ReflectionDeclarationKind { declaration: value }
            | HExprKind::ReflectionMemberKind { member: value }
            | HExprKind::TupleGet { tuple: value, .. }
            | HExprKind::IsType { value, .. }
            | HExprKind::CastType { value, .. }
            | HExprKind::CallArgs { call: value }
            | HExprKind::FaultCodeGet { fault: value }
            | HExprKind::FaultSiteGet { fault: value }
            | HExprKind::FaultTraceGet { fault: value }
            | HExprKind::RequestOpName { request: value }
            | HExprKind::FaultDenied { reason: value } => value.flow,
            HExprKind::Binary { left, right, .. } => Flow::strict([left.flow, right.flow]),
            HExprKind::And(left, _) | HExprKind::Or(left, _) => left.flow,
            HExprKind::Call { args, .. }
            | HExprKind::Construct { args, .. }
            | HExprKind::Spawn { args, .. }
            | HExprKind::Native { args, .. }
            | HExprKind::Intrinsic { args, .. }
            | HExprKind::Perform { args, .. }
            | HExprKind::PrepareWait { args, .. } => Flow::strict(args.iter().map(|arg| arg.flow)),
            HExprKind::MethodCall { recv, args, .. }
            | HExprKind::InterfaceCall { recv, args, .. } => {
                Flow::strict(std::iter::once(recv.flow).chain(args.iter().map(|arg| arg.flow)))
            }
            HExprKind::FieldGet { recv, .. } => recv.flow,
            HExprKind::MakeClosure { captures, .. } | HExprKind::MakeCallback { captures, .. } => {
                Flow::strict(captures.iter().map(|capture| capture.flow))
            }
            HExprKind::CallValue { callee, args } => {
                Flow::strict(std::iter::once(callee.flow).chain(args.iter().map(|arg| arg.flow)))
            }
            HExprKind::TupleLit(items) | HExprKind::ListLit(items) => {
                Flow::strict(items.iter().map(|item| item.flow))
            }
            HExprKind::MapLit(entries) => Flow::strict(
                entries
                    .iter()
                    .flat_map(|(key, value)| [key.flow, value.flow]),
            ),
            HExprKind::Interp(parts) => Flow::strict(parts.iter().filter_map(|part| match part {
                HInterpPart::Lit(_) => None,
                HInterpPart::Native { value, .. } | HInterpPart::Display { value, .. } => {
                    Some(value.flow)
                }
            })),
            HExprKind::Block(body) => block_flow(body),
            HExprKind::Loop { .. } => Flow::Normal,
            HExprKind::If { arms, else_body } => if_flow(arms, else_body.as_deref()),
            HExprKind::Case { scrut, arms, .. } => {
                if scrut.flow == Flow::Never {
                    Flow::Never
                } else if arms.iter().any(|arm| block_flow(&arm.body) == Flow::Normal) {
                    Flow::Normal
                } else {
                    Flow::Never
                }
            }
            HExprKind::ReflectCase {
                scrut,
                arms,
                fallback,
                ..
            } => {
                if scrut.flow == Flow::Never {
                    Flow::Never
                } else if block_flow(fallback) == Flow::Normal
                    || arms.iter().any(|arm| block_flow(&arm.body) == Flow::Normal)
                {
                    Flow::Normal
                } else {
                    Flow::Never
                }
            }
            HExprKind::TableEdit { table, mock, .. } => {
                Flow::strict(std::iter::once(table.flow).chain(mock.iter().map(|value| value.flow)))
            }
        }
    }
}

impl HStmt {
    /// Return the statement's normal-completion state.
    pub fn flow(&self) -> Flow {
        match self {
            HStmt::Assign { value, .. } => value.flow,
            HStmt::AssignField { recv, value, .. } => Flow::strict([recv.flow, value.flow]),
            HStmt::While { cond, .. } => cond.flow,
            HStmt::For { source, .. } => source.flow,
            HStmt::Return { .. } | HStmt::Break { .. } | HStmt::Continue => Flow::Never,
            HStmt::Expr(expr) => expr.flow,
        }
    }

    /// Return true when control cannot continue after this statement.
    pub fn diverges(&self) -> bool {
        self.flow() == Flow::Never
    }
}

fn block_flow(body: &[HStmt]) -> Flow {
    body.iter()
        .map(HStmt::flow)
        .find(|flow| *flow == Flow::Never)
        .unwrap_or(Flow::Normal)
}

fn if_flow(arms: &[(HExpr, Vec<HStmt>)], else_body: Option<&[HStmt]>) -> Flow {
    let mut can_complete = false;
    let mut can_reach_next = true;
    for (cond, body) in arms {
        if !can_reach_next {
            break;
        }
        if cond.flow == Flow::Never {
            can_reach_next = false;
            break;
        }
        can_complete |= block_flow(body) == Flow::Normal;
    }
    if can_reach_next {
        can_complete |= else_body.is_none_or(|body| block_flow(body) == Flow::Normal);
    }
    if can_complete {
        Flow::Normal
    } else {
        Flow::Never
    }
}

/// Render the class table in a stable readable form.
pub fn dump_classes(module: &HirModule) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (idx, class) in module.classes.iter().enumerate() {
        let final_mark = if class.is_final { " (final)" } else { "" };
        let frozen_mark = if class.is_frozen { " (frozen)" } else { "" };
        let native_mark = if class.native_repr.is_some() {
            " (native)"
        } else {
            ""
        };
        let kind = match class.kind {
            ClassKind::Normal => "",
            ClassKind::EnumParent => " (enum)",
            ClassKind::EnumCase => " (case)",
        };
        match class.parent {
            Some(p) => {
                let _ = writeln!(
                    out,
                    "class {} {}{}{}{}{} < {}",
                    idx,
                    class.name,
                    final_mark,
                    frozen_mark,
                    native_mark,
                    kind,
                    module.classes[p as usize].name
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "class {} {}{}{}{}{}",
                    idx, class.name, final_mark, frozen_mark, native_mark, kind
                );
            }
        }
        for (fidx, (name, ty)) in class
            .field_names
            .iter()
            .zip(class.field_tys.iter())
            .enumerate()
        {
            let default = if class.defaults[fidx].is_some() {
                " (default)"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "  field {} {}: {}{}",
                fidx,
                name,
                module.store.display(*ty),
                default
            );
        }
        for (name, func) in &class.methods {
            let _ = writeln!(out, "  method {name} -> fn{func}");
        }
        if let Some(init) = class.init {
            let _ = writeln!(out, "  init -> fn{init}");
        }
    }
    out
}
