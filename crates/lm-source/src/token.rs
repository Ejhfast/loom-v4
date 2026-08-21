//! Tokens for the week-2 language slice.

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
    /// String literal after escape processing, without interpolation.
    Str(String),
    /// String literal with interpolated expressions.
    StrInterp(Vec<StrPiece>),
    /// Identifier that is not a keyword.
    Ident(String),

    // Keywords inside the week-2 slice.
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
    KwType,
    KwFor,

    /// A reserved keyword outside the current language slice.
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

    /// Statement terminator: a newline or a semicolon.
    Newline,
    /// End of the token stream.
    Eof,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Tok::Int(v) => return write!(f, "integer literal `{v}`"),
            Tok::Str(_) | Tok::StrInterp(_) => return write!(f, "string literal"),
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
            Tok::KwType => "`type`",
            Tok::KwFor => "`for`",
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
