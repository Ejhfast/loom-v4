//! Recursive-descent parser for the week-2 language slice.
//!
//! The parser rejects all constructs outside the slice with a
//! precise diagnostic. It never accepts a silent fallback form.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::scan::scan;
use crate::span::Span;
use crate::token::{StrPiece, Tok, Token};

/// The maximum nesting depth for expressions, statements, and types.
///
/// The parser and the checker recurse on the Rust stack. This limit
/// keeps deep input inside the available stack and rejects deeper
/// input with a diagnostic.
pub const MAX_NEST_DEPTH: usize = 300;

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
        let mut classes = Vec::new();
        let mut funcs = Vec::new();
        let mut entry = Vec::new();
        loop {
            self.skip_newlines();
            match self.peek() {
                Tok::Eof => break,
                Tok::KwClass => classes.push(self.class_def()?),
                Tok::KwDef => funcs.push(self.func_def()?),
                _ => {
                    let stmt = self.stmt()?;
                    entry.push(stmt);
                    self.expect_terminator()?;
                }
            }
        }
        Ok(Module {
            classes,
            funcs,
            entry,
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
            Tok::Eof | Tok::KwEnd | Tok::KwElse | Tok::KwElsif => Ok(()),
            other => Err(self.error(
                "E1001",
                format!("expected the end of the statement, found {other}"),
            )),
        }
    }

    fn class_def(&mut self) -> Result<ClassDef, Diagnostic> {
        let class_tok = self.expect(Tok::KwClass, "`class`")?;
        let (name, name_span) = self.ident("a class name")?;
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
            parent,
            fields,
            methods,
            span: class_tok.span.to(end_tok.span),
        })
    }

    fn method_def(&mut self) -> Result<MethodDef, Diagnostic> {
        let def_tok = self.expect(Tok::KwDef, "`def`")?;
        let (name, name_span) = self.ident("a method name")?;
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
        let body = self.block(&[Tok::KwEnd])?;
        let end_tok = self.expect(Tok::KwEnd, "`end`")?;
        self.expect_terminator()?;
        Ok(MethodDef {
            name,
            name_span,
            mut_self,
            params,
            ret,
            body,
            span: def_tok.span.to(end_tok.span),
        })
    }

    fn func_def(&mut self) -> Result<FuncDef, Diagnostic> {
        let def_tok = self.expect(Tok::KwDef, "`def`")?;
        let (name, name_span) = self.ident("a function name")?;
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
        let body = self.block(&[Tok::KwEnd])?;
        let end_tok = self.expect(Tok::KwEnd, "`end`")?;
        self.expect_terminator()?;
        Ok(FuncDef {
            name,
            name_span,
            params,
            ret,
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
                let (name, span) = self.ident("a type")?;
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
                if !matches!(self.peek(), Tok::RParen) {
                    loop {
                        params.push(self.type_expr()?);
                        if matches!(self.peek(), Tok::Comma) {
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                }
                let close = self.expect(Tok::RParen, "`)`")?;
                if matches!(self.peek(), Tok::Arrow) {
                    self.pos += 1;
                    let ret = self.type_expr()?;
                    let span = open.span.to(ret.span);
                    Ok(TypeExpr {
                        kind: TypeExprKind::Fn(params, Box::new(ret)),
                        span,
                    })
                } else if params.is_empty() {
                    Ok(TypeExpr {
                        kind: TypeExprKind::Unit,
                        span: open.span.to(close.span),
                    })
                } else {
                    Err(self.error("E1003", "expected `->` to complete the function type"))
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
            Tok::KwReturn => {
                let ret_tok = self.next();
                let value = match self.peek() {
                    Tok::Newline | Tok::Eof | Tok::KwEnd | Tok::KwElse | Tok::KwElsif => None,
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
                            "index assignment is not supported in this language slice; \
                             for a map, use the `put` method; a list has no element \
                             write in this slice",
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

    fn eq_expr(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.ord_expr()?;
        loop {
            let op = match self.peek() {
                Tok::EqEq => BinOp::Eq,
                Tok::NotEq => BinOp::Ne,
                _ => break,
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

    fn postfix_expr(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.primary_expr()?;
        loop {
            match self.peek() {
                Tok::LParen => {
                    self.pos += 1; // consume `(`
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
                    let span = expr.span.to(close.span);
                    expr = match expr.kind {
                        ExprKind::Name(name) => Expr {
                            kind: ExprKind::Call {
                                name,
                                name_span: expr.span,
                                args,
                            },
                            span,
                        },
                        ExprKind::Field {
                            recv,
                            name,
                            name_span,
                        } => Expr {
                            kind: ExprKind::MethodCall {
                                recv,
                                name,
                                name_span,
                                args,
                            },
                            span,
                        },
                        _ => Expr {
                            kind: ExprKind::CallExpr {
                                callee: Box::new(expr),
                                args,
                            },
                            span,
                        },
                    };
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
                Tok::LBracket => {
                    self.pos += 1;
                    let index = self.expr()?;
                    let close = self.expect(Tok::RBracket, "`]` to complete the index")?;
                    let span = expr.span.to(close.span);
                    expr = Expr {
                        kind: ExprKind::Index {
                            recv: Box::new(expr),
                            index: Box::new(index),
                        },
                        span,
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
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
                self.pos += 1;
                let inner = self.expr()?;
                if matches!(self.peek(), Tok::Comma) {
                    return Err(
                        self.error("E1002", "tuples are not supported in this language slice")
                    );
                }
                self.expect(Tok::RParen, "`)`")?;
                Ok(inner)
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
        let body = self.block(&[Tok::KwEnd])?;
        let end_tok = self.expect(Tok::KwEnd, "`end`")?;
        Ok(Expr {
            kind: ExprKind::Closure { params, ret, body },
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
    fn rejects_reserved_keyword() {
        let err = parse("enum Color\nend\n").unwrap_err();
        assert_eq!(err.code, "E1002");
        assert!(err.message.contains("`enum`"));
    }

    #[test]
    fn rejects_tuple_literal() {
        assert_eq!(parse("(1, 2)\n").unwrap_err().code, "E1002");
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
}
