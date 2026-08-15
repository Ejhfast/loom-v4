//! Byte spans and source files.

/// A half-open byte range inside one source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(lo: u32, hi: u32) -> Span {
        Span { lo, hi }
    }

    /// Return the span that covers both input spans.
    pub fn to(self, other: Span) -> Span {
        Span {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        }
    }
}

/// One source file with its name and full text.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub name: String,
    pub text: String,
}

impl SourceFile {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> SourceFile {
        SourceFile {
            name: name.into(),
            text: text.into(),
        }
    }

    /// Convert a byte offset to a one-based line and column.
    /// The column counts Unicode scalar values.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let offset = (offset as usize).min(self.text.len());
        let mut line = 1u32;
        let mut col = 1u32;
        for (idx, ch) in self.text.char_indices() {
            if idx >= offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_counts_lines_and_columns() {
        let file = SourceFile::new("t.lm", "ab\ncd\n");
        assert_eq!(file.line_col(0), (1, 1));
        assert_eq!(file.line_col(1), (1, 2));
        assert_eq!(file.line_col(3), (2, 1));
        assert_eq!(file.line_col(4), (2, 2));
    }
}
