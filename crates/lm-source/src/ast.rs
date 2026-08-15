//! Abstract syntax tree for the week-2 language slice.

use crate::span::Span;
use std::fmt::Write as _;

/// A parsed module: classes, top-level functions, and entry statements.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub classes: Vec<ClassDef>,
    pub funcs: Vec<FuncDef>,
    /// Top-level statements. The value of the last expression statement
    /// becomes the program result.
    pub entry: Vec<Stmt>,
}

/// A `class` declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassDef {
    pub name: String,
    pub name_span: Span,
    pub parent: Option<(String, Span)>,
    pub fields: Vec<FieldDef>,
    pub methods: Vec<MethodDef>,
    pub span: Span,
}

/// One field declaration inside a class.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    pub name: String,
    pub ty: TypeExpr,
    /// The optional pure default expression.
    pub default: Option<Expr>,
    pub span: Span,
}

/// One method declaration inside a class. `init` is a method named
/// `init` with `mut self`.
#[derive(Debug, Clone, PartialEq)]
pub struct MethodDef {
    pub name: String,
    pub name_span: Span,
    /// True when the receiver is `mut self`.
    pub mut_self: bool,
    pub params: Vec<Param>,
    /// `None` means the unit result type `()`.
    pub ret: Option<TypeExpr>,
    pub body: Vec<Stmt>,
    pub span: Span,
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
    /// True for a `mut` parameter with mutable capability.
    pub mutable: bool,
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
    /// A named type such as `Int` or a class name.
    Name(String),
    /// The unit type `()`.
    Unit,
    /// A generic application such as `List[Int]` or `Map[String, Int]`.
    Apply(String, Vec<TypeExpr>),
    /// List shorthand `[T]`.
    ListShort(Box<TypeExpr>),
    /// Map shorthand `{K: V}`.
    MapShort(Box<TypeExpr>, Box<TypeExpr>),
    /// A function type `(A, B) -> R`.
    Fn(Vec<TypeExpr>, Box<TypeExpr>),
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
    /// `receiver.field = value`.
    AssignField {
        recv: Expr,
        field: String,
        field_span: Span,
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

/// One piece of an interpolated string expression.
#[derive(Debug, Clone, PartialEq)]
pub enum InterpPart {
    Lit(String),
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Str(String),
    /// An interpolated string literal.
    Interp(Vec<InterpPart>),
    Bool(bool),
    Name(String),
    /// The method receiver `self`.
    SelfRef,
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
    /// A call of a name: a function, a class constructor, or a
    /// closure-typed local. The checker selects the meaning.
    Call {
        name: String,
        name_span: Span,
        args: Vec<Expr>,
    },
    /// A call of a non-name callee expression: a closure value.
    CallExpr {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    /// `receiver.field` without a call.
    Field {
        recv: Box<Expr>,
        name: String,
        name_span: Span,
    },
    /// `receiver.method(args)`.
    MethodCall {
        recv: Box<Expr>,
        name: String,
        name_span: Span,
        args: Vec<Expr>,
    },
    /// `super.method(args)` or `super.init(args)`.
    SuperCall {
        name: String,
        name_span: Span,
        args: Vec<Expr>,
    },
    /// `receiver[index]`.
    Index {
        recv: Box<Expr>,
        index: Box<Expr>,
    },
    /// A list literal `[a, b]`.
    ListLit(Vec<Expr>),
    /// A map literal `{k: v}`.
    MapLit(Vec<(Expr, Expr)>),
    /// A closure literal `do |x: Int|: Int ... end`.
    Closure {
        params: Vec<Param>,
        /// `None` requests result inference from the body.
        ret: Option<TypeExpr>,
        body: Vec<Stmt>,
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
    for class in &module.classes {
        match &class.parent {
            Some((parent, _)) => {
                let _ = writeln!(out, "  class {} < {}", class.name, parent);
            }
            None => {
                let _ = writeln!(out, "  class {}", class.name);
            }
        }
        for field in &class.fields {
            let _ = writeln!(out, "    field {}: {}", field.name, dump_type(&field.ty));
            if let Some(default) = &field.default {
                dump_expr(&mut out, default, 3);
            }
        }
        for method in &class.methods {
            let recv = if method.mut_self { "mut self" } else { "self" };
            let _ = writeln!(
                out,
                "    def {}({recv}{}): {}",
                method.name,
                dump_params(&method.params, true),
                dump_ret(&method.ret)
            );
            for stmt in &method.body {
                dump_stmt(&mut out, stmt, 3);
            }
        }
    }
    for func in &module.funcs {
        let _ = writeln!(
            out,
            "  def {}({}): {}",
            func.name,
            dump_params(&func.params, false),
            dump_ret(&func.ret)
        );
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

fn dump_params(params: &[Param], leading_comma: bool) -> String {
    let mut out = String::new();
    for (i, p) in params.iter().enumerate() {
        if i > 0 || leading_comma {
            out.push_str(", ");
        }
        if p.mutable {
            out.push_str("mut ");
        }
        let _ = write!(out, "{}: {}", p.name, dump_type(&p.ty));
    }
    out
}

fn dump_ret(ret: &Option<TypeExpr>) -> String {
    ret.as_ref()
        .map(dump_type)
        .unwrap_or_else(|| "()".to_string())
}

fn dump_type(ty: &TypeExpr) -> String {
    match &ty.kind {
        TypeExprKind::Name(name) => name.clone(),
        TypeExprKind::Unit => "()".to_string(),
        TypeExprKind::Apply(name, args) => {
            let parts: Vec<String> = args.iter().map(dump_type).collect();
            format!("{}[{}]", name, parts.join(", "))
        }
        TypeExprKind::ListShort(elem) => format!("[{}]", dump_type(elem)),
        TypeExprKind::MapShort(k, v) => format!("{{{}: {}}}", dump_type(k), dump_type(v)),
        TypeExprKind::Fn(params, ret) => {
            let parts: Vec<String> = params.iter().map(dump_type).collect();
            format!("({}) -> {}", parts.join(", "), dump_type(ret))
        }
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
        StmtKind::AssignField {
            recv, field, value, ..
        } => {
            let _ = writeln!(out, "assign-field {field}");
            dump_expr(out, recv, depth + 1);
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
        ExprKind::Interp(parts) => {
            out.push_str("interp\n");
            for part in parts {
                match part {
                    InterpPart::Lit(text) => {
                        indent(out, depth + 1);
                        let _ = writeln!(out, "lit {text:?}");
                    }
                    InterpPart::Expr(e) => dump_expr(out, e, depth + 1),
                }
            }
        }
        ExprKind::Bool(v) => {
            let _ = writeln!(out, "bool {v}");
        }
        ExprKind::Name(name) => {
            let _ = writeln!(out, "name {name}");
        }
        ExprKind::SelfRef => out.push_str("self\n"),
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
        ExprKind::CallExpr { callee, args } => {
            out.push_str("call-value\n");
            dump_expr(out, callee, depth + 1);
            for arg in args {
                dump_expr(out, arg, depth + 1);
            }
        }
        ExprKind::Field { recv, name, .. } => {
            let _ = writeln!(out, "field {name}");
            dump_expr(out, recv, depth + 1);
        }
        ExprKind::MethodCall {
            recv, name, args, ..
        } => {
            let _ = writeln!(out, "method-call {name}");
            dump_expr(out, recv, depth + 1);
            for arg in args {
                dump_expr(out, arg, depth + 1);
            }
        }
        ExprKind::SuperCall { name, args, .. } => {
            let _ = writeln!(out, "super-call {name}");
            for arg in args {
                dump_expr(out, arg, depth + 1);
            }
        }
        ExprKind::Index { recv, index } => {
            out.push_str("index\n");
            dump_expr(out, recv, depth + 1);
            dump_expr(out, index, depth + 1);
        }
        ExprKind::ListLit(items) => {
            out.push_str("list\n");
            for item in items {
                dump_expr(out, item, depth + 1);
            }
        }
        ExprKind::MapLit(entries) => {
            out.push_str("map\n");
            for (k, v) in entries {
                dump_expr(out, k, depth + 1);
                dump_expr(out, v, depth + 1);
            }
        }
        ExprKind::Closure { params, ret, body } => {
            let _ = writeln!(
                out,
                "closure |{}|: {}",
                dump_params(params, false),
                dump_ret(ret)
            );
            for stmt in body {
                dump_stmt(out, stmt, depth + 1);
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
