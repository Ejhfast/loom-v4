//! Typed HIR for the week-2 language slice.
//!
//! Every expression carries its checked type and its reference
//! capability. Names are resolved to dense local slots, capture
//! indices, function indices, class indices, and field layout
//! indices. Later phases never repeat a textual lookup, except for
//! method selectors, which the lowering pass interns into dense
//! selector slots.

use lm_source::ast::BinOp;
use lm_types::{TypeId, TypeStore};

/// A checked module. The entry statements form one function.
pub struct HirModule {
    pub store: TypeStore,
    pub classes: Vec<HirClass>,
    pub funcs: Vec<HirFunc>,
    /// Index of the entry function inside `funcs`.
    pub entry: usize,
}

/// One checked class with its full field layout.
pub struct HirClass {
    pub name: String,
    /// Parent class index.
    pub parent: Option<u32>,
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
}

pub struct HirFunc {
    pub name: String,
    /// Parameter types. Parameters use the first local slots. A method
    /// receives `self` as parameter zero.
    pub params: Vec<TypeId>,
    pub ret: TypeId,
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
    MapLen,
    MapHas,
    MapAt,
    MapPut,
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

#[derive(Clone)]
pub enum HExprKind {
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
    /// superclass method.
    Call {
        func: u32,
        args: Vec<HExpr>,
    },
    /// Construction of a class instance.
    Construct {
        class: u32,
        args: Vec<HExpr>,
    },
    /// A virtual method call through the runtime class.
    MethodCall {
        recv: Box<HExpr>,
        selector: String,
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
        match class.parent {
            Some(p) => {
                let _ = writeln!(
                    out,
                    "class {} {} < {}",
                    idx, class.name, module.classes[p as usize].name
                );
            }
            None => {
                let _ = writeln!(out, "class {} {}", idx, class.name);
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
