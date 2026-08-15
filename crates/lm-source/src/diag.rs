//! Deterministic diagnostics with stable codes.

use crate::span::{SourceFile, Span};

/// One compile diagnostic with a stable code, a message, and a span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    pub fn new(code: &'static str, message: impl Into<String>, span: Span) -> Diagnostic {
        Diagnostic {
            code,
            message: message.into(),
            span,
        }
    }

    /// Render the diagnostic in the stable two-line format:
    ///
    /// ```text
    /// error[E1004]: expected Int, found String
    ///   --> tests/ui/type-mismatch.lm:2:5
    /// ```
    pub fn render(&self, file: &SourceFile) -> String {
        let (line, col) = file.line_col(self.span.lo);
        format!(
            "error[{}]: {}\n  --> {}:{}:{}\n",
            self.code, self.message, file.name, line, col
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_matches_stable_format() {
        let file = SourceFile::new("tests/ui/type-mismatch.lm", "x = 1\nx = \"two\"\n");
        let diag = Diagnostic::new("E1004", "expected Int, found String", Span::new(10, 15));
        assert_eq!(
            diag.render(&file),
            "error[E1004]: expected Int, found String\n  --> tests/ui/type-mismatch.lm:2:5\n"
        );
    }
}
