//! Scanner for Loom source code.
//!
//! The scanner produces tokens with byte spans. It separates body expressions
//! with a `Newline` token. It does not emit a `Newline` token inside
//! delimiters or after a token that cannot end an expression.
//!
//! A string literal can hold `#{ expression }` interpolation. The
//! scanner scans the inner expression with one nested pass and stores
//! its tokens inside the string token. The nested scanner handles
//! strings and balanced braces in the expression.

use crate::diag::Diagnostic;
use crate::span::Span;
use crate::token::{StrPiece, Tok, Token};

/// The largest nested interpolation depth.
const MAX_INTERPOLATION_DEPTH: usize = 64;

/// Scan the full source text into tokens.
pub fn scan(text: &str) -> Result<Vec<Token>, Diagnostic> {
    let mut scanner = Scanner {
        text,
        bytes: text.as_bytes(),
        pos: 0,
        tokens: Vec::new(),
        nesting: Vec::new(),
        interpolation_depth: 0,
    };
    scanner.run()?;
    Ok(scanner.tokens)
}

/// One open nesting context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Nest {
    /// An open `(`, `[`, or `{`. Newlines inside do not end a
    /// expression.
    Delim,
    /// An open expression block closed by `end`.
    /// Newlines inside the block separate expressions.
    Block,
    /// A loop header that waits for `do` or a newline.
    LoopBlock,
    /// An open brace closure `{ |x| ... }`. Its body is an expression
    /// block, so newlines separate expressions, and a right brace
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
    /// The number of interpolation scanners above this scanner.
    interpolation_depth: usize,
}

impl<'a> Scanner<'a> {
    fn run(&mut self) -> Result<(), Diagnostic> {
        // Skip one initial byte-order mark.
        if self.text.starts_with('\u{feff}') {
            self.pos = 3;
        }
        while self.pos < self.bytes.len() {
            self.scan_one()?;
        }
        let end = self.text.len() as u32;
        self.tokens.push(Token {
            tok: Tok::Eof,
            span: Span::new(end, end),
        });
        Ok(())
    }

    /// Scan one token or one item of ignored space.
    fn scan_one(&mut self) -> Result<(), Diagnostic> {
        let start = self.pos;
        let ch = self.cur_char();
        match ch {
            ' ' | '\t' | '\r' => self.pos += 1,
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
            '\'' => self.scan_char(start)?,
            '0'..='9' => self.scan_number(start)?,
            'a'..='z' | 'A'..='Z' | '_' => self.scan_word(start)?,
            _ => self.scan_punct(start)?,
        }
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

    /// Push an expression separator unless the expression continues.
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
                    | Tok::Amp
                    | Tok::Pipe
                    | Tok::Caret
                    | Tok::Shl
                    | Tok::Shr
                    | Tok::Ushr
                    | Tok::Tilde
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
            if self.nesting.last() == Some(&Nest::LoopBlock) {
                *self.nesting.last_mut().expect("the loop block exists") = Nest::Block;
            }
        }
    }

    /// True when `do` is followed by a body separator.
    fn do_has_body_separator(&self) -> bool {
        let mut pos = self.pos;
        while matches!(self.bytes.get(pos), Some(b' ' | b'\t' | b'\r')) {
            pos += 1;
        }
        matches!(self.bytes.get(pos), None | Some(b';' | b'\n' | b'#'))
    }

    fn scan_string(&mut self, start: usize) -> Result<(), Diagnostic> {
        if self.text[self.pos..].starts_with("\"\"\"") {
            return Err(self.error(
                "E0010",
                "Loom does not support triple-quoted strings",
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
                '#' if self.peek_byte(1) == b'{' => {
                    if !lit.is_empty() {
                        pieces.push(StrPiece::Lit(std::mem::take(&mut lit)));
                    }
                    self.pos += 1;
                    pieces.push(self.scan_interpolation()?);
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
                        '#' if self.peek_byte(1) == b'{' => lit.push('#'),
                        'x' => {
                            self.pos += 1;
                            let byte = self.scan_hex_byte(esc_start)?;
                            if byte > 0x7f {
                                return Err(self.error(
                                    "E0003",
                                    "a string byte escape must be in the ASCII range",
                                    esc_start,
                                ));
                            }
                            lit.push(char::from(byte));
                            continue;
                        }
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

    /// Scan one Unicode scalar character literal.
    fn scan_char(&mut self, start: usize) -> Result<(), Diagnostic> {
        self.pos += 1;
        if self.pos >= self.bytes.len() || matches!(self.cur_char(), '\n' | '\r') {
            return Err(self.error("E0008", "unterminated character literal", start));
        }
        if self.cur_char() == '\'' {
            return Err(self.error("E0008", "a character literal cannot be empty", start));
        }
        let value = if self.cur_char() == '\\' {
            let esc_start = self.pos;
            self.pos += 1;
            let escape = self.cur_char();
            match escape {
                '\\' => {
                    self.pos += 1;
                    '\\'
                }
                '"' => {
                    self.pos += 1;
                    '"'
                }
                '\'' => {
                    self.pos += 1;
                    '\''
                }
                'n' => {
                    self.pos += 1;
                    '\n'
                }
                'r' => {
                    self.pos += 1;
                    '\r'
                }
                't' => {
                    self.pos += 1;
                    '\t'
                }
                '0' => {
                    self.pos += 1;
                    '\0'
                }
                'x' => {
                    self.pos += 1;
                    let byte = self
                        .scan_hex_byte(esc_start)
                        .map_err(|_| self.error("E0008", "invalid character escape", esc_start))?;
                    if byte > 0x7f {
                        return Err(self.error(
                            "E0008",
                            "a character byte escape must be in the ASCII range",
                            esc_start,
                        ));
                    }
                    char::from(byte)
                }
                'u' => {
                    self.pos += 1;
                    self.scan_unicode_escape(esc_start)
                        .map_err(|_| self.error("E0008", "invalid character escape", esc_start))?
                }
                _ => {
                    self.pos += escape.len_utf8();
                    return Err(self.error("E0008", "invalid character escape", esc_start));
                }
            }
        } else {
            let value = self.cur_char();
            self.pos += value.len_utf8();
            value
        };
        if self.cur_char() != '\'' {
            return Err(self.error(
                "E0008",
                "a character literal must contain one Unicode scalar value",
                start,
            ));
        }
        self.pos += 1;
        self.push(Tok::Char(value), start);
        Ok(())
    }

    /// Scan one immutable byte string. `pos` is at the opening quote.
    fn scan_bytes(&mut self, start: usize) -> Result<(), Diagnostic> {
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            if self.pos >= self.bytes.len() {
                return Err(self.error("E0002", "unterminated byte string literal", start));
            }
            let byte = self.peek_byte(0);
            match byte {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\n' | b'\r' => {
                    return Err(self.error("E0002", "unterminated byte string literal", start));
                }
                b'\\' => {
                    let esc_start = self.pos;
                    self.pos += 1;
                    let value = match self.peek_byte(0) {
                        b'\\' => b'\\',
                        b'"' => b'"',
                        b'\'' => b'\'',
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'0' => b'\0',
                        b'x' => {
                            self.pos += 1;
                            let value = self.scan_hex_byte(esc_start)?;
                            out.push(value);
                            continue;
                        }
                        _ => {
                            self.pos += 1;
                            return Err(self.error(
                                "E0003",
                                "invalid byte string escape",
                                esc_start,
                            ));
                        }
                    };
                    self.pos += 1;
                    out.push(value);
                }
                0x20..=0x7e => {
                    out.push(byte);
                    self.pos += 1;
                }
                _ => {
                    self.pos += self.cur_char().len_utf8();
                    return Err(self.error(
                        "E0009",
                        "a byte string can contain direct ASCII bytes only",
                        self.pos.saturating_sub(1),
                    ));
                }
            }
        }
        self.push(Tok::Bytes(out), start);
        Ok(())
    }

    /// Scan exactly two hexadecimal digits after one `\\x` escape.
    fn scan_hex_byte(&mut self, esc_start: usize) -> Result<u8, Diagnostic> {
        let hi = self.peek_byte(0);
        let lo = self.peek_byte(1);
        if !hi.is_ascii_hexdigit() || !lo.is_ascii_hexdigit() {
            return Err(self.error(
                "E0003",
                "a byte escape needs two hexadecimal digits",
                esc_start,
            ));
        }
        self.pos += 2;
        let digit = |byte: u8| (byte as char).to_digit(16).expect("validated hex") as u8;
        Ok((digit(hi) << 4) | digit(lo))
    }

    /// Scan one `#{ expression }` interpolation. `pos` is at `{`.
    fn scan_interpolation(&mut self) -> Result<StrPiece, Diagnostic> {
        let brace = self.pos;
        let interpolation_depth = self.interpolation_depth + 1;
        if interpolation_depth > MAX_INTERPOLATION_DEPTH {
            return Err(self.error(
                "E1022",
                format!(
                    "the interpolation nesting is deeper than the limit of \
                     {MAX_INTERPOLATION_DEPTH} levels"
                ),
                brace,
            ));
        }
        self.pos += 1;
        let expr_start = self.pos;
        let mut nested = Scanner {
            text: self.text,
            bytes: self.bytes,
            pos: self.pos,
            tokens: Vec::new(),
            nesting: Vec::new(),
            interpolation_depth,
        };
        let mut brace_depth = 0usize;
        loop {
            if nested.pos >= nested.bytes.len() || nested.bytes[nested.pos] == b'\n' {
                return Err(self.error(
                    "E0006",
                    "the interpolation expression has no closing `}`",
                    brace,
                ));
            }
            let byte = nested.bytes[nested.pos];
            if byte == b'}' && brace_depth == 0 {
                break;
            }
            if byte == b'#' {
                return Err(self.error(
                    "E0006",
                    "a comment cannot occur inside an interpolation expression",
                    nested.pos,
                ));
            }
            nested.scan_one()?;
            match byte {
                b'{' => brace_depth += 1,
                b'}' => brace_depth -= 1,
                _ => {}
            }
        }
        self.pos = nested.pos + 1;
        let inner = &self.text[expr_start..nested.pos];
        if inner.trim().is_empty() {
            return Err(self.error("E0006", "the interpolation expression is empty", brace));
        }
        let end = nested.pos as u32;
        nested.tokens.push(Token {
            tok: Tok::Eof,
            span: Span::new(end, end),
        });
        Ok(StrPiece::Expr(nested.tokens))
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
        let after_dot = matches!(self.tokens.last().map(|token| &token.tok), Some(Tok::Dot));
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
        let mut value = Some(0i64);
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
                .and_then(|value| value.checked_mul(radix as i64))
                .and_then(|value| value.checked_add(digit as i64));
            self.pos += 1;
        }
        if digit_count == 0 {
            self.pos = digits_start.max(self.pos);
            return Err(self.error("E0007", "invalid numeric literal", start));
        }
        if radix == 10 && !after_dot {
            let next = self.peek_byte(0);
            let exponent = (next == b'e' || next == b'E')
                && (self.peek_byte(1).is_ascii_digit()
                    || (matches!(self.peek_byte(1), b'+' | b'-')
                        && self.peek_byte(2).is_ascii_digit()));
            let is_float = (next == b'.' && self.peek_byte(1).is_ascii_digit()) || exponent;
            if is_float {
                if next == b'.' {
                    self.pos += 1;
                    while self.peek_byte(0).is_ascii_digit() || self.peek_byte(0) == b'_' {
                        self.pos += 1;
                    }
                }
                if matches!(self.peek_byte(0), b'e' | b'E') {
                    self.pos += 1;
                    if matches!(self.peek_byte(0), b'+' | b'-') {
                        self.pos += 1;
                    }
                    let exponent_start = self.pos;
                    while self.peek_byte(0).is_ascii_digit() || self.peek_byte(0) == b'_' {
                        self.pos += 1;
                    }
                    if self.pos == exponent_start {
                        return Err(self.error("E0007", "a float exponent needs digits", start));
                    }
                }
                if self.peek_byte(0).is_ascii_alphanumeric() {
                    return Err(self.error("E0007", "invalid numeric literal", start));
                }
                let cleaned: String = self.text[start..self.pos]
                    .chars()
                    .filter(|ch| *ch != '_')
                    .collect();
                let value = cleaned
                    .parse::<f64>()
                    .map_err(|_| self.error("E0007", "invalid float literal", start))?;
                self.push(Tok::Float(value.to_bits()), start);
                return Ok(());
            }
        }
        if self.peek_byte(0).is_ascii_alphanumeric() {
            return Err(self.error("E0007", "invalid numeric literal", start));
        }
        let value = value
            .ok_or_else(|| self.error("E0004", "integer literal is too large for Int", start))?;
        self.push(Tok::Int(value), start);
        Ok(())
    }

    fn scan_word(&mut self, start: usize) -> Result<(), Diagnostic> {
        while self.peek_byte(0).is_ascii_alphanumeric() || self.peek_byte(0) == b'_' {
            self.pos += 1;
        }
        let word = &self.text[start..self.pos];
        if self.peek_byte(0) == b'"' && word == "b" {
            return self.scan_bytes(start);
        }
        if self.peek_byte(0) == b'"' && word == "re" {
            return self.scan_regex(start);
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
            "final" if self.followed_by_class() => Tok::KwFinal,
            "frozen" if self.followed_by_class() => Tok::KwFrozen,
            "class" => Tok::KwClass,
            "do" => Tok::KwDo,
            "self" => Tok::KwSelf,
            "super" => Tok::KwSuper,
            "mut" => Tok::KwMut,
            "escaping" => Tok::KwEscaping,
            "nonescaping" => Tok::KwNonescaping,
            "as" => Tok::KwAs,
            "case" => Tok::KwCase,
            "select" => Tok::KwSelect,
            "effect" => Tok::KwEffect,
            "enum" => Tok::KwEnum,
            "in" => Tok::KwIn,
            "is" => Tok::KwIs,
            "then" => Tok::KwThen,
            "with" => Tok::KwWith,
            "loop" => Tok::KwLoop,
            "use" => Tok::KwUse,
            "interface" => Tok::KwInterface,
            "implements" => Tok::KwImplements,
            "when" => Tok::KwWhen,
            "type" => Tok::KwType,
            "for" => Tok::KwFor,
            "const" => Tok::KwConst,
            _ => Tok::Ident(word.to_string()),
        };
        // Track expression blocks inside delimiters.
        match &tok {
            Tok::KwIf | Tok::KwCase | Tok::KwSelect => {
                self.nesting.push(Nest::Block);
            }
            Tok::KwWhile | Tok::KwLoop | Tok::KwFor => self.nesting.push(Nest::LoopBlock),
            Tok::KwDo => {
                let follows_loop = matches!(
                    self.tokens.last().map(|token| &token.tok),
                    Some(Tok::KwLoop)
                );
                if self.nesting.last() == Some(&Nest::LoopBlock)
                    && (follows_loop || self.do_has_body_separator())
                {
                    *self.nesting.last_mut().expect("the loop block exists") = Nest::Block;
                } else {
                    self.nesting.push(Nest::Block);
                }
            }
            Tok::KwEnd => {
                if matches!(self.nesting.last(), Some(Nest::Block | Nest::LoopBlock)) {
                    self.nesting.pop();
                }
            }
            _ => {}
        }
        self.push(tok, start);
        Ok(())
    }

    /// Scan raw regular-expression source.
    fn scan_regex(&mut self, start: usize) -> Result<(), Diagnostic> {
        self.pos += 1;
        let mut pattern = String::new();
        loop {
            if self.pos >= self.bytes.len() {
                return Err(self.error("E0011", "unterminated regular-expression literal", start));
            }
            let ch = self.cur_char();
            match ch {
                '\n' | '\r' => {
                    return Err(self.error(
                        "E0011",
                        "unterminated regular-expression literal",
                        start,
                    ));
                }
                '"' => {
                    self.pos += 1;
                    break;
                }
                '\\' => {
                    pattern.push(ch);
                    self.pos += 1;
                    if self.pos >= self.bytes.len() || matches!(self.cur_char(), '\n' | '\r') {
                        return Err(self.error(
                            "E0011",
                            "unterminated regular-expression literal",
                            start,
                        ));
                    }
                    let escaped = self.cur_char();
                    pattern.push(escaped);
                    self.pos += escaped.len_utf8();
                }
                _ => {
                    pattern.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        self.push(Tok::Regex(pattern), start);
        Ok(())
    }

    /// True when class modifiers and `class` follow the current word.
    fn followed_by_class(&self) -> bool {
        let mut pos = self.pos;
        while matches!(self.bytes.get(pos), Some(b' ' | b'\t' | b'\r')) {
            pos += 1;
        }
        for modifier in ["final", "frozen"] {
            if self.word_at(pos, modifier) {
                pos += modifier.len();
                while matches!(self.bytes.get(pos), Some(b' ' | b'\t' | b'\r')) {
                    pos += 1;
                }
                break;
            }
        }
        self.word_at(pos, "class")
    }

    /// Test one complete source word at a byte position.
    fn word_at(&self, pos: usize, word: &str) -> bool {
        self.text[pos..].starts_with(word)
            && !self
                .bytes
                .get(pos + word.len())
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
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
        let tok = if self.text[self.pos..].starts_with(">>>") {
            self.pos += 3;
            Tok::Ushr
        } else if two(b'<', b'<', self) {
            self.pos += 2;
            Tok::Shl
        } else if two(b'>', b'>', self) {
            self.pos += 2;
            Tok::Shr
        } else if two(b'=', b'=', self) {
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
                b'&' => Tok::Amp,
                b'^' => Tok::Caret,
                b'~' => Tok::Tilde,
                b'?' => Tok::Question,
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
            vec![Tok::Str("a\n{{b}}A".to_string()), Tok::Eof]
        );
    }

    #[test]
    fn plain_strings_keep_braces_inert() {
        assert_eq!(
            kinds("\"{name} {{literal}}\""),
            vec![Tok::Str("{name} {{literal}}".to_string()), Tok::Eof]
        );
    }

    #[test]
    fn a_backslash_escapes_the_interpolation_marker() {
        let tokens = kinds("\"\\#{name} #{name}\"");
        let Tok::StrInterp(pieces) = &tokens[0] else {
            panic!("the second marker must interpolate");
        };
        assert_eq!(pieces[0], StrPiece::Lit("#{name} ".to_string()));
    }

    #[test]
    fn scans_interpolation_pieces() {
        let toks = kinds("\"Hello #{name}!\"");
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
                        assert_eq!(inner[0].span.lo, 9);
                        assert_eq!(inner[0].span.hi, 13);
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
        let string_error = scan("\"x #{\"").expect_err("the inner string must reject");
        assert_eq!(string_error.code, "E0002");
        assert!(string_error.message.contains("string"));
        assert_eq!(scan("\"x #{ }\"").unwrap_err().code, "E0006");
        assert_eq!(scan("\"x #{a").unwrap_err().code, "E0006");

        let comment_error =
            scan("\"x #{1 # comment}\"").expect_err("the interpolation comment must reject");
        assert_eq!(comment_error.code, "E0006");
        assert!(comment_error.message.contains("comment"));
    }

    #[test]
    fn interpolation_scans_strings_and_balanced_braces() {
        let tokens = kinds("\"#{\"text\"} #{{\"key\": 1}.at(\"key\")}\"");
        let Tok::StrInterp(pieces) = &tokens[0] else {
            panic!("the string must contain interpolation pieces");
        };
        assert_eq!(pieces.len(), 3);
        let StrPiece::Expr(first) = &pieces[0] else {
            panic!("the first piece must be an expression");
        };
        assert!(matches!(&first[0].tok, Tok::Str(text) if text == "text"));
        let StrPiece::Expr(second) = &pieces[2] else {
            panic!("the last piece must be an expression");
        };
        assert!(second.iter().any(|token| matches!(token.tok, Tok::LBrace)));
    }

    #[test]
    fn interpolation_nesting_stops_at_a_fixed_depth() {
        let mut source = "0".to_string();
        for _ in 0..=MAX_INTERPOLATION_DEPTH {
            source = format!("\"#{{{source}}}\"");
        }
        let error = scan(&source).expect_err("deep interpolation must reject");
        assert_eq!(error.code, "E1022");
        assert!(error.message.contains("interpolation nesting"));
    }

    #[test]
    fn scans_arrow_and_new_keywords() {
        assert_eq!(
            kinds("do |x: Int| -> mut escaping nonescaping self super final frozen class end"),
            vec![
                Tok::KwDo,
                Tok::Pipe,
                Tok::Ident("x".to_string()),
                Tok::Colon,
                Tok::Ident("Int".to_string()),
                Tok::Pipe,
                Tok::Arrow,
                Tok::KwMut,
                Tok::KwEscaping,
                Tok::KwNonescaping,
                Tok::KwSelf,
                Tok::KwSuper,
                Tok::KwFinal,
                Tok::KwFrozen,
                Tok::KwClass,
                Tok::KwEnd,
                Tok::Eof
            ]
        );
    }

    #[test]
    fn class_modifiers_are_contextual_keywords() {
        assert_eq!(
            kinds(
                "final = 1; frozen: Int = 2; final class A end; \
                 frozen class B end; frozen final class C end",
            ),
            vec![
                Tok::Ident("final".to_string()),
                Tok::Assign,
                Tok::Int(1),
                Tok::Newline,
                Tok::Ident("frozen".to_string()),
                Tok::Colon,
                Tok::Ident("Int".to_string()),
                Tok::Assign,
                Tok::Int(2),
                Tok::Newline,
                Tok::KwFinal,
                Tok::KwClass,
                Tok::Ident("A".to_string()),
                Tok::KwEnd,
                Tok::Newline,
                Tok::KwFrozen,
                Tok::KwClass,
                Tok::Ident("B".to_string()),
                Tok::KwEnd,
                Tok::Newline,
                Tok::KwFrozen,
                Tok::KwFinal,
                Tok::KwClass,
                Tok::Ident("C".to_string()),
                Tok::KwEnd,
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn scans_result_propagation() {
        assert_eq!(
            kinds("work()?"),
            vec![
                Tok::Ident("work".to_string()),
                Tok::LParen,
                Tok::RParen,
                Tok::Question,
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn scans_float_literals() {
        assert_eq!(
            kinds("1.5 1e9 2.5e-3 9223372036854775808.0"),
            vec![
                Tok::Float(1.5f64.to_bits()),
                Tok::Float(1e9f64.to_bits()),
                Tok::Float(2.5e-3f64.to_bits()),
                Tok::Float(9_223_372_036_854_775_808.0f64.to_bits()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn scans_chained_tuple_projections_as_integer_positions() {
        assert_eq!(
            kinds("value.0.1"),
            vec![
                Tok::Ident("value".to_string()),
                Tok::Dot,
                Tok::Int(0),
                Tok::Dot,
                Tok::Int(1),
                Tok::Eof,
            ]
        );
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
    fn scans_byte_literals() {
        assert_eq!(
            kinds("b\"LM\\0\\x01\\xff\""),
            vec![Tok::Bytes(vec![b'L', b'M', 0, 1, 255]), Tok::Eof]
        );
    }

    #[test]
    fn scans_raw_regular_expression_literals() {
        assert_eq!(
            kinds(r##"re"\p{Greek}+\"quoted\"#{raw}""##),
            vec![
                Tok::Regex(r##"\p{Greek}+\"quoted\"#{raw}"##.into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn rejects_unterminated_regular_expression_literals() {
        assert_eq!(scan(r#"re"abc"#).unwrap_err().code, "E0011");
        assert_eq!(scan("re\"abc\ndef\"").unwrap_err().code, "E0011");
    }

    #[test]
    fn scans_character_literals() {
        assert_eq!(
            kinds(r"'a' '猫' '\n' '\'' '\x41' '\u{1f642}'"),
            vec![
                Tok::Char('a'),
                Tok::Char('猫'),
                Tok::Char('\n'),
                Tok::Char('\''),
                Tok::Char('A'),
                Tok::Char('🙂'),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn rejects_invalid_character_literals() {
        for source in ["''", "'ab'", "'\\x80'", "'\\xGG'", "'\\u{}'", "'\\q'", "'a"] {
            assert_eq!(scan(source).unwrap_err().code, "E0008", "{source}");
        }
    }

    #[test]
    fn rejects_triple_literals() {
        assert_eq!(scan("\"\"\"x\"\"\"").unwrap_err().code, "E0010");
    }

    #[test]
    fn checks_hex_escape_ranges() {
        assert_eq!(
            kinds("\"\\x7f\""),
            vec![Tok::Str("\u{7f}".into()), Tok::Eof]
        );
        assert_eq!(scan("\"\\x80\"").unwrap_err().code, "E0003");
        assert_eq!(scan("b\"é\"").unwrap_err().code, "E0009");
    }

    #[test]
    fn scans_bitwise_operators() {
        assert_eq!(
            kinds("a & b | c ^ ~d << 1 >> 2 >>> 3"),
            vec![
                Tok::Ident("a".into()),
                Tok::Amp,
                Tok::Ident("b".into()),
                Tok::Pipe,
                Tok::Ident("c".into()),
                Tok::Caret,
                Tok::Tilde,
                Tok::Ident("d".into()),
                Tok::Shl,
                Tok::Int(1),
                Tok::Shr,
                Tok::Int(2),
                Tok::Ushr,
                Tok::Int(3),
                Tok::Eof,
            ]
        );
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
    fn emits_newline_between_expressions() {
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
    fn semicolon_separates_expressions() {
        assert_eq!(
            kinds("1; 2"),
            vec![Tok::Int(1), Tok::Newline, Tok::Int(2), Tok::Eof]
        );
    }
}
