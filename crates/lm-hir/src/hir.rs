//! Typed HIR for the week-3 language slice.
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

/// The pinned core definition indices inside one checked module.
///
/// The core image is ordinary source. The compiler records where its
/// definitions landed so language rules such as `List.get` can name
/// the pinned `Option` identity.
#[derive(Debug, Clone, Copy)]
pub struct CoreIds {
    /// Class index of the `Option` enum parent.
    pub option_class: u32,
    /// Class index of the `Option.Some` case.
    pub some_class: u32,
    /// Class index of the `Option.None` case.
    pub none_class: u32,
}

/// One exported top-level definition of the source module.
pub struct HirExport {
    pub kind: lm_bytecode::ExportKind,
    pub name: String,
    /// The class index for a class-like export, the function index
    /// otherwise.
    pub def: u32,
    /// The structural signature the interface publishes.
    pub item: lm_bytecode::interface::IfaceItem,
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
    pub store: TypeStore,
    pub classes: Vec<HirClass>,
    pub funcs: Vec<HirFunc>,
    /// Index of the entry function inside `funcs`.
    pub entry: usize,
    /// Pinned core definition indices.
    pub core: CoreIds,
    /// The stable core role slots: one class index per role, in
    /// `lm_bytecode::corepin::PINNED_LABELS` order.
    pub core_roles: [u32; lm_bytecode::CORE_ROLE_COUNT],
    /// The exported top-level definitions, in declaration order.
    pub exports: Vec<HirExport>,
    /// The import slots, in slot order.
    pub imports: Vec<HirImport>,
    /// The named function bindings of the declared functions, the
    /// methods, and the initializers. Lowering appends one binding per
    /// generated constructor.
    pub bindings: Vec<lm_bytecode::FuncBinding>,
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
    Int,
    Bool,
    String,
    Bytes,
    StringBuilder,
    ByteBuffer,
}

/// One checked class with its full field layout.
pub struct HirClass {
    /// True for an imported declaration: a shape with no method body
    /// and no construction body.
    pub imported: bool,
    /// True when the class cannot have a subclass.
    pub is_final: bool,
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
    pub kind: ClassKind,
    pub ctor_kind: CtorKind,
    /// Full layout: inherited fields first, own fields after them.
    pub field_names: Vec<String>,
    pub field_tys: Vec<TypeId>,
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
    pub name: String,
    /// The number of generic type parameters in scope for the body.
    pub type_params: u32,
    /// The number of effect parameters in scope for the body.
    pub effect_params: u32,
    /// Parameter types. Parameters use the first local slots. A method
    /// receives `self` as parameter zero.
    pub params: Vec<TypeId>,
    /// Parameter `mut` markers, aligned with `params`.
    pub param_muts: Vec<bool>,
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
    Return {
        value: Option<HExpr>,
    },
    Break,
    Continue,
    Expr(HExpr),
}

#[derive(Clone)]
pub struct HExpr {
    pub ty: TypeId,
    /// True when the expression yields a mutable reference.
    pub mutable: bool,
    pub kind: HExprKind,
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
    /// Non-faulting lookup returning core `Option[V]`.
    MapGet,
    BytesNew,
    Freeze,
    /// The canonical digest of one frozen graph.
    Digest,
}

/// One piece of an interpolated string.
#[derive(Clone)]
pub enum HInterpPart {
    Lit(String),
    Expr(HExpr),
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

#[derive(Clone)]
pub enum HExprKind {
    Unit,
    Int(i64),
    Str(String),
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
    /// One `PERFORM` of an exact manifest operation. The receiver of
    /// a VM control operation is the first argument.
    Perform {
        op: u32,
        args: Vec<HExpr>,
    },
    /// A first-class operation value, for example `sys.io.print`.
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
    /// `request.op_name()` on a live request token.
    RequestOpName {
        request: Box<HExpr>,
    },
    /// `Fault.denied(reason)`: build one frozen `PolicyDenied` fault.
    FaultDenied {
        reason: Box<HExpr>,
    },
}

impl HStmt {
    /// Return true when control cannot continue after this statement.
    pub fn diverges(&self) -> bool {
        match self {
            HStmt::Return { .. } | HStmt::Break | HStmt::Continue => true,
            HStmt::Expr(e) => e.ty == lm_types::NEVER,
            HStmt::While { cond, body } => while_diverges(cond, body),
            _ => false,
        }
    }
}

/// True when one loop never completes.
///
/// The condition must be the literal `true`, and no `break` of this
/// loop may appear in the body. `loop do ... end` parses to
/// `while true`, so this is the shape a driver writes.
///
/// The lowering pass reads the same answer. A loop that never
/// completes emits no exit edge, so the block after it keeps no
/// predecessor and the verifier never reconstructs a state for it.
pub fn while_diverges(cond: &HExpr, body: &[HStmt]) -> bool {
    matches!(cond.kind, HExprKind::Bool(true)) && !block_breaks(body)
}

/// True when one statement list holds a `break` of the loop that
/// encloses it.
///
/// The test asks about the exit, and never about the body. A body that
/// returns on one path and repeats on another still cannot reach the
/// statement after the loop. `break` is the only exit, so its absence
/// proves the loop never completes.
///
/// The walk stops at a nested `while`, because a `break` inside it
/// leaves that loop instead. A closure body is out of reach here:
/// `MakeClosure` names a separate function by index, and `break` is
/// invalid across that boundary.
fn block_breaks(body: &[HStmt]) -> bool {
    body.iter().any(stmt_breaks)
}

fn stmt_breaks(stmt: &HStmt) -> bool {
    match stmt {
        HStmt::Break => true,
        // A nested loop owns every `break` inside it.
        HStmt::While { .. } => false,
        HStmt::Expr(e) => expr_breaks(e),
        HStmt::Assign { value, .. } => expr_breaks(value),
        HStmt::AssignField { recv, value, .. } => expr_breaks(recv) || expr_breaks(value),
        HStmt::Return { value } => value.as_ref().is_some_and(expr_breaks),
        HStmt::Continue => false,
    }
}

/// True when one expression holds a `break` of the enclosing loop.
///
/// Only `if` and `case` carry statement lists. `select` becomes a
/// `case` before this point, so the two forms cover every block.
fn expr_breaks(expr: &HExpr) -> bool {
    match &expr.kind {
        HExprKind::If { arms, else_body } => {
            arms.iter()
                .any(|(cond, body)| expr_breaks(cond) || block_breaks(body))
                || else_body.as_deref().is_some_and(block_breaks)
        }
        HExprKind::Case { scrut, arms, .. } => {
            expr_breaks(scrut) || arms.iter().any(|arm| block_breaks(&arm.body))
        }
        _ => false,
    }
}

/// Render the class table in a stable readable form.
pub fn dump_classes(module: &HirModule) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (idx, class) in module.classes.iter().enumerate() {
        let final_mark = if class.is_final { " (final)" } else { "" };
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
                    "class {} {}{}{}{} < {}",
                    idx, class.name, final_mark, native_mark, kind, module.classes[p as usize].name
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "class {} {}{}{}{}",
                    idx, class.name, final_mark, native_mark, kind
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
