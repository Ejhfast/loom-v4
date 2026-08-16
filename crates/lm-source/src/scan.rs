//! Scanner for the week-2 language slice.
//!
//! The scanner produces tokens with byte spans. It ends each statement
//! with a `Newline` token. It does not emit a `Newline` token inside
//! delimiters or after a token that cannot end an expression.
//!
//! A string literal can hold `{ expression }` interpolation. The
//! scanner scans the inner expression with one nested pass and stores
//! its tokens inside the string token. The inner expression cannot
//! hold a string literal or a brace in this slice.

use crate::diag::Diagnostic;
use crate::span::Span;
use crate::token::{StrPiece, Tok, Token};

/// Scan the full source text into tokens.
pub fn scan(text: &str) -> Result<Vec<Token>, Diagnostic> {
    let mut scanner = Scanner {
        text,
        bytes: text.as_bytes(),
        pos: 0,
        tokens: Vec::new(),
        nesting: Vec::new(),
    };
    scanner.run()?;
    Ok(scanner.tokens)
}

/// One open nesting context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Nest {
    /// An open `(`, `[`, or `{`. Newlines inside do not end a
    /// statement.
    Delim,
    /// An open statement block: `do`, `if`, `case`, `while`, or
    /// `loop`, closed by `end`. Newlines inside end statements, so a
    /// block body parses the same way at any delimiter depth.
    Block,
    /// An open brace closure `{ |x| ... }`. Its body is a statement
    /// block, so newlines inside end statements, and a right brace
    /// closes it.
    BraceBlock,
}

struct Scanner<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
    /// The open nesting contexts, innermost last.
    nesting: Vec<Nest>,
}

impl<'a> Scanner<'a> {
    fn run(&mut self) -> Result<(), Diagnostic> {
        // Skip one initial byte-order mark.
        if self.text.starts_with('\u{feff}') {
            self.pos = 3;
        }
        while self.pos < self.bytes.len() {
            let start = self.pos;
            let ch = self.cur_char();
            match ch {
                ' ' | '\t' | '\r' => {
                    self.pos += 1;
                }
                '#' => {
                    while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                }
                '\n' => {
                    self.pos += 1;
                    self.push_newline(start);
                }
                ';' => {
                    self.pos += 1;
                    self.push_newline(start);
                }
                '"' => self.scan_string(start)?,
                '\'' => {
                    return Err(self.error(
                        "E0008",
                        "character literals are not supported in this language slice",
                        start,
                    ));
                }
                '0'..='9' => self.scan_number(start)?,
                'a'..='z' | 'A'..='Z' | '_' => self.scan_word(start)?,
                _ => self.scan_punct(start)?,
            }
        }
        let end = self.text.len() as u32;
        self.tokens.push(Token {
            tok: Tok::Eof,
            span: Span::new(end, end),
        });
        Ok(())
    }

    fn cur_char(&self) -> char {
        self.text[self.pos..].chars().next().unwrap_or('\0')
    }

    fn peek_byte(&self, ahead: usize) -> u8 {
        *self.bytes.get(self.pos + ahead).unwrap_or(&0)
    }

    fn error(&self, code: &'static str, message: impl Into<String>, start: usize) -> Diagnostic {
        let hi = (self.pos.max(start + 1)).min(self.text.len());
        Diagnostic::new(code, message, Span::new(start as u32, hi as u32))
    }

    fn push(&mut self, tok: Tok, start: usize) {
        self.tokens.push(Token {
            tok,
            span: Span::new(start as u32, self.pos as u32),
        });
    }

    /// Push a statement terminator unless the position continues an expression.
    fn push_newline(&mut self, start: usize) {
        if self.nesting.last() == Some(&Nest::Delim) {
            return;
        }
        let continues = match self.tokens.last().map(|t| &t.tok) {
            None | Some(Tok::Newline) => true,
            Some(prev) => matches!(
                prev,
                Tok::Assign
                    | Tok::EqEq
                    | Tok::NotEq
                    | Tok::Lt
                    | Tok::Le
                    | Tok::Gt
                    | Tok::Ge
                    | Tok::Plus
                    | Tok::Minus
                    | Tok::Star
                    | Tok::Slash
                    | Tok::Percent
                    | Tok::Comma
                    | Tok::Colon
                    | Tok::Dot
                    | Tok::Arrow
                    | Tok::KwAnd
                    | Tok::KwOr
                    | Tok::KwNot
                    | Tok::LParen
            ),
        };
        if !continues {
            self.push(Tok::Newline, start);
        }
    }

    fn scan_string(&mut self, start: usize) -> Result<(), Diagnostic> {
        if self.text[self.pos..].starts_with("\"\"\"") {
            return Err(self.error(
                "E0010",
                "triple-quoted strings are not supported in this language slice",
                start,
            ));
        }
        self.pos += 1;
        let mut pieces: Vec<StrPiece> = Vec::new();
        let mut lit = String::new();
        loop {
            if self.pos >= self.bytes.len() {
                return Err(self.error("E0002", "unterminated string literal", start));
            }
            let ch = self.cur_char();
            match ch {
                '"' => {
                    self.pos += 1;
                    break;
                }
                '\n' => {
                    return Err(self.error("E0002", "unterminated string literal", start));
                }
                '{' => {
                    if self.peek_byte(1) == b'{' {
                        lit.push('{');
                        self.pos += 2;
                    } else {
                        if !lit.is_empty() {
                            pieces.push(StrPiece::Lit(std::mem::take(&mut lit)));
                        }
                        pieces.push(self.scan_interpolation()?);
                    }
                }
                '}' => {
                    if self.peek_byte(1) == b'}' {
                        lit.push('}');
                        self.pos += 2;
                    } else {
                        return Err(self.error(
                            "E0003",
                            "write `}}` for a literal `}` in a string",
                            self.pos,
                        ));
                    }
                }
                '\\' => {
                    let esc_start = self.pos;
                    self.pos += 1;
                    let esc = self.cur_char();
                    match esc {
                        '\\' => lit.push('\\'),
                        '"' => lit.push('"'),
                        '\'' => lit.push('\''),
                        'n' => lit.push('\n'),
                        'r' => lit.push('\r'),
                        't' => lit.push('\t'),
                        '0' => lit.push('\0'),
                        'u' => {
                            self.pos += 1;
                            let scalar = self.scan_unicode_escape(esc_start)?;
                            lit.push(scalar);
                            continue;
                        }
                        _ => {
                            self.pos += esc.len_utf8();
                            return Err(self.error("E0003", "invalid string escape", esc_start));
                        }
                    }
                    self.pos += 1;
                }
                _ => {
                    lit.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        if pieces.is_empty() {
            self.push(Tok::Str(lit), start);
        } else {
            if !lit.is_empty() {
                pieces.push(StrPiece::Lit(lit));
            }
            self.push(Tok::StrInterp(pieces), start);
        }
        Ok(())
    }

    /// Scan one `{ expression }` interpolation. `pos` is at `{`.
    fn scan_interpolation(&mut self) -> Result<StrPiece, Diagnostic> {
        let brace = self.pos;
        self.pos += 1;
        let expr_start = self.pos;
        loop {
            if self.pos >= self.bytes.len() || self.bytes[self.pos] == b'\n' {
                return Err(self.error(
                    "E0006",
                    "the interpolation expression has no closing `}`",
                    brace,
                ));
            }
            match self.bytes[self.pos] {
                b'}' => break,
                b'{' | b'"' => {
                    return Err(self.error(
                        "E0006",
                        "a brace or a string literal is not valid inside an \
                         interpolation expression in this language slice",
                        self.pos,
                    ));
                }
                _ => self.pos += 1,
            }
        }
        let inner = &self.text[expr_start..self.pos];
        self.pos += 1; // consume `}`
        if inner.trim().is_empty() {
            return Err(self.error("E0006", "the interpolation expression is empty", brace));
        }
        let offset = expr_start as u32;
        let mut tokens = scan(inner).map_err(|d| {
            Diagnostic::new(
                d.code,
                d.message,
                Span::new(d.span.lo + offset, d.span.hi + offset),
            )
        })?;
        for token in &mut tokens {
            token.span = Span::new(token.span.lo + offset, token.span.hi + offset);
        }
        Ok(StrPiece::Expr(tokens))
    }

    /// Scan the `{HEX}` part of a `\u{HEX}` escape. `pos` is at `{`.
    fn scan_unicode_escape(&mut self, esc_start: usize) -> Result<char, Diagnostic> {
        if self.peek_byte(0) != b'{' {
            return Err(self.error("E0003", "invalid string escape", esc_start));
        }
        self.pos += 1;
        let digits_start = self.pos;
        while self.peek_byte(0).is_ascii_hexdigit() {
            self.pos += 1;
        }
        if self.peek_byte(0) != b'}' || self.pos == digits_start {
            return Err(self.error("E0003", "invalid string escape", esc_start));
        }
        let digits = &self.text[digits_start..self.pos];
        self.pos += 1;
        let value = u32::from_str_radix(digits, 16)
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| self.error("E0003", "invalid string escape", esc_start))?;
        Ok(value)
    }

    fn scan_number(&mut self, start: usize) -> Result<(), Diagnostic> {
        let radix: u32;
        if self.peek_byte(0) == b'0'
            && matches!(self.peek_byte(1), b'x' | b'o' | b'b')
            && self.peek_byte(2).is_ascii_alphanumeric()
        {
            radix = match self.peek_byte(1) {
                b'x' => 16,
                b'o' => 8,
                _ => 2,
            };
            self.pos += 2;
        } else {
            radix = 10;
        }
        let digits_start = self.pos;
        let mut value: i64 = 0;
        let mut digit_count = 0u32;
        while self.pos < self.bytes.len() {
            let byte = self.bytes[self.pos];
            if byte == b'_' {
                self.pos += 1;
                continue;
            }
            let digit = match (byte as char).to_digit(radix) {
                Some(d) => d,
                None => break,
            };
            digit_count += 1;
            value = value
                .checked_mul(radix as i64)
                .and_then(|v| v.checked_add(digit as i64))
                .ok_or_else(|| {
                    self.error("E0004", "integer literal is too large for Int", start)
                })?;
            self.pos += 1;
        }
        if digit_count == 0 {
            self.pos = digits_start.max(self.pos);
            return Err(self.error("E0007", "invalid numeric literal", start));
        }
        // Reject float forms with a clear diagnostic.
        if radix == 10 {
            let next = self.peek_byte(0);
            let exponent = (next == b'e' || next == b'E')
                && (self.peek_byte(1).is_ascii_digit()
                    || (matches!(self.peek_byte(1), b'+' | b'-')
                        && self.peek_byte(2).is_ascii_digit()));
            let is_float = (next == b'.' && self.peek_byte(1).is_ascii_digit()) || exponent;
            if is_float {
                return Err(self.error(
                    "E0005",
                    "float literals are not supported in this language slice",
                    start,
                ));
            }
        }
        if self.peek_byte(0).is_ascii_alphanumeric() {
            return Err(self.error("E0007", "invalid numeric literal", start));
        }
        self.push(Tok::Int(value), start);
        Ok(())
    }

    fn scan_word(&mut self, start: usize) -> Result<(), Diagnostic> {
        while self.peek_byte(0).is_ascii_alphanumeric() || self.peek_byte(0) == b'_' {
            self.pos += 1;
        }
        let word = &self.text[start..self.pos];
        if word == "b" && self.peek_byte(0) == b'"' {
            return Err(self.error(
                "E0009",
                "byte-string literals are not supported in this language slice",
                start,
            ));
        }
        let tok = match word {
            "and" => Tok::KwAnd,
            "or" => Tok::KwOr,
            "not" => Tok::KwNot,
            "if" => Tok::KwIf,
            "elsif" => Tok::KwElsif,
            "else" => Tok::KwElse,
            "end" => Tok::KwEnd,
            "while" => Tok::KwWhile,
            "break" => Tok::KwBreak,
            "continue" => Tok::KwContinue,
            "def" => Tok::KwDef,
            "return" => Tok::KwReturn,
            "true" => Tok::KwTrue,
            "false" => Tok::KwFalse,
            "class" => Tok::KwClass,
            "do" => Tok::KwDo,
            "self" => Tok::KwSelf,
            "super" => Tok::KwSuper,
            "mut" => Tok::KwMut,
            "as" => Tok::KwAs,
            "case" => Tok::KwCase,
            "effect" => Tok::KwEffect,
            "enum" => Tok::KwEnum,
            "in" => Tok::KwIn,
            "is" => Tok::KwIs,
            "then" => Tok::KwThen,
            "with" => Tok::KwWith,
            "loop" => Tok::KwLoop,
            "use" => Tok::KwUse,
            _ => Tok::Ident(word.to_string()),
        };
        // Track statement blocks, so a block body inside `(`, `[`, or
        // `{` still ends its statements at newlines.
        match &tok {
            Tok::KwIf | Tok::KwCase | Tok::KwWhile | Tok::KwLoop => {
                self.nesting.push(Nest::Block);
            }
            Tok::KwDo => {
                // `loop do` opens one block, not two.
                if !matches!(self.tokens.last().map(|t| &t.tok), Some(Tok::KwLoop)) {
                    self.nesting.push(Nest::Block);
                }
            }
            Tok::KwEnd => {
                if self.nesting.last() == Some(&Nest::Block) {
                    self.nesting.pop();
                }
            }
            _ => {}
        }
        self.push(tok, start);
        Ok(())
    }

    /// Close one open delimiter. An unbalanced closer keeps the block
    /// contexts intact; the parser reports the mismatch.
    fn close_delim(&mut self) {
        if self.nesting.last() == Some(&Nest::Delim) {
            self.nesting.pop();
        }
    }

    /// Close one open brace: a brace closure or a map literal.
    fn close_brace(&mut self) {
        if matches!(
            self.nesting.last(),
            Some(Nest::Delim) | Some(Nest::BraceBlock)
        ) {
            self.nesting.pop();
        }
    }

    /// True when the left brace at the current position opens a brace
    /// closure. Specification appendix A.1: a left brace followed by a
    /// pipe starts a brace closure, and every other left brace starts
    /// a map literal.
    ///
    /// This is the one decision. The scanner reports it through
    /// `Tok::LBraceClosure`, so the parser never repeats the test and
    /// the two can never disagree. The lookahead passes blank space,
    /// line ends, and comments, so a closure may open on its own line.
    fn brace_opens_closure(&self) -> bool {
        let mut i = self.pos + 1;
        while i < self.bytes.len() {
            match self.bytes[i] {
                b' ' | b'\t' | b'\r' | b'\n' => i += 1,
                b'#' => {
                    while i < self.bytes.len() && self.bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                b'|' => return true,
                _ => return false,
            }
        }
        false
    }

    fn scan_punct(&mut self, start: usize) -> Result<(), Diagnostic> {
        let two = |a: u8, b: u8, s: &Scanner| s.peek_byte(0) == a && s.peek_byte(1) == b;
        let tok = if two(b'=', b'=', self) {
            self.pos += 2;
            Tok::EqEq
        } else if two(b'!', b'=', self) {
            self.pos += 2;
            Tok::NotEq
        } else if two(b'<', b'=', self) {
            self.pos += 2;
            Tok::Le
        } else if two(b'>', b'=', self) {
            self.pos += 2;
            Tok::Ge
        } else if two(b'-', b'>', self) {
            self.pos += 2;
            Tok::Arrow
        } else {
            let single = match self.peek_byte(0) {
                b'=' => Tok::Assign,
                b'<' => Tok::Lt,
                b'>' => Tok::Gt,
                b'+' => Tok::Plus,
                b'-' => Tok::Minus,
                b'*' => Tok::Star,
                b'/' => Tok::Slash,
                b'%' => Tok::Percent,
                b'(' => {
                    self.nesting.push(Nest::Delim);
                    Tok::LParen
                }
                b')' => {
                    self.close_delim();
                    Tok::RParen
                }
                b'[' => {
                    self.nesting.push(Nest::Delim);
                    Tok::LBracket
                }
                b']' => {
                    self.close_delim();
                    Tok::RBracket
                }
                b'{' => {
                    if self.brace_opens_closure() {
                        self.nesting.push(Nest::BraceBlock);
                        Tok::LBraceClosure
                    } else {
                        self.nesting.push(Nest::Delim);
                        Tok::LBrace
                    }
                }
                b'}' => {
                    self.close_brace();
                    Tok::RBrace
                }
                b',' => Tok::Comma,
                b':' => Tok::Colon,
                b'.' => Tok::Dot,
                b'|' => Tok::Pipe,
                _ => {
                    let ch = self.cur_char();
                    self.pos += ch.len_utf8();
                    return Err(self.error(
                        "E0001",
                        format!("invalid character `{ch}` in source"),
                        start,
                    ));
                }
            };
            self.pos += 1;
            single
        };
        self.push(tok, start);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<Tok> {
        scan(text).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn scans_number_forms() {
        assert_eq!(
            kinds("0 42 1_000_000 0xff 0o755 0b1010_0011"),
            vec![
                Tok::Int(0),
                Tok::Int(42),
                Tok::Int(1_000_000),
                Tok::Int(255),
                Tok::Int(493),
                Tok::Int(163),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn scans_string_escapes() {
        assert_eq!(
            kinds("\"a\\n{{b}}\\u{41}\""),
            vec![Tok::Str("a\n{b}A".to_string()), Tok::Eof]
        );
    }

    #[test]
    fn scans_interpolation_pieces() {
        let toks = kinds("\"Hello {name}!\"");
        assert_eq!(toks.len(), 2);
        match &toks[0] {
            Tok::StrInterp(pieces) => {
                assert_eq!(pieces.len(), 3);
                assert_eq!(pieces[0], StrPiece::Lit("Hello ".to_string()));
                match &pieces[1] {
                    StrPiece::Expr(inner) => {
                        assert_eq!(inner.len(), 2);
                        assert_eq!(inner[0].tok, Tok::Ident("name".to_string()));
                        assert_eq!(inner[1].tok, Tok::Eof);
                        // Spans point into the outer source text.
                        assert_eq!(inner[0].span.lo, 8);
                        assert_eq!(inner[0].span.hi, 12);
                    }
                    other => panic!("expected an expression piece, got {other:?}"),
                }
                assert_eq!(pieces[2], StrPiece::Lit("!".to_string()));
            }
            other => panic!("expected an interpolated string, got {other:?}"),
        }
    }

    #[test]
    fn rejects_bad_interpolation() {
        assert_eq!(scan("\"x {\"").unwrap_err().code, "E0006");
        assert_eq!(scan("\"x { }\"").unwrap_err().code, "E0006");
        assert_eq!(scan("\"x {a{b}\"").unwrap_err().code, "E0006");
        assert_eq!(scan("\"x {\"y\"}\"").unwrap_err().code, "E0006");
    }

    #[test]
    fn scans_arrow_and_new_keywords() {
        assert_eq!(
            kinds("do |x: Int| -> mut self super class end"),
            vec![
                Tok::KwDo,
                Tok::Pipe,
                Tok::Ident("x".to_string()),
                Tok::Colon,
                Tok::Ident("Int".to_string()),
                Tok::Pipe,
                Tok::Arrow,
                Tok::KwMut,
                Tok::KwSelf,
                Tok::KwSuper,
                Tok::KwClass,
                Tok::KwEnd,
                Tok::Eof
            ]
        );
    }

    #[test]
    fn rejects_float_literal() {
        assert_eq!(scan("1.5").unwrap_err().code, "E0005");
        assert_eq!(scan("1e9").unwrap_err().code, "E0005");
    }

    #[test]
    fn rejects_overflowing_literal() {
        assert_eq!(scan("9223372036854775808").unwrap_err().code, "E0004");
    }

    #[test]
    fn rejects_invalid_number_suffix() {
        assert_eq!(scan("12ab").unwrap_err().code, "E0007");
        assert_eq!(scan("0x").unwrap_err().code, "E0007");
    }

    #[test]
    fn rejects_unterminated_string() {
        assert_eq!(scan("\"abc").unwrap_err().code, "E0002");
        assert_eq!(scan("\"abc\ndef\"").unwrap_err().code, "E0002");
    }

    #[test]
    fn rejects_char_and_byte_literals() {
        assert_eq!(scan("'a'").unwrap_err().code, "E0008");
        assert_eq!(scan("b\"x\"").unwrap_err().code, "E0009");
        assert_eq!(scan("\"\"\"x\"\"\"").unwrap_err().code, "E0010");
    }

    #[test]
    fn suppresses_newline_after_operator_and_inside_parens() {
        assert_eq!(
            kinds("1 +\n2"),
            vec![Tok::Int(1), Tok::Plus, Tok::Int(2), Tok::Eof]
        );
        assert_eq!(
            kinds("f(1,\n2)"),
            vec![
                Tok::Ident("f".to_string()),
                Tok::LParen,
                Tok::Int(1),
                Tok::Comma,
                Tok::Int(2),
                Tok::RParen,
                Tok::Eof
            ]
        );
    }

    #[test]
    fn emits_newline_between_statements() {
        assert_eq!(
            kinds("x = 1\nx"),
            vec![
                Tok::Ident("x".to_string()),
                Tok::Assign,
                Tok::Int(1),
                Tok::Newline,
                Tok::Ident("x".to_string()),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn skips_comments() {
        assert_eq!(kinds("# a comment\n42"), vec![Tok::Int(42), Tok::Eof]);
    }

    #[test]
    fn semicolon_ends_statement() {
        assert_eq!(
            kinds("1; 2"),
            vec![Tok::Int(1), Tok::Newline, Tok::Int(2), Tok::Eof]
        );
    }
}
