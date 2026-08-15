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

/// A checked module. The entry statements form one function.
pub struct HirModule {
    pub store: TypeStore,
    pub classes: Vec<HirClass>,
    pub funcs: Vec<HirFunc>,
    /// Index of the entry function inside `funcs`.
    pub entry: usize,
    /// Pinned core definition indices.
    pub core: CoreIds,
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

/// One checked class with its full field layout.
pub struct HirClass {
    pub name: String,
    /// Parent class index.
    pub parent: Option<u32>,
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
    /// Own method table: `(selector name, function index)`.
    pub methods: Vec<(String, u32)>,
    /// The `init` function index, when declared.
    pub init: Option<u32>,
    /// Constructor parameter types, without `self`.
    pub ctor_params: Vec<TypeId>,
    /// The row charged by construction (the `init` row).
    pub ctor_row: Row,
}

pub struct HirFunc {
    pub name: String,
    /// The number of generic type parameters in scope for the body.
    pub type_params: u32,
    /// The number of effect parameters in scope for the body.
    pub effect_params: u32,
    /// Parameter types. Parameters use the first local slots. A method
    /// receives `self` as parameter zero.
    pub params: Vec<TypeId>,
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
    SbNew,
    SbAppend,
    SbBuild,
    BbNew,
    BbAppend,
    BbLen,
    BbBuild,
    Freeze,
}

/// One piece of an interpolated string.
#[derive(Clone)]
pub enum HInterpPart {
    Lit(String),
    Expr(HExpr),
}

/// One resolved pattern.
#[derive(Clone)]
pub enum HPattern {
    /// Matches anything, binds nothing.
    Wildcard,
    /// Matches anything and stores the value in a local slot.
    Bind(u32),
    Int(i64),
    Bool(bool),
    Str(String),
    /// Tests the final case class and destructures its fields. `ty`
    /// is the instantiated case type used by the test and the cast.
    Ctor {
        class: u32,
        ty: TypeId,
        args: Vec<HPattern>,
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
}

impl HStmt {
    /// Return true when control cannot continue after this statement.
    pub fn diverges(&self) -> bool {
        match self {
            HStmt::Return { .. } | HStmt::Break | HStmt::Continue => true,
            HStmt::Expr(e) => e.ty == lm_types::NEVER,
            _ => false,
        }
    }
}

/// Render the class table in a stable readable form.
pub fn dump_classes(module: &HirModule) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (idx, class) in module.classes.iter().enumerate() {
        let kind = match class.kind {
            ClassKind::Normal => "",
            ClassKind::EnumParent => " (enum)",
            ClassKind::EnumCase => " (case)",
        };
        match class.parent {
            Some(p) => {
                let _ = writeln!(
                    out,
                    "class {} {}{} < {}",
                    idx, class.name, kind, module.classes[p as usize].name
                );
            }
            None => {
                let _ = writeln!(out, "class {} {}{}", idx, class.name, kind);
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
