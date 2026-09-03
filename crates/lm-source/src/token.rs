//! Tokens for Loom source code.

use crate::span::Span;
use std::fmt;

/// One piece of an interpolated string literal.
#[derive(Debug, Clone, PartialEq)]
pub enum StrPiece {
    /// Literal text after escape processing.
    Lit(String),
    /// The tokens of one interpolated expression.
    Expr(Vec<Token>),
}

/// The kind of one token.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// Decimal, hexadecimal, octal, or binary integer literal.
    Int(i64),
    /// IEEE 754 binary64 literal, stored as raw bits.
    Float(u64),
    /// One Unicode scalar value.
    Char(char),
    /// String literal after escape processing, without interpolation.
    Str(String),
    /// String literal with interpolated expressions.
    StrInterp(Vec<StrPiece>),
    /// Immutable byte literal after escape processing.
    Bytes(Vec<u8>),
    /// Raw regular-expression source.
    Regex(String),
    /// Identifier that is not a keyword.
    Ident(String),

    // Language keywords.
    KwAnd,
    KwOr,
    KwNot,
    KwIf,
    KwElsif,
    KwElse,
    KwEnd,
    KwWhile,
    KwBreak,
    KwContinue,
    KwDef,
    KwReturn,
    KwTrue,
    KwFalse,
    KwFinal,
    KwFrozen,
    KwClass,
    KwDo,
    KwSelf,
    KwSuper,
    KwMut,
    KwEscaping,
    KwEnum,
    KwCase,
    KwSelect,
    KwIn,
    KwThen,
    KwWith,
    KwEffect,
    KwIs,
    KwAs,
    KwLoop,
    KwUse,
    KwInterface,
    KwImplements,
    KwWhen,
    KwType,
    KwFor,
    KwConst,

    /// A reserved keyword with no accepted syntax.
    KwReserved(&'static str),

    // Punctuation.
    LParen,
    RParen,
    LBracket,
    RBracket,
    /// A left brace that opens a map literal or a map type.
    LBrace,
    /// A left brace that opens a brace closure. The scanner makes
    /// this decision once, so the parser never repeats it.
    LBraceClosure,
    RBrace,
    Comma,
    Colon,
    Dot,
    Pipe,
    Arrow,
    Question,

    // Operators.
    Assign,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Caret,
    Shl,
    Shr,
    Ushr,
    Tilde,

    /// Statement terminator: a newline or a semicolon.
    Newline,
    /// End of the token stream.
    Eof,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Tok::Int(v) => return write!(f, "integer literal `{v}`"),
            Tok::Float(v) => return write!(f, "float literal `{}`", f64::from_bits(*v)),
            Tok::Char(v) => return write!(f, "character literal `{v:?}`"),
            Tok::Str(_) | Tok::StrInterp(_) => return write!(f, "string literal"),
            Tok::Bytes(_) => return write!(f, "byte string literal"),
            Tok::Regex(_) => return write!(f, "regular-expression literal"),
            Tok::Ident(name) => return write!(f, "`{name}`"),
            Tok::KwAnd => "`and`",
            Tok::KwOr => "`or`",
            Tok::KwNot => "`not`",
            Tok::KwIf => "`if`",
            Tok::KwElsif => "`elsif`",
            Tok::KwElse => "`else`",
            Tok::KwEnd => "`end`",
            Tok::KwWhile => "`while`",
            Tok::KwBreak => "`break`",
            Tok::KwContinue => "`continue`",
            Tok::KwDef => "`def`",
            Tok::KwReturn => "`return`",
            Tok::KwTrue => "`true`",
            Tok::KwFalse => "`false`",
            Tok::KwFinal => "`final`",
            Tok::KwFrozen => "`frozen`",
            Tok::KwClass => "`class`",
            Tok::KwDo => "`do`",
            Tok::KwSelf => "`self`",
            Tok::KwSuper => "`super`",
            Tok::KwMut => "`mut`",
            Tok::KwEscaping => "`escaping`",
            Tok::KwEnum => "`enum`",
            Tok::KwCase => "`case`",
            Tok::KwSelect => "`select`",
            Tok::KwIn => "`in`",
            Tok::KwThen => "`then`",
            Tok::KwWith => "`with`",
            Tok::KwEffect => "`effect`",
            Tok::KwIs => "`is`",
            Tok::KwAs => "`as`",
            Tok::KwLoop => "`loop`",
            Tok::KwUse => "`use`",
            Tok::KwInterface => "`interface`",
            Tok::KwImplements => "`implements`",
            Tok::KwWhen => "`when`",
            Tok::KwType => "`type`",
            Tok::KwFor => "`for`",
            Tok::KwConst => "`const`",
            Tok::KwReserved(name) => return write!(f, "`{name}`"),
            Tok::LParen => "`(`",
            Tok::RParen => "`)`",
            Tok::LBracket => "`[`",
            Tok::RBracket => "`]`",
            Tok::LBrace => "`{`",
            // The reader writes one brace; the split is internal.
            Tok::LBraceClosure => "`{`",
            Tok::RBrace => "`}`",
            Tok::Comma => "`,`",
            Tok::Colon => "`:`",
            Tok::Dot => "`.`",
            Tok::Pipe => "`|`",
            Tok::Arrow => "`->`",
            Tok::Question => "`?`",
            Tok::Assign => "`=`",
            Tok::EqEq => "`==`",
            Tok::NotEq => "`!=`",
            Tok::Lt => "`<`",
            Tok::Le => "`<=`",
            Tok::Gt => "`>`",
            Tok::Ge => "`>=`",
            Tok::Plus => "`+`",
            Tok::Minus => "`-`",
            Tok::Star => "`*`",
            Tok::Slash => "`/`",
            Tok::Percent => "`%`",
            Tok::Amp => "`&`",
            Tok::Caret => "`^`",
            Tok::Shl => "`<<`",
            Tok::Shr => "`>>`",
            Tok::Ushr => "`>>>`",
            Tok::Tilde => "`~`",
            Tok::Newline => "end of line",
            Tok::Eof => "end of file",
        };
        f.write_str(text)
    }
}

/// One token with its span.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}
