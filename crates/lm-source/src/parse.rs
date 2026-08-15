//! Recursive-descent parser for the week-1 language slice.
//!
//! The parser rejects all constructs outside the slice with a
//! precise diagnostic. It never accepts a silent fallback form.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::scan::scan;
use crate::span::Span;
use crate::token::{Tok, Token};

/// The maximum nesting depth for expressions and statements.
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
        let mut funcs = Vec::new();
        let mut entry = Vec::new();
        loop {
            self.skip_newlines();
            match self.peek() {
                Tok::Eof => break,
                Tok::KwDef => funcs.push(self.func_def()?),
                _ => {
                    let stmt = self.stmt()?;
                    entry.push(stmt);
                    self.expect_terminator()?;
                }
            }
        }
        Ok(Module { funcs, entry })
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

    fn func_def(&mut self) -> Result<FuncDef, Diagnostic> {
        let def_tok = self.expect(Tok::KwDef, "`def`")?;
        let (name, name_span) = self.ident("a function name")?;
        self.expect(Tok::LParen, "`(`")?;
        let mut params = Vec::new();
        if !matches!(self.peek(), Tok::RParen) {
            loop {
                let (pname, pspan) = self.ident("a parameter name")?;
                self.expect(Tok::Colon, "`:` and a parameter type")?;
                let ty = self.type_expr()?;
                let span = pspan.to(ty.span);
                params.push(Param {
                    name: pname,
                    ty,
                    span,
                });
                if matches!(self.peek(), Tok::Comma) {
                    self.pos += 1;
                } else {
                    break;
                }
            }
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
        Ok(FuncDef {
            name,
            name_span,
            params,
            ret,
            body,
            span: def_tok.span.to(end_tok.span),
        })
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
        match self.peek() {
            Tok::Ident(_) => {
                let token = self.next();
                match token.tok {
                    Tok::Ident(name) => Ok(TypeExpr {
                        kind: TypeExprKind::Name(name),
                        span: token.span,
                    }),
                    _ => unreachable!(),
                }
            }
            Tok::LParen => {
                let open = self.next();
                let close = self.expect(Tok::RParen, "`)` to complete the unit type `()`")?;
                Ok(TypeExpr {
                    kind: TypeExprKind::Unit,
                    span: open.span.to(close.span),
                })
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
                "a `def` function is only valid at the top level of a module",
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
            Tok::Ident(_) if matches!(self.peek_at(1), Tok::Assign) => {
                let (name, name_span) = self.ident("a name")?;
                self.pos += 1; // consume `=`
                let value = self.expr()?;
                let span = name_span.to(value.span);
                Ok(Stmt {
                    kind: StmtKind::Assign {
                        name,
                        name_span,
                        ty: None,
                        value,
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
                    let (name, name_span) = match &expr.kind {
                        ExprKind::Name(name) => (name.clone(), expr.span),
                        _ => {
                            return Err(self.error(
                                "E1002",
                                "only a direct call of a named function is supported \
                                 in this language slice",
                            ));
                        }
                    };
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
                    let span = name_span.to(close.span);
                    expr = Expr {
                        kind: ExprKind::Call {
                            name,
                            name_span,
                            args,
                        },
                        span,
                    };
                }
                Tok::Dot => {
                    return Err(self.error(
                        "E1002",
                        "field and method access is not supported in this language slice",
                    ));
                }
                Tok::LBracket => {
                    return Err(
                        self.error("E1002", "indexing is not supported in this language slice")
                    );
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
            Tok::KwIf => self.if_expr(),
            Tok::LBracket => Err(self.error(
                "E1002",
                "list literals are not supported in this language slice",
            )),
            Tok::LBrace => Err(self.error(
                "E1002",
                "map literals are not supported in this language slice",
            )),
            other => Err(self.error("E1001", format!("expected an expression, found {other}"))),
        }
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
    fn parses_while_with_break_and_continue() {
        let source = "x = 0\nwhile x < 10\n  x = x + 1\n  if x == 5\n    break\n  end\n  \
                      continue\nend\nx\n";
        let module = parse(source).unwrap();
        assert_eq!(module.entry.len(), 3);
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
        let err = parse("class Foo\nend\n").unwrap_err();
        assert_eq!(err.code, "E1002");
        assert!(err.message.contains("`class`"));
    }

    #[test]
    fn rejects_tuple_literal() {
        assert_eq!(parse("(1, 2)\n").unwrap_err().code, "E1002");
    }

    #[test]
    fn rejects_list_literal() {
        assert_eq!(parse("[1, 2]\n").unwrap_err().code, "E1002");
    }

    #[test]
    fn rejects_method_call() {
        assert_eq!(parse("x.len()\n").unwrap_err().code, "E1002");
    }

    #[test]
    fn rejects_indirect_call() {
        assert_eq!(parse("(1)(2)\n").unwrap_err().code, "E1002");
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
    fn parses_bare_and_valued_return() {
        let source = "def f(): Int\n  return 1\nend\ndef g()\n  return\nend\n1\n";
        let module = parse(source).unwrap();
        assert_eq!(module.funcs.len(), 2);
    }
}
