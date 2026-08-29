//! Lossless public syntax records.

use crate::ast::Module;
use crate::diag::Diagnostic;
use crate::parse::parse_tokens;
use crate::scan::scan;
use crate::span::Span;
use crate::token::{Tok, Token};
use lm_abi::syntax::*;

/// The parser status of one source unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseStatus {
    Complete,
    Incomplete,
    Invalid,
}

/// One lossless public parse result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicSyntax {
    pub records: Vec<u8>,
    pub status: ParseStatus,
    pub diagnostics: Vec<Diagnostic>,
}

/// Parse one complete module and build its public syntax in one scan.
pub fn parse_complete(text: &str) -> Result<(Module, PublicSyntax), Diagnostic> {
    let tokens = scan(text)?;
    let module = parse_tokens(text, &tokens)?;
    let syntax = build_valid(text, &tokens, &module);
    Ok((module, syntax))
}

/// Parse source into the portable public syntax format.
pub fn parse_public_syntax(text: &str) -> PublicSyntax {
    match scan(text) {
        Ok(tokens) => match parse_tokens(text, &tokens) {
            Ok(module) => build_valid(text, &tokens, &module),
            Err(diagnostic) => {
                let status = if diagnostic.span.lo as usize >= text.len() {
                    ParseStatus::Incomplete
                } else {
                    ParseStatus::Invalid
                };
                build_invalid(text, &tokens, status, diagnostic)
            }
        },
        Err(diagnostic) => {
            let status = if diagnostic.span.hi as usize >= text.len()
                && matches!(diagnostic.code, "E0002" | "E0006")
            {
                ParseStatus::Incomplete
            } else {
                ParseStatus::Invalid
            };
            build_unscanned(text, status, diagnostic)
        }
    }
}

#[derive(Clone, Copy)]
struct TopItem {
    span: Span,
    kind: u16,
}

fn build_valid(text: &str, tokens: &[Token], module: &Module) -> PublicSyntax {
    let mut top = Vec::new();
    top.extend(module.uses.iter().map(|item| TopItem {
        span: item.span,
        kind: KIND_USE,
    }));
    top.extend(module.interfaces.iter().map(|item| TopItem {
        span: item.span,
        kind: KIND_INTERFACE,
    }));
    top.extend(module.classes.iter().map(|item| TopItem {
        span: item.span,
        kind: KIND_CLASS,
    }));
    top.extend(module.enums.iter().map(|item| TopItem {
        span: item.span,
        kind: KIND_ENUM,
    }));
    top.extend(module.constants.iter().map(|item| TopItem {
        span: item.span,
        kind: KIND_CONST,
    }));
    top.extend(module.funcs.iter().map(|item| TopItem {
        span: item.span,
        kind: KIND_FUNCTION,
    }));
    top.extend(module.entry.iter().map(|item| TopItem {
        span: item.span,
        kind: KIND_STATEMENT,
    }));
    top.sort_by_key(|item| (item.span.lo, item.span.hi));

    let mut builder = Builder::new(text, tokens);
    let mut root_children = Vec::new();
    let mut cursor = 0u32;
    for item in top {
        if item.span.lo > cursor {
            builder.add_lexical_range(Span::new(cursor, item.span.lo), &mut root_children);
        }
        let index = builder.add_node(item.kind, item.span, SyntaxClass::Node);
        root_children.push(index);
        cursor = cursor.max(item.span.hi);
    }
    if cursor < text.len() as u32 {
        builder.add_lexical_range(Span::new(cursor, text.len() as u32), &mut root_children);
    }
    let root = builder.finish_root(root_children);
    let records = encode_syntax(&builder.records, &builder.children, root)
        .expect("the public parser creates valid syntax records");
    PublicSyntax {
        records,
        status: ParseStatus::Complete,
        diagnostics: Vec::new(),
    }
}

fn build_invalid(
    text: &str,
    tokens: &[Token],
    status: ParseStatus,
    diagnostic: Diagnostic,
) -> PublicSyntax {
    let mut builder = Builder::new(text, tokens);
    let invalid = builder.add_node(
        KIND_INVALID,
        Span::new(0, text.len() as u32),
        SyntaxClass::Invalid,
    );
    let root = builder.finish_root(vec![invalid]);
    let records = encode_syntax(&builder.records, &builder.children, root)
        .expect("the public parser creates valid invalid records");
    PublicSyntax {
        records,
        status,
        diagnostics: vec![diagnostic],
    }
}

fn build_unscanned(text: &str, status: ParseStatus, diagnostic: Diagnostic) -> PublicSyntax {
    let mut builder = Builder::new(text, &[]);
    let invalid = builder.add_leaf(
        SyntaxClass::Invalid,
        KIND_INVALID,
        Span::new(0, text.len() as u32),
    );
    let root = builder.finish_root(vec![invalid]);
    let records = encode_syntax(&builder.records, &builder.children, root)
        .expect("the public parser creates valid unscanned records");
    PublicSyntax {
        records,
        status,
        diagnostics: vec![diagnostic],
    }
}

struct Builder<'a> {
    text: &'a str,
    tokens: &'a [Token],
    records: Vec<SyntaxRecord>,
    children: Vec<u32>,
    token_cursor: usize,
}

impl<'a> Builder<'a> {
    fn new(text: &'a str, tokens: &'a [Token]) -> Builder<'a> {
        Builder {
            text,
            tokens,
            records: Vec::new(),
            children: Vec::new(),
            token_cursor: 0,
        }
    }

    fn add_node(&mut self, kind: u16, span: Span, class: SyntaxClass) -> u32 {
        let mut node_children = Vec::new();
        self.add_lexical_range(span, &mut node_children);
        let child_start = self.children.len() as u32;
        let child_len = node_children.len() as u32;
        self.children.extend(node_children);
        let index = self.records.len() as u32;
        self.records.push(SyntaxRecord {
            class,
            kind,
            lo: span.lo,
            hi: span.hi,
            child_start,
            child_len,
        });
        index
    }

    fn finish_root(&mut self, root_children: Vec<u32>) -> u32 {
        let child_start = self.children.len() as u32;
        let child_len = root_children.len() as u32;
        self.children.extend(root_children);
        let root = self.records.len() as u32;
        self.records.push(SyntaxRecord {
            class: SyntaxClass::Node,
            kind: KIND_MODULE,
            lo: 0,
            hi: self.text.len() as u32,
            child_start,
            child_len,
        });
        root
    }

    fn add_lexical_range(&mut self, span: Span, output: &mut Vec<u32>) {
        let mut cursor = span.lo;
        while self.token_cursor < self.tokens.len()
            && self.tokens[self.token_cursor].span.hi <= span.lo
        {
            self.token_cursor += 1;
        }
        while let Some(token) = self.tokens.get(self.token_cursor) {
            if matches!(token.tok, Tok::Eof) || token.span.lo >= span.hi {
                break;
            }
            if token.span.lo < span.lo || token.span.hi > span.hi {
                self.token_cursor += 1;
                continue;
            }
            if token.span.lo > cursor {
                self.add_trivia(Span::new(cursor, token.span.lo), output);
            }
            output.push(self.add_leaf(SyntaxClass::Token, token_kind(&token.tok), token.span));
            cursor = cursor.max(token.span.hi);
            self.token_cursor += 1;
        }
        if cursor < span.hi {
            self.add_trivia(Span::new(cursor, span.hi), output);
        }
    }

    fn add_trivia(&mut self, span: Span, output: &mut Vec<u32>) {
        let bytes = self.text.as_bytes();
        let mut at = span.lo as usize;
        let end = span.hi as usize;
        while at < end {
            let start = at;
            let kind = if at == 0 && bytes.get(..3) == Some(&[0xef, 0xbb, 0xbf]) {
                at += 3;
                KIND_BOM
            } else if bytes[at] == b'#' {
                at += 1;
                while at < end && bytes[at] != b'\n' {
                    at += 1;
                }
                KIND_COMMENT
            } else if bytes[at].is_ascii_whitespace() {
                at += 1;
                while at < end && bytes[at].is_ascii_whitespace() {
                    at += 1;
                }
                KIND_WHITESPACE
            } else {
                at += 1;
                while at < end && bytes[at] != b'#' && !bytes[at].is_ascii_whitespace() {
                    at += 1;
                }
                KIND_INVALID
            };
            let class = if kind == KIND_INVALID {
                SyntaxClass::Invalid
            } else {
                SyntaxClass::Trivia
            };
            let index = self.add_leaf(class, kind, Span::new(start as u32, at as u32));
            output.push(index);
        }
    }

    fn add_leaf(&mut self, class: SyntaxClass, kind: u16, span: Span) -> u32 {
        let index = self.records.len() as u32;
        self.records.push(SyntaxRecord {
            class,
            kind,
            lo: span.lo,
            hi: span.hi,
            child_start: 0,
            child_len: 0,
        });
        index
    }
}

fn token_kind(token: &Tok) -> u16 {
    match token {
        Tok::Int(_) => KIND_INT,
        Tok::Float(_) => KIND_FLOAT,
        Tok::Char(_) => KIND_CHAR,
        Tok::Str(_) | Tok::StrInterp(_) => KIND_STRING,
        Tok::Bytes(_) => KIND_BYTES,
        Tok::Ident(_) => KIND_IDENTIFIER,
        Tok::KwAnd
        | Tok::KwOr
        | Tok::KwNot
        | Tok::KwIf
        | Tok::KwElsif
        | Tok::KwElse
        | Tok::KwEnd
        | Tok::KwWhile
        | Tok::KwBreak
        | Tok::KwContinue
        | Tok::KwDef
        | Tok::KwReturn
        | Tok::KwTrue
        | Tok::KwFalse
        | Tok::KwFinal
        | Tok::KwFrozen
        | Tok::KwClass
        | Tok::KwDo
        | Tok::KwSelf
        | Tok::KwSuper
        | Tok::KwMut
        | Tok::KwEscaping
        | Tok::KwEnum
        | Tok::KwCase
        | Tok::KwSelect
        | Tok::KwIn
        | Tok::KwThen
        | Tok::KwWith
        | Tok::KwEffect
        | Tok::KwIs
        | Tok::KwAs
        | Tok::KwLoop
        | Tok::KwUse
        | Tok::KwInterface
        | Tok::KwImplements
        | Tok::KwWhen
        | Tok::KwType
        | Tok::KwFor
        | Tok::KwConst
        | Tok::KwReserved(_) => KIND_KEYWORD,
        Tok::LParen => KIND_LPAREN,
        Tok::RParen => KIND_RPAREN,
        Tok::LBracket => KIND_LBRACKET,
        Tok::RBracket => KIND_RBRACKET,
        Tok::LBrace | Tok::LBraceClosure => KIND_LBRACE,
        Tok::RBrace => KIND_RBRACE,
        Tok::Comma => KIND_COMMA,
        Tok::Colon => KIND_COLON,
        Tok::Dot => KIND_DOT,
        Tok::Pipe => KIND_PIPE,
        Tok::Arrow => KIND_ARROW,
        Tok::Question => KIND_QUESTION,
        Tok::Assign => KIND_ASSIGN,
        Tok::EqEq => KIND_EQ,
        Tok::NotEq => KIND_NE,
        Tok::Lt => KIND_LT,
        Tok::Le => KIND_LE,
        Tok::Gt => KIND_GT,
        Tok::Ge => KIND_GE,
        Tok::Plus => KIND_PLUS,
        Tok::Minus => KIND_MINUS,
        Tok::Star => KIND_STAR,
        Tok::Slash => KIND_SLASH,
        Tok::Percent => KIND_PERCENT,
        Tok::Amp => KIND_AMP,
        Tok::Caret => KIND_CARET,
        Tok::Shl => KIND_SHL,
        Tok::Shr => KIND_SHR,
        Tok::Ushr => KIND_USHR,
        Tok::Tilde => KIND_TILDE,
        Tok::Newline => KIND_NEWLINE,
        Tok::Eof => unreachable!("the syntax builder skips the end marker"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn concatenated_children(text: &str, syntax: &PublicSyntax) -> String {
        let view = SyntaxView::new(&syntax.records, text.len()).expect("valid syntax records");
        let root = view.record(view.root()).expect("the root exists");
        let mut out = String::new();
        for offset in 0..root.child_len {
            let index = view.child(root, offset).expect("the child exists");
            let record = view.record(index).expect("the child is valid");
            out.push_str(&text[record.lo as usize..record.hi as usize]);
        }
        out
    }

    #[test]
    fn preserves_complete_source_in_root_order() {
        let text = "\u{feff}# head\nfinal class Box\n  value: Int\nend\n\nBox(3)\n";
        let syntax = parse_public_syntax(text);
        assert_eq!(syntax.status, ParseStatus::Complete);
        assert_eq!(concatenated_children(text, &syntax), text);
    }

    #[test]
    fn combined_parse_produces_the_public_syntax() {
        let text = "def value(): Int\n  3\nend\nvalue()\n";
        let (module, syntax) = parse_complete(text).expect("the source parses");
        assert_eq!(
            module,
            crate::parse::parse(text).expect("the source parses")
        );
        assert_eq!(syntax, parse_public_syntax(text));
    }

    #[test]
    fn reports_general_parse_status() {
        assert_eq!(parse_public_syntax("1 + 2\n").status, ParseStatus::Complete);
        assert_eq!(
            parse_public_syntax("def value(): Int\n  3\nend\n").status,
            ParseStatus::Complete
        );
        assert_eq!(
            parse_public_syntax("def value(): Int\n").status,
            ParseStatus::Incomplete
        );
        assert_eq!(parse_public_syntax("1 + )\n").status, ParseStatus::Invalid);
    }

    #[test]
    fn accepts_mixed_modules_without_repl_policy() {
        let syntax = parse_public_syntax("def value(): Int\n  3\nend\nvalue()\n");
        assert_eq!(syntax.status, ParseStatus::Complete);
        assert!(syntax.diagnostics.is_empty());
    }

    #[test]
    fn keeps_recovered_tokens_inside_invalid_fragments() {
        let text = "def value(";
        let syntax = parse_public_syntax(text);
        let view = SyntaxView::new(&syntax.records, text.len()).expect("valid syntax records");
        let root = view.record(view.root()).expect("the root exists");
        let invalid = view
            .record(view.child(root, 0).expect("the invalid fragment exists"))
            .expect("the invalid fragment is valid");
        assert_eq!(invalid.class, SyntaxClass::Invalid);
        assert!(invalid.child_len > 0);
        assert_eq!(concatenated_children(text, &syntax), text);
    }
}
