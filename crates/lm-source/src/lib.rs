//! Source handling for the Loom bootstrap compiler.
//!
//! This crate reads UTF-8 source, produces tokens with spans,
//! parses the week-1 language slice, and renders diagnostics.

pub mod ast;
pub mod diag;
pub mod parse;
pub mod scan;
pub mod span;
pub mod syntax;
pub mod token;

pub use diag::Diagnostic;
pub use span::{SourceFile, Span};
