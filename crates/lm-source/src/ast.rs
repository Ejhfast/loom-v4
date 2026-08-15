//! Abstract syntax tree for the week-1 language slice.

use crate::span::Span;
use std::fmt::Write as _;

/// A parsed module: top-level functions plus the entry statements.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub funcs: Vec<FuncDef>,
    /// Top-level statements. The value of the last expression statement
    /// becomes the program result.
    pub entry: Vec<Stmt>,
}

/// A top-level `def` function.
#[derive(Debug, Clone, PartialEq)]
pub struct FuncDef {
    pub name: String,
    pub name_span: Span,
    pub params: Vec<Param>,
    /// `None` means the unit result type `()`.
    pub ret: Option<TypeExpr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: TypeExpr,
    pub span: Span,
}

/// A source type annotation.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeExprKind {
    /// A named type such as `Int`.
    Name(String),
    /// The unit type `()`.
    Unit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// `name = value` or `name: Type = value`.
    Assign {
        name: String,
        name_span: Span,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    /// `while cond ... end`.
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    /// `return` with an optional value.
    Return {
        value: Option<Expr>,
    },
    Break,
    Continue,
    /// An expression in statement position.
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub fn text(&self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Str(String),
    Bool(bool),
    Name(String),
    /// `not value` or `- value`.
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// Short-circuit `and`.
    And(Box<Expr>, Box<Expr>),
    /// Short-circuit `or`.
    Or(Box<Expr>, Box<Expr>),
    /// A direct call of a named function.
    Call {
        name: String,
        name_span: Span,
        args: Vec<Expr>,
    },
    /// `if ... elsif ... else ... end` as an expression.
    If {
        /// Condition and body for `if` and each `elsif`.
        arms: Vec<(Expr, Vec<Stmt>)>,
        else_body: Option<Vec<Stmt>>,
    },
}

/// Render a module as an indented, stable, human-readable tree.
pub fn dump_module(module: &Module) -> String {
    let mut out = String::new();
    out.push_str("module\n");
    for func in &module.funcs {
        let _ = write!(out, "  def {}(", func.name);
        for (i, p) in func.params.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "{}: {}", p.name, dump_type(&p.ty));
        }
        let ret = func
            .ret
            .as_ref()
            .map(dump_type)
            .unwrap_or_else(|| "()".to_string());
        let _ = writeln!(out, "): {ret}");
        for stmt in &func.body {
            dump_stmt(&mut out, stmt, 2);
        }
    }
    out.push_str("  entry\n");
    for stmt in &module.entry {
        dump_stmt(&mut out, stmt, 2);
    }
    out
}

fn dump_type(ty: &TypeExpr) -> String {
    match &ty.kind {
        TypeExprKind::Name(name) => name.clone(),
        TypeExprKind::Unit => "()".to_string(),
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn dump_stmt(out: &mut String, stmt: &Stmt, depth: usize) {
    indent(out, depth);
    match &stmt.kind {
        StmtKind::Assign {
            name, ty, value, ..
        } => {
            match ty {
                Some(ty) => {
                    let _ = writeln!(out, "assign {}: {}", name, dump_type(ty));
                }
                None => {
                    let _ = writeln!(out, "assign {name}");
                }
            }
            dump_expr(out, value, depth + 1);
        }
        StmtKind::While { cond, body } => {
            out.push_str("while\n");
            dump_expr(out, cond, depth + 1);
            indent(out, depth);
            out.push_str("do\n");
            for s in body {
                dump_stmt(out, s, depth + 1);
            }
        }
        StmtKind::Return { value } => {
            out.push_str("return\n");
            if let Some(value) = value {
                dump_expr(out, value, depth + 1);
            }
        }
        StmtKind::Break => out.push_str("break\n"),
        StmtKind::Continue => out.push_str("continue\n"),
        StmtKind::Expr(expr) => {
            out.push_str("expr\n");
            dump_expr(out, expr, depth + 1);
        }
    }
}

fn dump_expr(out: &mut String, expr: &Expr, depth: usize) {
    indent(out, depth);
    match &expr.kind {
        ExprKind::Int(v) => {
            let _ = writeln!(out, "int {v}");
        }
        ExprKind::Str(v) => {
            let _ = writeln!(out, "str {v:?}");
        }
        ExprKind::Bool(v) => {
            let _ = writeln!(out, "bool {v}");
        }
        ExprKind::Name(name) => {
            let _ = writeln!(out, "name {name}");
        }
        ExprKind::Not(inner) => {
            out.push_str("not\n");
            dump_expr(out, inner, depth + 1);
        }
        ExprKind::Neg(inner) => {
            out.push_str("neg\n");
            dump_expr(out, inner, depth + 1);
        }
        ExprKind::Binary { op, left, right } => {
            let _ = writeln!(out, "binary {}", op.text());
            dump_expr(out, left, depth + 1);
            dump_expr(out, right, depth + 1);
        }
        ExprKind::And(left, right) => {
            out.push_str("and\n");
            dump_expr(out, left, depth + 1);
            dump_expr(out, right, depth + 1);
        }
        ExprKind::Or(left, right) => {
            out.push_str("or\n");
            dump_expr(out, left, depth + 1);
            dump_expr(out, right, depth + 1);
        }
        ExprKind::Call { name, args, .. } => {
            let _ = writeln!(out, "call {name}");
            for arg in args {
                dump_expr(out, arg, depth + 1);
            }
        }
        ExprKind::If { arms, else_body } => {
            out.push_str("if\n");
            for (cond, body) in arms {
                indent(out, depth + 1);
                out.push_str("arm\n");
                dump_expr(out, cond, depth + 2);
                for s in body {
                    dump_stmt(out, s, depth + 2);
                }
            }
            if let Some(body) = else_body {
                indent(out, depth + 1);
                out.push_str("else\n");
                for s in body {
                    dump_stmt(out, s, depth + 2);
                }
            }
        }
    }
}
