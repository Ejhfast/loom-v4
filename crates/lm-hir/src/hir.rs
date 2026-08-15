//! Typed HIR for the week-1 language slice.
//!
//! Every expression carries its checked type. Names are resolved to
//! dense local slots and function indices. Later phases never repeat
//! a textual lookup.

use lm_source::ast::BinOp;
use lm_types::{TypeId, TypeStore};

/// A checked module. The entry statements form the last function.
pub struct HirModule {
    pub store: TypeStore,
    pub funcs: Vec<HirFunc>,
    /// Index of the entry function inside `funcs`.
    pub entry: usize,
}

pub struct HirFunc {
    pub name: String,
    /// Parameter types. Parameters use the first local slots.
    pub params: Vec<TypeId>,
    pub ret: TypeId,
    /// All local slot types, parameters included.
    pub locals: Vec<TypeId>,
    pub body: Vec<HStmt>,
}

pub enum HStmt {
    Assign { slot: u32, value: HExpr },
    While { cond: HExpr, body: Vec<HStmt> },
    Return { value: Option<HExpr> },
    Break,
    Continue,
    Expr(HExpr),
}

pub struct HExpr {
    pub ty: TypeId,
    pub kind: HExprKind,
}

pub enum HExprKind {
    Int(i64),
    Str(String),
    Bool(bool),
    Local(u32),
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
    Call {
        func: u32,
        args: Vec<HExpr>,
    },
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
