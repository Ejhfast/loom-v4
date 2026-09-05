//! Abstract syntax tree for Loom source.

use crate::span::Span;
use std::fmt::Write as _;

/// A parsed module with one expression category for every body.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    /// The `use` lines. They come before every definition.
    pub uses: Vec<UseDecl>,
    pub interfaces: Vec<InterfaceDef>,
    pub classes: Vec<ClassDef>,
    pub enums: Vec<EnumDef>,
    pub constants: Vec<ConstDef>,
    pub funcs: Vec<FuncDef>,
    /// The last entry expression becomes the program result.
    pub entry: Vec<Expr>,
}

/// One module constant with an explicit type and a literal value.
#[derive(Debug, Clone, PartialEq)]
pub struct ConstDef {
    pub name: String,
    pub name_span: Span,
    pub ty: TypeExpr,
    pub value: Expr,
    pub span: Span,
}

/// One `use` line: a dotted path whose last segment becomes the bound
/// name, for example `use sys.io.write`.
#[derive(Debug, Clone, PartialEq)]
pub struct UseDecl {
    /// The path segments, in source order.
    pub path: Vec<String>,
    pub span: Span,
    /// The span of the last segment, which is the bound name.
    pub name_span: Span,
}

/// One generic parameter: a type parameter, or an effect parameter
/// declared with `effect`.
#[derive(Debug, Clone, PartialEq)]
pub struct GenericParam {
    pub name: String,
    pub is_effect: bool,
    pub bounds: Vec<InterfaceRef>,
    pub span: Span,
}

/// One type premise on an interface method.
#[derive(Debug, Clone, PartialEq)]
pub struct TypePremise {
    pub subject: TypeExpr,
    pub bounds: Vec<InterfaceRef>,
    pub span: Span,
}

/// One nominal interface application.
#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceRef {
    pub name: String,
    pub type_args: Vec<TypeExpr>,
    pub row_args: Vec<InterfaceRowArg>,
    pub span: Span,
}

/// One class or enum conformance with optional type premises.
#[derive(Debug, Clone, PartialEq)]
pub struct ConformanceRef {
    pub application: InterfaceRef,
    pub premises: Vec<GenericParam>,
    pub span: Span,
}

/// One effect-row argument of an interface application.
#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceRowArg {
    pub row: Vec<RowItem>,
    pub span: Span,
}

/// One associated type requirement or binding.
#[derive(Debug, Clone, PartialEq)]
pub struct AssociatedType {
    pub name: String,
    pub name_span: Span,
    pub bounds: Vec<InterfaceRef>,
    pub value: Option<TypeExpr>,
    pub span: Span,
}

/// One method requirement inside an interface.
#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceMethod {
    pub name: String,
    pub name_span: Span,
    pub generics: Vec<GenericParam>,
    pub mut_self: bool,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub row: Vec<RowItem>,
    pub premises: Vec<TypePremise>,
    /// `None` marks a required method. `Some` marks one default body.
    pub body: Option<Vec<Expr>>,
    pub span: Span,
}

/// One nominal interface declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceDef {
    pub name: String,
    pub name_span: Span,
    pub generics: Vec<GenericParam>,
    pub parents: Vec<InterfaceRef>,
    pub associated: Vec<AssociatedType>,
    pub methods: Vec<InterfaceMethod>,
    pub span: Span,
}

/// One element of a declared effect row: an operation or group name
/// such as `Io.Write` or `Io`, or an effect-parameter name.
#[derive(Debug, Clone, PartialEq)]
pub struct RowItem {
    pub name: String,
    pub span: Span,
}

/// A parent clause: `< Name` or `< Name[T, U]`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParentClause {
    pub name: String,
    pub span: Span,
    /// The type arguments of a generic parent. Empty for a plain name.
    pub args: Vec<TypeExpr>,
}

/// A `class` declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassDef {
    /// True when the declaration uses the `final` modifier.
    pub is_final: bool,
    /// True when every completed instance is deeply immutable.
    pub is_frozen: bool,
    pub name: String,
    pub name_span: Span,
    pub generics: Vec<GenericParam>,
    pub parent: Option<ParentClause>,
    pub interfaces: Vec<ConformanceRef>,
    pub associated: Vec<AssociatedType>,
    pub fields: Vec<FieldDef>,
    pub methods: Vec<MethodDef>,
    pub span: Span,
}

/// An `enum` declaration: arms first, then optional methods.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDef {
    pub name: String,
    pub name_span: Span,
    pub generics: Vec<GenericParam>,
    pub interfaces: Vec<ConformanceRef>,
    pub associated: Vec<AssociatedType>,
    pub arms: Vec<ArmDef>,
    pub methods: Vec<MethodDef>,
    pub span: Span,
}

/// One enum arm: a final case with zero or more typed fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ArmDef {
    pub name: String,
    pub name_span: Span,
    /// Field name, field type.
    pub fields: Vec<(String, TypeExpr)>,
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

/// One method declaration inside a class or an enum. `init` is a
/// method named `init` with `mut self`.
#[derive(Debug, Clone, PartialEq)]
pub struct MethodDef {
    pub name: String,
    pub name_span: Span,
    pub generics: Vec<GenericParam>,
    /// True when the receiver is `mut self`.
    pub mut_self: bool,
    pub params: Vec<Param>,
    /// `None` means the unit result type `()`.
    pub ret: Option<TypeExpr>,
    pub row: Vec<RowItem>,
    pub premises: Vec<GenericParam>,
    pub body: Vec<Expr>,
    pub span: Span,
}

/// A top-level `def` function.
#[derive(Debug, Clone, PartialEq)]
pub struct FuncDef {
    pub name: String,
    pub name_span: Span,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    /// `None` means the unit result type `()`.
    pub ret: Option<TypeExpr>,
    pub row: Vec<RowItem>,
    pub body: Vec<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    /// True for a `mut` parameter with mutable capability.
    pub mutable: bool,
    /// True when a function parameter can escape its call.
    pub escaping: bool,
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
    /// A named type such as `Int`, a class name, or a type parameter.
    Name(String),
    /// The unit type `()`.
    Unit,
    /// A generic application such as `List[Int]` or `Box[T]`.
    Apply(String, Vec<TypeExpr>),
    /// List shorthand `[T]`.
    ListShort(Box<TypeExpr>),
    /// Map shorthand `{K: V}`.
    MapShort(Box<TypeExpr>, Box<TypeExpr>),
    /// A tuple type `(T, U)` or `(T,)`.
    Tuple(Vec<TypeExpr>),
    /// A function type `(A, mut B) -> R with row`. The `bool` list
    /// marks the `mut` parameters and aligns with the parameter list.
    Fn(Vec<TypeExpr>, Vec<bool>, Box<TypeExpr>, Vec<RowItem>),
    /// A nonescaping function type.
    Nonescaping(Box<TypeExpr>),
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
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Ushr,
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
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Ushr => ">>>",
        }
    }
}

/// One piece of an interpolated string expression.
#[derive(Debug, Clone, PartialEq)]
pub enum InterpPart {
    Lit(String),
    Expr(Expr),
}

/// One `case` arm: a pattern and a body.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseArm {
    pub pattern: Pattern,
    pub body: Vec<Expr>,
    pub span: Span,
}

/// One `select` arm.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectArm {
    pub wait: Expr,
    pub binding: String,
    pub binding_span: Span,
    pub body: Vec<Expr>,
    pub span: Span,
}

/// One pattern inside a `case` arm.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatternKind {
    /// `_` matches any value and binds nothing.
    Wildcard,
    /// A bare name: a binding, or a zero-field constructor when the
    /// name resolves to an arm of the scrutinee enum.
    Name(String),
    /// A constructor pattern. `qualifier` holds the enum name for a
    /// canonical qualified form such as `Option.Some`. `has_parens`
    /// records whether an argument list appeared.
    Ctor {
        qualifier: Option<String>,
        name: String,
        args: Vec<Pattern>,
        has_parens: bool,
    },
    /// Refine one reflection descriptor and bind its value.
    /// The binders exist only inside this case arm.
    Reflect {
        kind: String,
        generics: Vec<GenericParam>,
        /// A callable signature, or none for a class descriptor refinement.
        signature: Option<TypeExpr>,
        binding: Box<Pattern>,
    },
    /// A tuple pattern: `(a, b)`. One element needs a trailing
    /// comma, as a one-tuple expression does.
    Tuple(Vec<Pattern>),
    /// Supported literal patterns.
    Int(i64),
    Bool(bool),
    Char(char),
    Str(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// `name = value` or `name: Type = value`.
    Assign {
        name: String,
        name_span: Span,
        ty: Option<TypeExpr>,
        value: Box<Expr>,
    },
    /// `(left, right) = value` binds one irrefutable tuple pattern.
    Destructure {
        pattern: Box<Pattern>,
        value: Box<Expr>,
    },
    /// `receiver.field = value`.
    AssignField {
        recv: Box<Expr>,
        field: String,
        field_span: Span,
        value: Box<Expr>,
    },
    /// `while condition` followed by its body.
    While {
        cond: Box<Expr>,
        body: Vec<Expr>,
    },
    /// `for name in value` followed by its body.
    For {
        bindings: Vec<(String, Span)>,
        value: Box<Expr>,
        body: Vec<Expr>,
    },
    /// `loop do body end`.
    Loop {
        body: Vec<Expr>,
    },
    /// `return` with an optional value.
    Return {
        value: Option<Box<Expr>>,
    },
    /// `break` with an optional loop result.
    Break {
        value: Option<Box<Expr>>,
    },
    Continue,
    Int(i64),
    /// IEEE 754 binary64 value, stored as raw bits.
    Float(u64),
    /// One Unicode scalar value.
    Char(char),
    Str(String),
    Bytes(Vec<u8>),
    Regex(String),
    /// An interpolated string literal.
    Interp(Vec<InterpPart>),
    Bool(bool),
    /// The unit literal `()`.
    Unit,
    Name(String),
    /// The method receiver `self`.
    SelfRef,
    /// `not value` or `- value`.
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Invert(Box<Expr>),
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// Short-circuit `and`.
    And(Box<Expr>, Box<Expr>),
    /// Short-circuit `or`.
    Or(Box<Expr>, Box<Expr>),
    /// `value is Type`: a pure nominal type test.
    Is {
        value: Box<Expr>,
        ty: TypeExpr,
    },
    /// `value as Type`: a cast that faults `BadCast` on failure.
    Cast {
        value: Box<Expr>,
        ty: TypeExpr,
    },
    /// A call of a name: a function, a class constructor, an enum
    /// constructor, or a closure-typed local. The checker selects the
    /// meaning. `type_args` holds explicit generic arguments.
    Call {
        name: String,
        name_span: Span,
        type_args: Vec<TypeExpr>,
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
        type_args: Vec<TypeExpr>,
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
    /// `tuple.0` reads one tuple element by static position.
    TupleGet {
        tuple: Box<Expr>,
        index: i64,
        index_span: Span,
    },
    /// `value?` returns an error from the enclosing callable.
    Propagate(Box<Expr>),
    /// A tuple literal `(a, b)` or `(a,)`.
    TupleLit(Vec<Expr>),
    /// A list literal `[a, b]`.
    ListLit(Vec<Expr>),
    /// A map literal `{k: v}`.
    MapLit(Vec<(Expr, Expr)>),
    /// A closure literal `do |x: Int|: Int with Row ... end`.
    Closure {
        params: Vec<Param>,
        /// `None` requests result inference from the body.
        ret: Option<TypeExpr>,
        row: Vec<RowItem>,
        /// True when the source contains a `with` clause.
        row_explicit: bool,
        body: Vec<Expr>,
    },
    /// `if ... elsif ... else ... end` as an expression.
    If {
        /// Condition and body for `if` and each `elsif`.
        arms: Vec<(Expr, Vec<Expr>)>,
        else_body: Option<Vec<Expr>>,
    },
    /// `case scrutinee in pattern then|body ... end` as an expression.
    Case {
        scrut: Box<Expr>,
        arms: Vec<CaseArm>,
    },
    /// `select in wait -> value body ... end` as an expression.
    Select {
        arms: Vec<SelectArm>,
    },
    /// A labeled call argument, for example `args: ()`. Valid only in
    /// the argument positions the checker accepts.
    Labeled {
        label: String,
        value: Box<Expr>,
    },
}

/// Render a module as an indented, stable, human-readable tree.
pub fn dump_module(module: &Module) -> String {
    let mut out = String::new();
    out.push_str("module\n");
    for use_decl in &module.uses {
        let _ = writeln!(out, "  use {}", use_decl.path.join("."));
    }
    for class in &module.classes {
        let generics = dump_generics(&class.generics);
        match &class.parent {
            Some(parent) => {
                let args = if parent.args.is_empty() {
                    String::new()
                } else {
                    let parts: Vec<String> = parent.args.iter().map(dump_type).collect();
                    format!("[{}]", parts.join(", "))
                };
                let _ = writeln!(
                    out,
                    "  class {}{generics} < {}{args}",
                    class.name, parent.name
                );
            }
            None => {
                let _ = writeln!(out, "  class {}{generics}", class.name);
            }
        }
        for field in &class.fields {
            let _ = writeln!(out, "    field {}: {}", field.name, dump_type(&field.ty));
            if let Some(default) = &field.default {
                dump_expr(&mut out, default, 3);
            }
        }
        for method in &class.methods {
            dump_method(&mut out, method);
        }
    }
    for enum_def in &module.enums {
        let generics = dump_generics(&enum_def.generics);
        let _ = writeln!(out, "  enum {}{generics}", enum_def.name);
        for arm in &enum_def.arms {
            let fields: Vec<String> = arm
                .fields
                .iter()
                .map(|(name, ty)| format!("{name}: {}", dump_type(ty)))
                .collect();
            let _ = writeln!(out, "    arm {}({})", arm.name, fields.join(", "));
        }
        for method in &enum_def.methods {
            dump_method(&mut out, method);
        }
    }
    for constant in &module.constants {
        let _ = writeln!(
            out,
            "  const {}: {}",
            constant.name,
            dump_type(&constant.ty)
        );
        dump_expr(&mut out, &constant.value, 2);
    }
    for func in &module.funcs {
        let _ = writeln!(
            out,
            "  def {}{}({}): {}{}",
            func.name,
            dump_generics(&func.generics),
            dump_params(&func.params, false),
            dump_ret(&func.ret),
            dump_row(&func.row)
        );
        for expr in &func.body {
            dump_expr(&mut out, expr, 2);
        }
    }
    out.push_str("  entry\n");
    for expr in &module.entry {
        dump_expr(&mut out, expr, 2);
    }
    out
}

fn dump_method(out: &mut String, method: &MethodDef) {
    let recv = if method.mut_self { "mut self" } else { "self" };
    let _ = writeln!(
        out,
        "    def {}{}({recv}{}): {}{}",
        method.name,
        dump_generics(&method.generics),
        dump_params(&method.params, true),
        dump_ret(&method.ret),
        dump_row(&method.row)
    );
    for expr in &method.body {
        dump_expr(out, expr, 3);
    }
}

fn dump_generics(generics: &[GenericParam]) -> String {
    if generics.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = generics
        .iter()
        .map(|g| {
            if g.is_effect {
                format!("effect {}", g.name)
            } else {
                g.name.clone()
            }
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

fn dump_row(row: &[RowItem]) -> String {
    if row.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = row.iter().map(|r| r.name.clone()).collect();
    format!(" with {}", parts.join(", "))
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
        if p.escaping {
            out.push_str("escaping ");
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
        TypeExprKind::Tuple(elems) => {
            let parts: Vec<String> = elems.iter().map(dump_type).collect();
            if parts.len() == 1 {
                format!("({},)", parts[0])
            } else {
                format!("({})", parts.join(", "))
            }
        }
        TypeExprKind::Fn(params, muts, ret, row) => {
            let parts: Vec<String> = params
                .iter()
                .zip(muts.iter())
                .map(|(p, m)| {
                    if *m {
                        format!("mut {}", dump_type(p))
                    } else {
                        dump_type(p)
                    }
                })
                .collect();
            format!(
                "({}) -> {}{}",
                parts.join(", "),
                dump_type(ret),
                dump_row(row)
            )
        }
        TypeExprKind::Nonescaping(inner) => format!("nonescaping {}", dump_type(inner)),
    }
}

fn dump_pattern(pattern: &Pattern) -> String {
    match &pattern.kind {
        PatternKind::Wildcard => "_".to_string(),
        PatternKind::Tuple(elems) => {
            let parts: Vec<String> = elems.iter().map(dump_pattern).collect();
            if parts.len() == 1 {
                format!("({},)", parts[0])
            } else {
                format!("({})", parts.join(", "))
            }
        }
        PatternKind::Name(name) => name.clone(),
        PatternKind::Ctor {
            qualifier,
            name,
            args,
            has_parens,
        } => {
            let mut out = String::new();
            if let Some(q) = qualifier {
                out.push_str(q);
                out.push('.');
            }
            out.push_str(name);
            if *has_parens {
                let parts: Vec<String> = args.iter().map(dump_pattern).collect();
                let _ = write!(out, "({})", parts.join(", "));
            }
            out
        }
        PatternKind::Reflect {
            kind,
            generics,
            signature,
            binding,
        } => {
            let mut items: Vec<String> = generics
                .iter()
                .map(|generic| {
                    let prefix = if generic.is_effect { "effect" } else { "type" };
                    format!("{prefix} {}", generic.name)
                })
                .collect();
            if let Some(signature) = signature {
                items.push(dump_type(signature));
            }
            format!("{kind}[{}]({})", items.join(", "), dump_pattern(binding))
        }
        PatternKind::Int(v) => v.to_string(),
        PatternKind::Bool(v) => v.to_string(),
        PatternKind::Char(v) => format!("{v:?}"),
        PatternKind::Str(v) => format!("{v:?}"),
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn dump_expr(out: &mut String, expr: &Expr, depth: usize) {
    indent(out, depth);
    match &expr.kind {
        ExprKind::Assign {
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
        ExprKind::Destructure { pattern, value } => {
            let _ = writeln!(out, "destructure {}", dump_pattern(pattern));
            dump_expr(out, value, depth + 1);
        }
        ExprKind::AssignField {
            recv, field, value, ..
        } => {
            let _ = writeln!(out, "assign-field {field}");
            dump_expr(out, recv, depth + 1);
            dump_expr(out, value, depth + 1);
        }
        ExprKind::While { cond, body } => {
            out.push_str("while\n");
            dump_expr(out, cond, depth + 1);
            indent(out, depth);
            out.push_str("do\n");
            for item in body {
                dump_expr(out, item, depth + 1);
            }
        }
        ExprKind::For {
            bindings,
            value,
            body,
        } => {
            let names: Vec<&str> = bindings.iter().map(|item| item.0.as_str()).collect();
            let _ = writeln!(out, "for {}", names.join(", "));
            dump_expr(out, value, depth + 1);
            for item in body {
                dump_expr(out, item, depth + 1);
            }
        }
        ExprKind::Loop { body } => {
            out.push_str("loop\n");
            for item in body {
                dump_expr(out, item, depth + 1);
            }
        }
        ExprKind::Return { value } => {
            out.push_str("return\n");
            if let Some(value) = value {
                dump_expr(out, value, depth + 1);
            }
        }
        ExprKind::Break { value } => {
            out.push_str("break\n");
            if let Some(value) = value {
                dump_expr(out, value, depth + 1);
            }
        }
        ExprKind::Continue => out.push_str("continue\n"),
        ExprKind::Int(v) => {
            let _ = writeln!(out, "int {v}");
        }
        ExprKind::Float(bits) => {
            let _ = writeln!(out, "float {:?}", f64::from_bits(*bits));
        }
        ExprKind::Char(value) => {
            let _ = writeln!(out, "char {value:?}");
        }
        ExprKind::Str(v) => {
            let _ = writeln!(out, "str {v:?}");
        }
        ExprKind::Bytes(v) => {
            let _ = writeln!(out, "bytes {v:?}");
        }
        ExprKind::Regex(v) => {
            let _ = writeln!(out, "regex {v:?}");
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
        ExprKind::Unit => out.push_str("unit\n"),
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
        ExprKind::Invert(inner) => {
            out.push_str("invert\n");
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
        ExprKind::Is { value, ty } => {
            let _ = writeln!(out, "is {}", dump_type(ty));
            dump_expr(out, value, depth + 1);
        }
        ExprKind::Cast { value, ty } => {
            let _ = writeln!(out, "as {}", dump_type(ty));
            dump_expr(out, value, depth + 1);
        }
        ExprKind::Call {
            name,
            type_args,
            args,
            ..
        } => {
            if type_args.is_empty() {
                let _ = writeln!(out, "call {name}");
            } else {
                let parts: Vec<String> = type_args.iter().map(dump_type).collect();
                let _ = writeln!(out, "call {name}[{}]", parts.join(", "));
            }
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
            recv,
            name,
            type_args,
            args,
            ..
        } => {
            if type_args.is_empty() {
                let _ = writeln!(out, "method-call {name}");
            } else {
                let parts: Vec<String> = type_args.iter().map(dump_type).collect();
                let _ = writeln!(out, "method-call {name}[{}]", parts.join(", "));
            }
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
        ExprKind::TupleGet { tuple, index, .. } => {
            let _ = writeln!(out, "tuple-get {index}");
            dump_expr(out, tuple, depth + 1);
        }
        ExprKind::Propagate(value) => {
            out.push_str("propagate\n");
            dump_expr(out, value, depth + 1);
        }
        ExprKind::TupleLit(items) => {
            out.push_str("tuple\n");
            for item in items {
                dump_expr(out, item, depth + 1);
            }
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
        ExprKind::Closure {
            params,
            ret,
            row,
            row_explicit: _,
            body,
        } => {
            let _ = writeln!(
                out,
                "closure |{}|: {}{}",
                dump_params(params, false),
                dump_ret(ret),
                dump_row(row)
            );
            for expr in body {
                dump_expr(out, expr, depth + 1);
            }
        }
        ExprKind::If { arms, else_body } => {
            out.push_str("if\n");
            for (cond, body) in arms {
                indent(out, depth + 1);
                out.push_str("arm\n");
                dump_expr(out, cond, depth + 2);
                for expr in body {
                    dump_expr(out, expr, depth + 2);
                }
            }
            if let Some(body) = else_body {
                indent(out, depth + 1);
                out.push_str("else\n");
                for expr in body {
                    dump_expr(out, expr, depth + 2);
                }
            }
        }
        ExprKind::Case { scrut, arms } => {
            out.push_str("case\n");
            dump_expr(out, scrut, depth + 1);
            for arm in arms {
                indent(out, depth + 1);
                let _ = writeln!(out, "in {}", dump_pattern(&arm.pattern));
                for expr in &arm.body {
                    dump_expr(out, expr, depth + 2);
                }
            }
        }
        ExprKind::Select { arms } => {
            out.push_str("select\n");
            for arm in arms {
                indent(out, depth + 1);
                let _ = writeln!(out, "in -> {}", arm.binding);
                dump_expr(out, &arm.wait, depth + 2);
                for expr in &arm.body {
                    dump_expr(out, expr, depth + 2);
                }
            }
        }
        ExprKind::Labeled { label, value } => {
            let _ = writeln!(out, "labeled {label}");
            dump_expr(out, value, depth + 1);
        }
    }
}
