//! Recursive-descent parser for the week-3 language slice.
//!
//! The parser rejects all constructs outside the slice with a
//! precise diagnostic. It never accepts a silent fallback form.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::scan::scan;
use crate::span::Span;
use crate::token::{StrPiece, Tok, Token};

/// The maximum nesting depth for expressions, statements, types, and
/// patterns.
///
/// The parser and the checker recurse on the Rust stack. This limit
/// keeps deep input inside the available stack and rejects deeper
/// input with a diagnostic.
pub const MAX_NEST_DEPTH: usize = 300;

/// The maximum portable tuple arity.
pub const MAX_TUPLE_ARITY: usize = 16;

/// Scan and parse one module.
pub fn parse(text: &str) -> Result<Module, Diagnostic> {
    let tokens = scan(text)?;
    let mut parser = Parser {
        tokens,
        pos: 0,
        depth: 0,
    };
    parser.module()
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    fn peek_at(&self, ahead: usize) -> &Tok {
        let idx = (self.pos + ahead).min(self.tokens.len() - 1);
        &self.tokens[idx].tok
    }

    fn peek_span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn next(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    fn skip_newlines(&mut self) {
        while matches!(self.peek(), Tok::Newline) {
            self.pos += 1;
        }
    }

    fn error(&self, code: &'static str, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(code, message, self.peek_span())
    }

    fn expect(&mut self, want: Tok, what: &str) -> Result<Token, Diagnostic> {
        if *self.peek() == want {
            Ok(self.next())
        } else {
            Err(self.error("E1003", format!("expected {what}, found {}", self.peek())))
        }
    }

    /// Reject a reserved keyword with a precise diagnostic.
    fn reject_reserved(&self) -> Result<(), Diagnostic> {
        if let Tok::KwReserved(word) = self.peek() {
            return Err(self.error(
                "E1002",
                format!("`{word}` is not supported in this language slice"),
            ));
        }
        Ok(())
    }

    fn module(&mut self) -> Result<Module, Diagnostic> {
        let mut uses = Vec::new();
        let mut classes = Vec::new();
        let mut enums = Vec::new();
        let mut funcs = Vec::new();
        let mut entry = Vec::new();
        // The `use` lines come first, before any definition or entry
        // statement.
        loop {
            self.skip_newlines();
            if !matches!(self.peek(), Tok::KwUse) {
                break;
            }
            uses.push(self.use_decl()?);
        }
        loop {
            self.skip_newlines();
            match self.peek() {
                Tok::Eof => break,
                Tok::KwClass => classes.push(self.class_def()?),
                Tok::KwEnum => enums.push(self.enum_def()?),
                Tok::KwDef => funcs.push(self.func_def()?),
                Tok::KwUse => {
                    return Err(self.error(
                        "E1052",
                        "a `use` line must come before every definition and statement",
                    ));
                }
                _ => {
                    let stmt = self.stmt()?;
                    entry.push(stmt);
                    self.expect_terminator()?;
                }
            }
        }
        Ok(Module {
            uses,
            classes,
            enums,
            funcs,
            entry,
        })
    }

    /// Parse one `use` line: `use` plus one dotted path. The last
    /// segment becomes the bound name.
    fn use_decl(&mut self) -> Result<UseDecl, Diagnostic> {
        let start = self.peek_span();
        self.expect(Tok::KwUse, "`use`")?;
        let (first, first_span) = self.ident("a path segment after `use`")?;
        let mut path = vec![first];
        let mut name_span = first_span;
        while matches!(self.peek(), Tok::Dot) {
            self.pos += 1;
            let (segment, segment_span) = self.ident("a path segment after `.`")?;
            path.push(segment);
            name_span = segment_span;
        }
        let span = start.to(name_span);
        self.expect_terminator()?;
        Ok(UseDecl {
            path,
            span,
            name_span,
        })
    }

    /// Require a statement end: a newline, a semicolon, the end of the
    /// file, or a following block keyword.
    fn expect_terminator(&mut self) -> Result<(), Diagnostic> {
        match self.peek() {
            Tok::Newline => {
                self.pos += 1;
                Ok(())
            }
            Tok::Eof | Tok::KwEnd | Tok::KwElse | Tok::KwElsif | Tok::KwIn => Ok(()),
            other => Err(self.error(
                "E1001",
                format!("expected the end of the statement, found {other}"),
            )),
        }
    }

    /// Parse an optional `[T, U, effect e]` generic parameter list.
    fn generic_params(&mut self) -> Result<Vec<GenericParam>, Diagnostic> {
        if !matches!(self.peek(), Tok::LBracket) {
            return Ok(Vec::new());
        }
        self.pos += 1;
        let mut params = Vec::new();
        loop {
            let is_effect = if matches!(self.peek(), Tok::KwEffect) {
                self.pos += 1;
                true
            } else {
                false
            };
            let (name, span) = self.ident("a generic parameter name")?;
            if params.iter().any(|p: &GenericParam| p.name == name) {
                return Err(Diagnostic::new(
                    "E1014",
                    format!("duplicate generic parameter name `{name}`"),
                    span,
                ));
            }
            params.push(GenericParam {
                name,
                is_effect,
                span,
            });
            if matches!(self.peek(), Tok::Comma) {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.expect(Tok::RBracket, "`]` to complete the generic parameter list")?;
        Ok(params)
    }

    /// Parse an optional `with` effect row. In a comma-separated outer
    /// context an element followed by `:` belongs to the outer list,
    /// so the parser backs off before it.
    fn row_clause(&mut self) -> Result<Vec<RowItem>, Diagnostic> {
        if !matches!(self.peek(), Tok::KwWith) {
            return Ok(Vec::new());
        }
        self.pos += 1;
        let mut items = Vec::new();
        let first = self.row_item()?;
        items.push(first);
        loop {
            if !matches!(self.peek(), Tok::Comma) {
                break;
            }
            let save = self.pos;
            self.pos += 1; // consume the comma
            if !matches!(self.peek(), Tok::Ident(_)) {
                self.pos = save;
                break;
            }
            let item = self.row_item()?;
            if matches!(self.peek(), Tok::Colon) {
                // The name belongs to the outer parameter list.
                self.pos = save;
                break;
            }
            items.push(item);
        }
        Ok(items)
    }

    /// Parse one row element: `Name`, `Group.Op`, or an effect name.
    fn row_item(&mut self) -> Result<RowItem, Diagnostic> {
        let (mut name, mut span) = self.ident("an operation, group, or effect name")?;
        while matches!(self.peek(), Tok::Dot) {
            self.pos += 1;
            let (part, part_span) = self.ident("an operation name after `.`")?;
            name.push('.');
            name.push_str(&part);
            span = span.to(part_span);
        }
        Ok(RowItem { name, span })
    }

    fn class_def(&mut self) -> Result<ClassDef, Diagnostic> {
        let class_tok = self.expect(Tok::KwClass, "`class`")?;
        let (name, name_span) = self.ident("a class name")?;
        let generics = self.generic_params()?;
        let parent = if matches!(self.peek(), Tok::Lt) {
            self.pos += 1;
            Some(self.ident("a parent class name")?)
        } else {
            None
        };
        self.expect_terminator()?;
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        loop {
            self.skip_newlines();
            match self.peek() {
                Tok::KwEnd => break,
                Tok::KwDef => methods.push(self.method_def()?),
                Tok::Ident(_) => {
                    let (fname, fspan) = self.ident("a field name")?;
                    self.expect(Tok::Colon, "`:` and a field type")?;
                    let ty = self.type_expr()?;
                    let default = if matches!(self.peek(), Tok::Assign) {
                        self.pos += 1;
                        Some(self.expr()?)
                    } else {
                        None
                    };
                    let span = fspan.to(default.as_ref().map(|e| e.span).unwrap_or(ty.span));
                    fields.push(FieldDef {
                        name: fname,
                        ty,
                        default,
                        span,
                    });
                    self.expect_terminator()?;
                }
                Tok::Eof => {
                    return Err(self.error("E1003", "expected `end`, found end of file"));
                }
                other => {
                    return Err(self.error(
                        "E1003",
                        format!("expected a field, a method, or `end`, found {other}"),
                    ));
                }
            }
        }
        let end_tok = self.expect(Tok::KwEnd, "`end`")?;
        self.expect_terminator()?;
        Ok(ClassDef {
            name,
            name_span,
            generics,
            parent,
            fields,
            methods,
            span: class_tok.span.to(end_tok.span),
        })
    }

    fn enum_def(&mut self) -> Result<EnumDef, Diagnostic> {
        let enum_tok = self.expect(Tok::KwEnum, "`enum`")?;
        let (name, name_span) = self.ident("an enum name")?;
        let generics = self.generic_params()?;
        self.expect_terminator()?;
        let mut arms: Vec<ArmDef> = Vec::new();
        let mut methods = Vec::new();
        loop {
            self.skip_newlines();
            match self.peek() {
                Tok::KwEnd => break,
                Tok::KwDef => methods.push(self.method_def()?),
                Tok::Ident(_) if methods.is_empty() => {
                    arms.push(self.enum_arm()?);
                }
                Tok::Ident(_) => {
                    return Err(self.error("E1040", "enum arms must come before the enum methods"));
                }
                Tok::Eof => {
                    return Err(self.error("E1003", "expected `end`, found end of file"));
                }
                other => {
                    return Err(self.error(
                        "E1003",
                        format!("expected an arm, a method, or `end`, found {other}"),
                    ));
                }
            }
        }
        if arms.is_empty() {
            return Err(Diagnostic::new(
                "E1040",
                "an enum needs at least one arm",
                name_span,
            ));
        }
        let end_tok = self.expect(Tok::KwEnd, "`end`")?;
        self.expect_terminator()?;
        Ok(EnumDef {
            name,
            name_span,
            generics,
            arms,
            methods,
            span: enum_tok.span.to(end_tok.span),
        })
    }

    fn enum_arm(&mut self) -> Result<ArmDef, Diagnostic> {
        self.enter_nesting()?;
        let result = self.enum_arm_inner();
        self.depth -= 1;
        result
    }

    fn enum_arm_inner(&mut self) -> Result<ArmDef, Diagnostic> {
        let (name, name_span) = self.ident("an arm name")?;
        let mut fields = Vec::new();
        let mut span = name_span;
        if matches!(self.peek(), Tok::LParen) {
            self.pos += 1;
            if matches!(self.peek(), Tok::RParen) {
                return Err(self.error(
                    "E1040",
                    "an arm with `()` needs at least one field; \
                     write the arm name alone for a field-less arm",
                ));
            }
            loop {
                let (fname, fspan) = self.ident("a field name")?;
                if fields.iter().any(|(n, _)| *n == fname) {
                    return Err(Diagnostic::new(
                        "E1040",
                        format!("the arm already has a field named `{fname}`"),
                        fspan,
                    ));
                }
                self.expect(Tok::Colon, "`:` and a field type")?;
                let ty = self.type_expr()?;
                fields.push((fname, ty));
                if matches!(self.peek(), Tok::Comma) {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            let close = self.expect(Tok::RParen, "`)` to complete the arm fields")?;
            span = span.to(close.span);
        }
        self.expect_terminator()?;
        Ok(ArmDef {
            name,
            name_span,
            fields,
            span,
        })
    }

    fn method_def(&mut self) -> Result<MethodDef, Diagnostic> {
        let def_tok = self.expect(Tok::KwDef, "`def`")?;
        let (name, name_span) = self.ident("a method name")?;
        let generics = self.generic_params()?;
        self.expect(Tok::LParen, "`(`")?;
        let mut_self = match self.peek() {
            Tok::KwSelf => {
                self.pos += 1;
                false
            }
            Tok::KwMut if matches!(self.peek_at(1), Tok::KwSelf) => {
                self.pos += 2;
                true
            }
            _ => {
                return Err(self.error(
                    "E1023",
                    "a method must declare `self` or `mut self` as its first parameter",
                ));
            }
        };
        let mut params = Vec::new();
        if matches!(self.peek(), Tok::Comma) {
            self.pos += 1;
            params = self.param_list()?;
        }
        self.expect(Tok::RParen, "`)`")?;
        let ret = if matches!(self.peek(), Tok::Colon) {
            self.pos += 1;
            Some(self.type_expr()?)
        } else {
            None
        };
        let row = self.row_clause()?;
        let body = self.block(&[Tok::KwEnd])?;
        let end_tok = self.expect(Tok::KwEnd, "`end`")?;
        self.expect_terminator()?;
        Ok(MethodDef {
            name,
            name_span,
            generics,
            mut_self,
            params,
            ret,
            row,
            body,
            span: def_tok.span.to(end_tok.span),
        })
    }

    fn func_def(&mut self) -> Result<FuncDef, Diagnostic> {
        let def_tok = self.expect(Tok::KwDef, "`def`")?;
        let (name, name_span) = self.ident("a function name")?;
        let generics = self.generic_params()?;
        self.expect(Tok::LParen, "`(`")?;
        if matches!(self.peek(), Tok::KwSelf)
            || (matches!(self.peek(), Tok::KwMut) && matches!(self.peek_at(1), Tok::KwSelf))
        {
            return Err(self.error("E1023", "`self` is only valid in a method inside a class"));
        }
        let params = if matches!(self.peek(), Tok::RParen) {
            Vec::new()
        } else {
            self.param_list()?
        };
        self.expect(Tok::RParen, "`)`")?;
        let ret = if matches!(self.peek(), Tok::Colon) {
            self.pos += 1;
            Some(self.type_expr()?)
        } else {
            None
        };
        let row = self.row_clause()?;
        let body = self.block(&[Tok::KwEnd])?;
        let end_tok = self.expect(Tok::KwEnd, "`end`")?;
        self.expect_terminator()?;
        Ok(FuncDef {
            name,
            name_span,
            generics,
            params,
            ret,
            row,
            body,
            span: def_tok.span.to(end_tok.span),
        })
    }

    /// Parse one or more `[mut] name: Type` parameters.
    fn param_list(&mut self) -> Result<Vec<Param>, Diagnostic> {
        let mut params = Vec::new();
        loop {
            let mutable = if matches!(self.peek(), Tok::KwMut) {
                self.pos += 1;
                true
            } else {
                false
            };
            let (pname, pspan) = self.ident("a parameter name")?;
            self.expect(Tok::Colon, "`:` and a parameter type")?;
            let ty = self.type_expr()?;
            let span = pspan.to(ty.span);
            params.push(Param {
                name: pname,
                mutable,
                ty,
                span,
            });
            if matches!(self.peek(), Tok::Comma) {
                self.pos += 1;
            } else {
                break;
            }
        }
        Ok(params)
    }

    fn ident(&mut self, what: &str) -> Result<(String, Span), Diagnostic> {
        self.reject_reserved()?;
        match self.peek() {
            Tok::Ident(_) => {
                let token = self.next();
                match token.tok {
                    Tok::Ident(name) => Ok((name, token.span)),
                    _ => unreachable!(),
                }
            }
            other => Err(self.error("E1003", format!("expected {what}, found {other}"))),
        }
    }

    fn type_expr(&mut self) -> Result<TypeExpr, Diagnostic> {
        self.enter_nesting()?;
        let result = self.type_expr_inner();
        self.depth -= 1;
        result
    }

    fn type_expr_inner(&mut self) -> Result<TypeExpr, Diagnostic> {
        match self.peek() {
            Tok::Ident(_) => {
                let (mut name, mut span) = self.ident("a type")?;
                // A qualified type name such as `matrix.Matrix` names
                // one type through a `use` binding. The checker
                // resolves the dotted name.
                while matches!(self.peek(), Tok::Dot) {
                    self.pos += 1;
                    let (segment, segment_span) = self.ident("a type after `.`")?;
                    name.push('.');
                    name.push_str(&segment);
                    span = span.to(segment_span);
                }
                if matches!(self.peek(), Tok::LBracket) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    loop {
                        args.push(self.type_expr()?);
                        if matches!(self.peek(), Tok::Comma) {
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                    let close = self.expect(Tok::RBracket, "`]` to complete the type arguments")?;
                    Ok(TypeExpr {
                        kind: TypeExprKind::Apply(name, args),
                        span: span.to(close.span),
                    })
                } else {
                    Ok(TypeExpr {
                        kind: TypeExprKind::Name(name),
                        span,
                    })
                }
            }
            Tok::LBracket => {
                let open = self.next();
                let elem = self.type_expr()?;
                let close = self.expect(Tok::RBracket, "`]` to complete the list type")?;
                Ok(TypeExpr {
                    kind: TypeExprKind::ListShort(Box::new(elem)),
                    span: open.span.to(close.span),
                })
            }
            Tok::LBrace => {
                let open = self.next();
                let key = self.type_expr()?;
                self.expect(Tok::Colon, "`:` between the key and value types")?;
                let value = self.type_expr()?;
                let close = self.expect(Tok::RBrace, "`}` to complete the map type")?;
                Ok(TypeExpr {
                    kind: TypeExprKind::MapShort(Box::new(key), Box::new(value)),
                    span: open.span.to(close.span),
                })
            }
            Tok::LParen => {
                let open = self.next();
                let mut params = Vec::new();
                let mut muts = Vec::new();
                let mut first_mut_span = None;
                let mut trailing_comma = false;
                if !matches!(self.peek(), Tok::RParen) {
                    loop {
                        // A `mut` marker is valid only in a function
                        // type parameter list. The check runs after
                        // the arrow decides the form.
                        let is_mut = matches!(self.peek(), Tok::KwMut);
                        if is_mut {
                            let tok = self.next();
                            if first_mut_span.is_none() {
                                first_mut_span = Some(tok.span);
                            }
                        }
                        muts.push(is_mut);
                        params.push(self.type_expr()?);
                        if matches!(self.peek(), Tok::Comma) {
                            self.pos += 1;
                            if matches!(self.peek(), Tok::RParen) {
                                trailing_comma = true;
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                let close = self.expect(Tok::RParen, "`)`")?;
                if matches!(self.peek(), Tok::Arrow) {
                    self.pos += 1;
                    let ret = self.type_expr()?;
                    let row = self.row_clause()?;
                    let hi = row.last().map(|r| r.span).unwrap_or(ret.span);
                    let span = open.span.to(hi);
                    Ok(TypeExpr {
                        kind: TypeExprKind::Fn(params, muts, Box::new(ret), row),
                        span,
                    })
                } else if let Some(span) = first_mut_span {
                    Err(Diagnostic::new(
                        "E1001",
                        "`mut` is only valid before a parameter type in a \
                         function type",
                        span,
                    ))
                } else if params.is_empty() {
                    Ok(TypeExpr {
                        kind: TypeExprKind::Unit,
                        span: open.span.to(close.span),
                    })
                } else if params.len() >= 2 || trailing_comma {
                    if params.len() > MAX_TUPLE_ARITY {
                        return Err(Diagnostic::new(
                            "E1048",
                            format!(
                                "a tuple has a maximum arity of {MAX_TUPLE_ARITY}; \
                                 use a class for a larger record"
                            ),
                            open.span.to(close.span),
                        ));
                    }
                    Ok(TypeExpr {
                        kind: TypeExprKind::Tuple(params),
                        span: open.span.to(close.span),
                    })
                } else {
                    Err(self.error(
                        "E1003",
                        "expected `->` for a function type, or `,` for a tuple type",
                    ))
                }
            }
            other => Err(self.error("E1003", format!("expected a type, found {other}"))),
        }
    }

    fn block(&mut self, stops: &[Tok]) -> Result<Vec<Stmt>, Diagnostic> {
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if stops.contains(self.peek()) {
                break;
            }
            if matches!(self.peek(), Tok::Eof) {
                return Err(self.error("E1003", "expected `end`, found end of file"));
            }
            let stmt = self.stmt()?;
            stmts.push(stmt);
            self.expect_terminator()?;
        }
        Ok(stmts)
    }

    fn stmt(&mut self) -> Result<Stmt, Diagnostic> {
        self.enter_nesting()?;
        let result = self.stmt_inner();
        self.depth -= 1;
        result
    }

    fn stmt_inner(&mut self) -> Result<Stmt, Diagnostic> {
        self.reject_reserved()?;
        match self.peek() {
            Tok::KwDef => Err(self.error(
                "E1002",
                "a `def` function is only valid at the top level of a module or in a class",
            )),
            Tok::KwClass => Err(self.error(
                "E1002",
                "a `class` is only valid at the top level of a module",
            )),
            Tok::KwEnum => Err(self.error(
                "E1002",
                "an `enum` is only valid at the top level of a module",
            )),
            Tok::KwReturn => {
                let ret_tok = self.next();
                let value = match self.peek() {
                    Tok::Newline
                    | Tok::Eof
                    | Tok::KwEnd
                    | Tok::KwElse
                    | Tok::KwElsif
                    | Tok::KwIn => None,
                    _ => Some(self.expr()?),
                };
                let span = match &value {
                    Some(v) => ret_tok.span.to(v.span),
                    None => ret_tok.span,
                };
                Ok(Stmt {
                    kind: StmtKind::Return { value },
                    span,
                })
            }
            Tok::KwBreak => {
                let token = self.next();
                Ok(Stmt {
                    kind: StmtKind::Break,
                    span: token.span,
                })
            }
            Tok::KwContinue => {
                let token = self.next();
                Ok(Stmt {
                    kind: StmtKind::Continue,
                    span: token.span,
                })
            }
            Tok::KwWhile => {
                let while_tok = self.next();
                let cond = self.expr()?;
                let body = self.block(&[Tok::KwEnd])?;
                let end_tok = self.expect(Tok::KwEnd, "`end`")?;
                Ok(Stmt {
                    kind: StmtKind::While { cond, body },
                    span: while_tok.span.to(end_tok.span),
                })
            }
            Tok::KwLoop => {
                // `loop [do] ... end` is sugar for `while true`.
                let loop_tok = self.next();
                if matches!(self.peek(), Tok::KwDo) {
                    self.pos += 1;
                }
                let body = self.block(&[Tok::KwEnd])?;
                let end_tok = self.expect(Tok::KwEnd, "`end`")?;
                let span = loop_tok.span.to(end_tok.span);
                Ok(Stmt {
                    kind: StmtKind::While {
                        cond: Expr {
                            kind: ExprKind::Bool(true),
                            span: loop_tok.span,
                        },
                        body,
                    },
                    span,
                })
            }
            Tok::Ident(_) if matches!(self.peek_at(1), Tok::Colon) => {
                let (name, name_span) = self.ident("a name")?;
                self.pos += 1; // consume `:`
                let ty = self.type_expr()?;
                self.expect(Tok::Assign, "`=` after the type annotation")?;
                let value = self.expr()?;
                let span = name_span.to(value.span);
                Ok(Stmt {
                    kind: StmtKind::Assign {
                        name,
                        name_span,
                        ty: Some(ty),
                        value,
                    },
                    span,
                })
            }
            _ => {
                let expr = self.expr()?;
                if matches!(self.peek(), Tok::Assign) {
                    self.pos += 1; // consume `=`
                    let value = self.expr()?;
                    let span = expr.span.to(value.span);
                    return match expr.kind {
                        ExprKind::Name(name) => Ok(Stmt {
                            kind: StmtKind::Assign {
                                name,
                                name_span: expr.span,
                                ty: None,
                                value,
                            },
                            span,
                        }),
                        ExprKind::Field {
                            recv,
                            name,
                            name_span,
                        } => Ok(Stmt {
                            kind: StmtKind::AssignField {
                                recv: *recv,
                                field: name,
                                field_span: name_span,
                                value,
                            },
                            span,
                        }),
                        ExprKind::Index { .. } => Err(Diagnostic::new(
                            "E1002",
                            "index assignment is not supported in this language slice. \
                             For a map, use the `put` method. A list has no element \
                             write in this slice.",
                            expr.span,
                        )),
                        _ => Err(Diagnostic::new(
                            "E1002",
                            "this expression is not a valid assignment target",
                            expr.span,
                        )),
                    };
                }
                let span = expr.span;
                Ok(Stmt {
                    kind: StmtKind::Expr(expr),
                    span,
                })
            }
        }
    }

    /// Record one more nesting level, or reject input that is too deep.
    fn enter_nesting(&mut self) -> Result<(), Diagnostic> {
        self.depth += 1;
        if self.depth > MAX_NEST_DEPTH {
            return Err(self.error(
                "E1022",
                format!("the nesting is deeper than the limit of {MAX_NEST_DEPTH} levels"),
            ));
        }
        Ok(())
    }

    fn expr(&mut self) -> Result<Expr, Diagnostic> {
        self.enter_nesting()?;
        let result = self.or_expr();
        self.depth -= 1;
        result
    }

    fn or_expr(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.and_expr()?;
        while matches!(self.peek(), Tok::KwOr) {
            self.pos += 1;
            let right = self.and_expr()?;
            let span = left.span.to(right.span);
            left = Expr {
                kind: ExprKind::Or(Box::new(left), Box::new(right)),
                span,
            };
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.eq_expr()?;
        while matches!(self.peek(), Tok::KwAnd) {
            self.pos += 1;
            let right = self.eq_expr()?;
            let span = left.span.to(right.span);
            left = Expr {
                kind: ExprKind::And(Box::new(left), Box::new(right)),
                span,
            };
        }
        Ok(left)
    }

    /// Equality, `is`, and `as` share one precedence level.
    fn eq_expr(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.ord_expr()?;
        loop {
            match self.peek() {
                Tok::EqEq | Tok::NotEq => {
                    let op = if matches!(self.peek(), Tok::EqEq) {
                        BinOp::Eq
                    } else {
                        BinOp::Ne
                    };
                    self.pos += 1;
                    let right = self.ord_expr()?;
                    let span = left.span.to(right.span);
                    left = Expr {
                        kind: ExprKind::Binary {
                            op,
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                        span,
                    };
                }
                Tok::KwIs => {
                    self.pos += 1;
                    let ty = self.type_expr()?;
                    let span = left.span.to(ty.span);
                    left = Expr {
                        kind: ExprKind::Is {
                            value: Box::new(left),
                            ty,
                        },
                        span,
                    };
                }
                Tok::KwAs => {
                    self.pos += 1;
                    let ty = self.type_expr()?;
                    let span = left.span.to(ty.span);
                    left = Expr {
                        kind: ExprKind::Cast {
                            value: Box::new(left),
                            ty,
                        },
                        span,
                    };
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn ord_expr(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.add_expr()?;
        loop {
            let op = match self.peek() {
                Tok::Lt => BinOp::Lt,
                Tok::Le => BinOp::Le,
                Tok::Gt => BinOp::Gt,
                Tok::Ge => BinOp::Ge,
                _ => break,
            };
            self.pos += 1;
            let right = self.add_expr()?;
            let span = left.span.to(right.span);
            left = Expr {
                kind: ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span,
            };
        }
        Ok(left)
    }

    fn add_expr(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.mul_expr()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let right = self.mul_expr()?;
            let span = left.span.to(right.span);
            left = Expr {
                kind: ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span,
            };
        }
        Ok(left)
    }

    fn mul_expr(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.unary_expr()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Rem,
                _ => break,
            };
            self.pos += 1;
            let right = self.unary_expr()?;
            let span = left.span.to(right.span);
            left = Expr {
                kind: ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span,
            };
        }
        Ok(left)
    }

    fn unary_expr(&mut self) -> Result<Expr, Diagnostic> {
        self.enter_nesting()?;
        let result = self.unary_expr_inner();
        self.depth -= 1;
        result
    }

    fn unary_expr_inner(&mut self) -> Result<Expr, Diagnostic> {
        match self.peek() {
            Tok::KwNot => {
                let token = self.next();
                let inner = self.unary_expr()?;
                let span = token.span.to(inner.span);
                Ok(Expr {
                    kind: ExprKind::Not(Box::new(inner)),
                    span,
                })
            }
            Tok::Minus => {
                let token = self.next();
                let inner = self.unary_expr()?;
                let span = token.span.to(inner.span);
                Ok(Expr {
                    kind: ExprKind::Neg(Box::new(inner)),
                    span,
                })
            }
            _ => self.postfix_expr(),
        }
    }

    /// Try to parse `[Type, ...]` followed by `(`. Return `None` and
    /// restore the position when the brackets are not type arguments.
    fn try_type_args(&mut self) -> Option<Vec<TypeExpr>> {
        let save_pos = self.pos;
        let save_depth = self.depth;
        let result = self.type_args_inner();
        match result {
            Ok(args) if matches!(self.peek(), Tok::LParen) => Some(args),
            _ => {
                self.pos = save_pos;
                self.depth = save_depth;
                None
            }
        }
    }

    fn type_args_inner(&mut self) -> Result<Vec<TypeExpr>, Diagnostic> {
        self.expect(Tok::LBracket, "`[`")?;
        let mut args = Vec::new();
        loop {
            args.push(self.type_expr()?);
            if matches!(self.peek(), Tok::Comma) {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.expect(Tok::RBracket, "`]`")?;
        Ok(args)
    }

    fn postfix_expr(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.primary_expr()?;
        loop {
            match self.peek() {
                Tok::LParen => {
                    let (args, close_span) = self.call_args()?;
                    let span = expr.span.to(close_span);
                    expr = self.make_call(expr, Vec::new(), args, span)?;
                }
                Tok::LBracket
                    if matches!(expr.kind, ExprKind::Name(_) | ExprKind::Field { .. }) =>
                {
                    // Either explicit generic call arguments or an
                    // index expression. Try type arguments first.
                    match self.try_type_args() {
                        Some(type_args) => {
                            let (args, close_span) = self.call_args()?;
                            let span = expr.span.to(close_span);
                            expr = self.make_call(expr, type_args, args, span)?;
                        }
                        None => {
                            expr = self.index_expr(expr)?;
                        }
                    }
                }
                Tok::LBracket => {
                    expr = self.index_expr(expr)?;
                }
                Tok::Dot => {
                    self.pos += 1;
                    let (name, name_span) = self.ident("a field or method name")?;
                    let span = expr.span.to(name_span);
                    expr = Expr {
                        kind: ExprKind::Field {
                            recv: Box::new(expr),
                            name,
                            name_span,
                        },
                        span,
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// Parse `(arg, ...)` and return the arguments and the span of `)`.
    /// An argument may carry a label, for example `args: ()`. The
    /// checker validates where a label is permitted.
    fn call_args(&mut self) -> Result<(Vec<Expr>, Span), Diagnostic> {
        self.expect(Tok::LParen, "`(`")?;
        let mut args = Vec::new();
        if !matches!(self.peek(), Tok::RParen) {
            loop {
                if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Colon) {
                    let (label, label_span) = self.ident("an argument label")?;
                    self.pos += 1; // consume `:`
                    let value = self.expr()?;
                    let span = label_span.to(value.span);
                    args.push(Expr {
                        kind: ExprKind::Labeled {
                            label,
                            value: Box::new(value),
                        },
                        span,
                    });
                } else {
                    args.push(self.expr()?);
                }
                if matches!(self.peek(), Tok::Comma) {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        let close = self.expect(Tok::RParen, "`)` to complete the call")?;
        Ok((args, close.span))
    }

    /// Build the call node for a callee expression.
    fn make_call(
        &self,
        callee: Expr,
        type_args: Vec<TypeExpr>,
        args: Vec<Expr>,
        span: Span,
    ) -> Result<Expr, Diagnostic> {
        let kind = match callee.kind {
            ExprKind::Name(name) => ExprKind::Call {
                name,
                name_span: callee.span,
                type_args,
                args,
            },
            ExprKind::Field {
                recv,
                name,
                name_span,
            } => ExprKind::MethodCall {
                recv,
                name,
                name_span,
                type_args,
                args,
            },
            _ if type_args.is_empty() => ExprKind::CallExpr {
                callee: Box::new(callee),
                args,
            },
            _ => {
                return Err(Diagnostic::new(
                    "E1003",
                    "type arguments are only valid on a named call",
                    span,
                ));
            }
        };
        Ok(Expr { kind, span })
    }

    fn index_expr(&mut self, recv: Expr) -> Result<Expr, Diagnostic> {
        self.expect(Tok::LBracket, "`[`")?;
        let index = self.expr()?;
        let close = self.expect(Tok::RBracket, "`]` to complete the index")?;
        let span = recv.span.to(close.span);
        Ok(Expr {
            kind: ExprKind::Index {
                recv: Box::new(recv),
                index: Box::new(index),
            },
            span,
        })
    }

    fn primary_expr(&mut self) -> Result<Expr, Diagnostic> {
        self.reject_reserved()?;
        match self.peek() {
            Tok::Int(_) => {
                let token = self.next();
                match token.tok {
                    Tok::Int(v) => Ok(Expr {
                        kind: ExprKind::Int(v),
                        span: token.span,
                    }),
                    _ => unreachable!(),
                }
            }
            Tok::Str(_) => {
                let token = self.next();
                match token.tok {
                    Tok::Str(v) => Ok(Expr {
                        kind: ExprKind::Str(v),
                        span: token.span,
                    }),
                    _ => unreachable!(),
                }
            }
            Tok::StrInterp(_) => {
                let token = self.next();
                let pieces = match token.tok {
                    Tok::StrInterp(pieces) => pieces,
                    _ => unreachable!(),
                };
                let mut parts = Vec::new();
                for piece in pieces {
                    match piece {
                        StrPiece::Lit(text) => parts.push(InterpPart::Lit(text)),
                        StrPiece::Expr(tokens) => {
                            let mut sub = Parser {
                                tokens,
                                pos: 0,
                                depth: 0,
                            };
                            let inner = sub.expr()?;
                            if !matches!(sub.peek(), Tok::Eof) {
                                return Err(sub.error(
                                    "E1003",
                                    format!(
                                        "expected the end of the interpolation \
                                         expression, found {}",
                                        sub.peek()
                                    ),
                                ));
                            }
                            parts.push(InterpPart::Expr(inner));
                        }
                    }
                }
                Ok(Expr {
                    kind: ExprKind::Interp(parts),
                    span: token.span,
                })
            }
            Tok::KwTrue => {
                let token = self.next();
                Ok(Expr {
                    kind: ExprKind::Bool(true),
                    span: token.span,
                })
            }
            Tok::KwFalse => {
                let token = self.next();
                Ok(Expr {
                    kind: ExprKind::Bool(false),
                    span: token.span,
                })
            }
            Tok::KwSelf => {
                let token = self.next();
                Ok(Expr {
                    kind: ExprKind::SelfRef,
                    span: token.span,
                })
            }
            Tok::KwSuper => {
                let super_tok = self.next();
                self.expect(Tok::Dot, "`.` after `super`")?;
                let (name, name_span) = self.ident("a method name")?;
                self.expect(Tok::LParen, "`(` to call the superclass method")?;
                let mut args = Vec::new();
                if !matches!(self.peek(), Tok::RParen) {
                    loop {
                        args.push(self.expr()?);
                        if matches!(self.peek(), Tok::Comma) {
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                }
                let close = self.expect(Tok::RParen, "`)` to complete the call")?;
                Ok(Expr {
                    kind: ExprKind::SuperCall {
                        name,
                        name_span,
                        args,
                    },
                    span: super_tok.span.to(close.span),
                })
            }
            Tok::Ident(_) => {
                let (name, span) = self.ident("a name")?;
                Ok(Expr {
                    kind: ExprKind::Name(name),
                    span,
                })
            }
            Tok::LParen => {
                let open = self.next();
                if matches!(self.peek(), Tok::RParen) {
                    let close = self.next();
                    return Ok(Expr {
                        kind: ExprKind::Unit,
                        span: open.span.to(close.span),
                    });
                }
                let first = self.expr()?;
                if matches!(self.peek(), Tok::Comma) {
                    let mut items = vec![first];
                    while matches!(self.peek(), Tok::Comma) {
                        self.pos += 1;
                        if matches!(self.peek(), Tok::RParen) {
                            break;
                        }
                        items.push(self.expr()?);
                    }
                    let close = self.expect(Tok::RParen, "`)` to complete the tuple")?;
                    if items.len() > MAX_TUPLE_ARITY {
                        return Err(Diagnostic::new(
                            "E1048",
                            format!(
                                "a tuple has a maximum arity of {MAX_TUPLE_ARITY}; \
                                 use a class for a larger record"
                            ),
                            open.span.to(close.span),
                        ));
                    }
                    return Ok(Expr {
                        kind: ExprKind::TupleLit(items),
                        span: open.span.to(close.span),
                    });
                }
                self.expect(Tok::RParen, "`)`")?;
                Ok(first)
            }
            Tok::LBracket => {
                let open = self.next();
                let mut items = Vec::new();
                if !matches!(self.peek(), Tok::RBracket) {
                    loop {
                        items.push(self.expr()?);
                        if matches!(self.peek(), Tok::Comma) {
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                }
                let close = self.expect(Tok::RBracket, "`]` to complete the list")?;
                Ok(Expr {
                    kind: ExprKind::ListLit(items),
                    span: open.span.to(close.span),
                })
            }
            Tok::LBrace => {
                let open = self.next();
                let mut entries = Vec::new();
                if !matches!(self.peek(), Tok::RBrace) {
                    loop {
                        let key = self.expr()?;
                        self.expect(Tok::Colon, "`:` between the key and the value")?;
                        let value = self.expr()?;
                        entries.push((key, value));
                        if matches!(self.peek(), Tok::Comma) {
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                }
                let close = self.expect(Tok::RBrace, "`}` to complete the map")?;
                Ok(Expr {
                    kind: ExprKind::MapLit(entries),
                    span: open.span.to(close.span),
                })
            }
            Tok::KwDo => self.closure_expr(),
            Tok::KwIf => self.if_expr(),
            Tok::KwCase => self.case_expr(),
            other => Err(self.error("E1001", format!("expected an expression, found {other}"))),
        }
    }

    fn closure_expr(&mut self) -> Result<Expr, Diagnostic> {
        let do_tok = self.expect(Tok::KwDo, "`do`")?;
        self.expect(Tok::Pipe, "`|` to open the parameter list")?;
        let params = if matches!(self.peek(), Tok::Pipe) {
            Vec::new()
        } else {
            self.param_list()?
        };
        self.expect(Tok::Pipe, "`|` to close the parameter list")?;
        let ret = if matches!(self.peek(), Tok::Colon) {
            self.pos += 1;
            Some(self.type_expr()?)
        } else {
            None
        };
        let row = self.row_clause()?;
        let body = self.block(&[Tok::KwEnd])?;
        let end_tok = self.expect(Tok::KwEnd, "`end`")?;
        Ok(Expr {
            kind: ExprKind::Closure {
                params,
                ret,
                row,
                body,
            },
            span: do_tok.span.to(end_tok.span),
        })
    }

    fn if_expr(&mut self) -> Result<Expr, Diagnostic> {
        let if_tok = self.expect(Tok::KwIf, "`if`")?;
        let mut arms = Vec::new();
        let else_body;
        loop {
            let cond = self.expr()?;
            let body = self.block(&[Tok::KwElsif, Tok::KwElse, Tok::KwEnd])?;
            arms.push((cond, body));
            match self.peek() {
                Tok::KwElsif => {
                    self.pos += 1;
                }
                Tok::KwElse => {
                    self.pos += 1;
                    else_body = Some(self.block(&[Tok::KwEnd])?);
                    break;
                }
                _ => {
                    else_body = None;
                    break;
                }
            }
        }
        let end_tok = self.expect(Tok::KwEnd, "`end`")?;
        Ok(Expr {
            kind: ExprKind::If { arms, else_body },
            span: if_tok.span.to(end_tok.span),
        })
    }

    fn case_expr(&mut self) -> Result<Expr, Diagnostic> {
        let case_tok = self.expect(Tok::KwCase, "`case`")?;
        let scrut = self.expr()?;
        let mut arms = Vec::new();
        loop {
            self.skip_newlines();
            match self.peek() {
                Tok::KwIn => {
                    let in_tok = self.next();
                    let pattern = self.pattern()?;
                    let (body, hi) = if matches!(self.peek(), Tok::KwThen) {
                        self.pos += 1;
                        let value = self.expr()?;
                        let hi = value.span;
                        let span = value.span;
                        (
                            vec![Stmt {
                                kind: StmtKind::Expr(value),
                                span,
                            }],
                            hi,
                        )
                    } else {
                        self.expect_terminator()?;
                        let body = self.block(&[Tok::KwIn, Tok::KwEnd])?;
                        let hi = body.last().map(|s| s.span).unwrap_or(pattern.span);
                        (body, hi)
                    };
                    arms.push(CaseArm {
                        span: in_tok.span.to(hi),
                        pattern,
                        body,
                    });
                }
                Tok::KwEnd => break,
                Tok::Eof => {
                    return Err(self.error("E1003", "expected `end`, found end of file"));
                }
                other => {
                    return Err(self.error(
                        "E1003",
                        format!("expected `in` or `end` in the case, found {other}"),
                    ));
                }
            }
        }
        if arms.is_empty() {
            return Err(Diagnostic::new(
                "E1041",
                "a case needs at least one `in` arm",
                case_tok.span,
            ));
        }
        let end_tok = self.expect(Tok::KwEnd, "`end`")?;
        Ok(Expr {
            kind: ExprKind::Case {
                scrut: Box::new(scrut),
                arms,
            },
            span: case_tok.span.to(end_tok.span),
        })
    }

    fn pattern(&mut self) -> Result<Pattern, Diagnostic> {
        self.enter_nesting()?;
        let result = self.pattern_inner();
        self.depth -= 1;
        result
    }

    fn pattern_inner(&mut self) -> Result<Pattern, Diagnostic> {
        self.reject_reserved()?;
        match self.peek() {
            Tok::Int(_) => {
                let token = self.next();
                match token.tok {
                    Tok::Int(v) => Ok(Pattern {
                        kind: PatternKind::Int(v),
                        span: token.span,
                    }),
                    _ => unreachable!(),
                }
            }
            Tok::Minus => {
                let minus = self.next();
                match self.peek() {
                    Tok::Int(_) => {
                        let token = self.next();
                        match token.tok {
                            Tok::Int(v) => Ok(Pattern {
                                kind: PatternKind::Int(-v),
                                span: minus.span.to(token.span),
                            }),
                            _ => unreachable!(),
                        }
                    }
                    other => Err(self.error(
                        "E1041",
                        format!("expected an integer literal after `-`, found {other}"),
                    )),
                }
            }
            Tok::KwTrue => {
                let token = self.next();
                Ok(Pattern {
                    kind: PatternKind::Bool(true),
                    span: token.span,
                })
            }
            Tok::KwFalse => {
                let token = self.next();
                Ok(Pattern {
                    kind: PatternKind::Bool(false),
                    span: token.span,
                })
            }
            Tok::Str(_) => {
                let token = self.next();
                match token.tok {
                    Tok::Str(v) => Ok(Pattern {
                        kind: PatternKind::Str(v),
                        span: token.span,
                    }),
                    _ => unreachable!(),
                }
            }
            Tok::StrInterp(_) => {
                Err(self.error("E1041", "an interpolated string is not a valid pattern"))
            }
            Tok::Ident(_) => {
                let (name, span) = self.ident("a pattern")?;
                if name == "_" {
                    return Ok(Pattern {
                        kind: PatternKind::Wildcard,
                        span,
                    });
                }
                let (qualifier, ctor_name, mut hi) = if matches!(self.peek(), Tok::Dot) {
                    self.pos += 1;
                    let (arm, arm_span) = self.ident("an arm name after `.`")?;
                    (Some(name), arm, arm_span)
                } else {
                    (None, name, span)
                };
                if matches!(self.peek(), Tok::LParen) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if !matches!(self.peek(), Tok::RParen) {
                        loop {
                            args.push(self.pattern()?);
                            if matches!(self.peek(), Tok::Comma) {
                                self.pos += 1;
                            } else {
                                break;
                            }
                        }
                    }
                    let close = self.expect(Tok::RParen, "`)` to complete the pattern")?;
                    hi = close.span;
                    return Ok(Pattern {
                        kind: PatternKind::Ctor {
                            qualifier,
                            name: ctor_name,
                            args,
                            has_parens: true,
                        },
                        span: span.to(hi),
                    });
                }
                if qualifier.is_some() {
                    return Ok(Pattern {
                        kind: PatternKind::Ctor {
                            qualifier,
                            name: ctor_name,
                            args: Vec::new(),
                            has_parens: false,
                        },
                        span: span.to(hi),
                    });
                }
                Ok(Pattern {
                    kind: PatternKind::Name(ctor_name),
                    span,
                })
            }
            other => Err(self.error("E1041", format!("expected a pattern, found {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::dump_module;

    #[test]
    fn parses_factorial() {
        let source = "def factorial(n: Int): Int\n  if n <= 1\n    1\n  else\n    \
                      n * factorial(n - 1)\n  end\nend\n\nfactorial(10)\n";
        let module = parse(source).unwrap();
        assert_eq!(module.funcs.len(), 1);
        assert_eq!(module.funcs[0].name, "factorial");
        assert_eq!(module.entry.len(), 1);
    }

    #[test]
    fn parses_counter_class() {
        let source = "class Counter\n  value: Int = 0\n\n  def add(mut self, n: Int): Int\n    \
                      self.value = self.value + n\n    self.value\n  end\nend\n\nc = Counter()\n\
                      c.add(2)\nc.add(3)\n";
        let module = parse(source).unwrap();
        assert_eq!(module.classes.len(), 1);
        let class = &module.classes[0];
        assert_eq!(class.name, "Counter");
        assert_eq!(class.fields.len(), 1);
        assert_eq!(class.methods.len(), 1);
        assert!(class.methods[0].mut_self);
        assert_eq!(module.entry.len(), 3);
    }

    #[test]
    fn parses_inheritance_and_super() {
        let source = "class Animal\n  def init(mut self, n: Int)\n  end\nend\n\
                      class Dog < Animal\n  def init(mut self)\n    super.init(1)\n  end\nend\n1\n";
        let module = parse(source).unwrap();
        assert_eq!(module.classes.len(), 2);
        assert_eq!(
            module.classes[1].parent,
            Some((
                "Animal".to_string(),
                module.classes[1].parent.as_ref().unwrap().1
            ))
        );
    }

    #[test]
    fn parses_collection_literals_and_indexing() {
        let source = "words = [\"a\", \"b\"]\ncounts: {String: Int} = {}\n\
                      counts.put(words[0], 1)\nwords.len()\n";
        let module = parse(source).unwrap();
        assert_eq!(module.entry.len(), 4);
    }

    #[test]
    fn parses_closures_and_function_types() {
        let source = "adders: [(Int) -> Int] = [do |x: Int|: Int x + 1 end]\n\
                      f = adders.at(0)\nf(41)\n";
        let module = parse(source).unwrap();
        assert_eq!(module.entry.len(), 3);
    }

    #[test]
    fn parses_empty_closure_params() {
        let module = parse("t = do || 42 end\nt()\n").unwrap();
        assert_eq!(module.entry.len(), 2);
    }

    #[test]
    fn parses_field_assignment_targets() {
        let module = parse("class A\n  x: Int = 0\nend\na = A()\na.x = 3\na.x\n").unwrap();
        assert!(matches!(module.entry[1].kind, StmtKind::AssignField { .. }));
    }

    #[test]
    fn rejects_index_assignment() {
        let err = parse("m = {1: 2}\nm[1] = 3\n").unwrap_err();
        assert_eq!(err.code, "E1002");
        assert!(err.message.contains("put"), "{}", err.message);
    }

    #[test]
    fn rejects_method_without_self() {
        let err = parse("class A\n  def f(n: Int): Int\n    n\n  end\nend\n1\n").unwrap_err();
        assert_eq!(err.code, "E1023");
    }

    #[test]
    fn rejects_self_in_top_level_def() {
        let err = parse("def f(self): Int\n  1\nend\n1\n").unwrap_err();
        assert_eq!(err.code, "E1023");
    }

    #[test]
    fn dump_is_stable() {
        let module = parse("x = 1\nx + 2\n").unwrap();
        let dump = dump_module(&module);
        assert_eq!(
            dump,
            "module\n  entry\n    assign x\n      int 1\n    expr\n      binary +\n        \
             name x\n        int 2\n"
        );
    }

    #[test]
    fn parses_loop_as_while_true() {
        let module = parse("loop do\nbreak\nend\n1\n").unwrap();
        match &module.entry[0].kind {
            StmtKind::While { cond, .. } => {
                assert!(matches!(cond.kind, ExprKind::Bool(true)));
            }
            other => panic!("expected a while, got {other:?}"),
        }
        // The `do` is optional.
        assert!(parse("loop\nbreak\nend\n1\n").is_ok());
    }

    #[test]
    fn parses_tuple_literals() {
        let module = parse("(1, 2)\n(\"only\",)\n()\n").unwrap();
        assert_eq!(module.entry.len(), 3);
        match &module.entry[0].kind {
            StmtKind::Expr(e) => assert!(matches!(e.kind, ExprKind::TupleLit(_))),
            other => panic!("expected a tuple, got {other:?}"),
        }
        match &module.entry[1].kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::TupleLit(items) => assert_eq!(items.len(), 1),
                other => panic!("expected a one-element tuple, got {other:?}"),
            },
            other => panic!("expected an expression, got {other:?}"),
        }
        match &module.entry[2].kind {
            StmtKind::Expr(e) => assert!(matches!(e.kind, ExprKind::Unit)),
            other => panic!("expected the unit literal, got {other:?}"),
        }
    }

    #[test]
    fn parses_tuple_types() {
        let module = parse("p: (Int, String) = (1, \"a\")\nq: (Int,) = (2,)\np\n").unwrap();
        assert_eq!(module.entry.len(), 3);
    }

    #[test]
    fn rejects_overlong_tuples() {
        let items: Vec<String> = (0..17).map(|i| i.to_string()).collect();
        let text = format!("({})\n", items.join(", "));
        assert_eq!(parse(&text).unwrap_err().code, "E1048");
    }

    #[test]
    fn parses_enum_with_methods() {
        let source = "enum Option[T]\n  Some(v: T)\n  None\n\n  def is_some(self): Bool\n    \
                      case self\n    in Some(_) then true\n    in None    then false\n    \
                      end\n  end\nend\n1\n";
        let module = parse(source).unwrap();
        assert_eq!(module.enums.len(), 1);
        let e = &module.enums[0];
        assert_eq!(e.arms.len(), 2);
        assert_eq!(e.arms[0].fields.len(), 1);
        assert_eq!(e.methods.len(), 1);
        assert_eq!(e.generics.len(), 1);
    }

    #[test]
    fn parses_case_with_nested_patterns() {
        let source = "case x\nin Pair(Some(a), None)\n  a\nin _\n  0\nend\n";
        let module = parse(source).unwrap();
        match &module.entry[0].kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Case { arms, .. } => {
                    assert_eq!(arms.len(), 2);
                    match &arms[0].pattern.kind {
                        PatternKind::Ctor { name, args, .. } => {
                            assert_eq!(name, "Pair");
                            assert_eq!(args.len(), 2);
                        }
                        other => panic!("expected a constructor pattern, got {other:?}"),
                    }
                }
                other => panic!("expected a case, got {other:?}"),
            },
            other => panic!("expected an expression, got {other:?}"),
        }
    }

    #[test]
    fn parses_qualified_constructor_patterns() {
        let source = "case x\nin Option.Some(v) then v\nin Option.None then 0\nend\n";
        let module = parse(source).unwrap();
        match &module.entry[0].kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Case { arms, .. } => match &arms[1].pattern.kind {
                    PatternKind::Ctor {
                        qualifier, name, ..
                    } => {
                        assert_eq!(qualifier.as_deref(), Some("Option"));
                        assert_eq!(name, "None");
                    }
                    other => panic!("expected a qualified pattern, got {other:?}"),
                },
                other => panic!("expected a case, got {other:?}"),
            },
            other => panic!("expected an expression, got {other:?}"),
        }
    }

    #[test]
    fn parses_generic_def_with_row() {
        let source = "def apply[T, U, effect e](x: T, f: (T) -> U with e): U with e\n  \
                      f(x)\nend\n1\n";
        let module = parse(source).unwrap();
        let f = &module.funcs[0];
        assert_eq!(f.generics.len(), 3);
        assert!(f.generics[2].is_effect);
        assert_eq!(f.row.len(), 1);
        assert_eq!(f.row[0].name, "e");
        match &f.params[1].ty.kind {
            TypeExprKind::Fn(_, _, _, row) => assert_eq!(row.len(), 1),
            other => panic!("expected a function type, got {other:?}"),
        }
    }

    #[test]
    fn parses_explicit_generic_call_arguments() {
        let module = parse("choose[String](a, b)\nxs[0]\n").unwrap();
        match &module.entry[0].kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Call { type_args, .. } => assert_eq!(type_args.len(), 1),
                other => panic!("expected a generic call, got {other:?}"),
            },
            other => panic!("expected an expression, got {other:?}"),
        }
        match &module.entry[1].kind {
            StmtKind::Expr(e) => assert!(matches!(e.kind, ExprKind::Index { .. })),
            other => panic!("expected an index, got {other:?}"),
        }
    }

    #[test]
    fn parses_is_and_as() {
        let module = parse("a is Dog\na as Dog\n").unwrap();
        match &module.entry[0].kind {
            StmtKind::Expr(e) => assert!(matches!(e.kind, ExprKind::Is { .. })),
            other => panic!("expected `is`, got {other:?}"),
        }
        match &module.entry[1].kind {
            StmtKind::Expr(e) => assert!(matches!(e.kind, ExprKind::Cast { .. })),
            other => panic!("expected `as`, got {other:?}"),
        }
    }

    #[test]
    fn rejects_nested_def() {
        let err =
            parse("def outer(): Int\n  def inner(): Int\n    1\n  end\n  1\nend\n").unwrap_err();
        assert_eq!(err.code, "E1002");
    }

    #[test]
    fn rejects_missing_end() {
        assert_eq!(parse("if true\n  1\n").unwrap_err().code, "E1003");
    }

    #[test]
    fn rejects_two_statements_on_one_line() {
        assert_eq!(parse("x = 1 y = 2\n").unwrap_err().code, "E1001");
    }

    #[test]
    fn parses_annotated_assignment() {
        let module = parse("x: Int = 3\n").unwrap();
        assert_eq!(module.entry.len(), 1);
    }

    #[test]
    fn parses_interpolation_expression() {
        let module = parse("name = \"Ada\"\n\"Hello {name}!\"\n").unwrap();
        assert_eq!(module.entry.len(), 2);
        match &module.entry[1].kind {
            StmtKind::Expr(e) => assert!(matches!(e.kind, ExprKind::Interp(_))),
            other => panic!("expected an expression, got {other:?}"),
        }
    }

    #[test]
    fn parses_bare_and_valued_return() {
        let source = "def f(): Int\n  return 1\nend\ndef g()\n  return\nend\n1\n";
        let module = parse(source).unwrap();
        assert_eq!(module.funcs.len(), 2);
    }

    #[test]
    fn row_backs_off_before_a_parameter_name() {
        let source = "def f(g: (Int) -> Int with e, h: Int): Int\n  h\nend\n1\n";
        let module = parse(source).unwrap();
        assert_eq!(module.funcs[0].params.len(), 2);
        assert_eq!(module.funcs[0].params[1].name, "h");
    }
}
