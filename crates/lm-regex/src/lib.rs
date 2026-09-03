//! Bounded regular-expression execution for Loom.
//!
//! This crate owns the regular-expression semantics used by every Loom engine.

use regex_automata::{meta, util::syntax, MatchKind, PatternID};
use std::{fmt, ops::ControlFlow, sync::Arc};

/// The largest accepted pattern, in UTF-8 bytes.
pub const MAX_PATTERN_BYTES: usize = 64 * 1024;

/// The largest accepted syntax nesting depth.
pub const MAX_NESTING_DEPTH: u32 = 64;

/// The largest accepted capture count, including the complete match.
pub const MAX_CAPTURES: usize = 128;

/// The largest compiled Thompson NFA.
pub const MAX_NFA_BYTES: usize = 2 * 1024 * 1024;

/// The largest compiled dense DFA.
pub const MAX_DFA_BYTES: usize = 2 * 1024 * 1024;

/// The largest compiled one-pass DFA.
pub const MAX_ONEPASS_BYTES: usize = 1024 * 1024;

/// The largest lazy-DFA cache for one search worker.
pub const MAX_HYBRID_CACHE_BYTES: usize = 2 * 1024 * 1024;

/// The largest compiled regex-literal table in one verified module.
pub const MAX_LITERAL_TABLE_BYTES: usize = 64 * 1024 * 1024;

/// The largest accepted replacement plan.
pub const MAX_REPLACEMENT_PARTS: usize = 4 * 1024;

/// One half-open byte range in UTF-8 text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteRange {
    /// The first matched byte.
    pub start: usize,
    /// The byte after the match.
    pub end: usize,
}

/// Captures from one match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captures {
    groups: Vec<Option<ByteRange>>,
}

/// One bounded replacement error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceError {
    /// The result exceeds its byte limit.
    Limit,
    /// The allocator rejected a bounded reservation.
    Allocation,
}

impl Captures {
    /// Return the complete match.
    pub fn complete(&self) -> ByteRange {
        self.groups[0].expect("a successful capture has a complete match")
    }

    /// Return the number of groups, including the complete match.
    pub fn len(&self) -> usize {
        self.groups.len()
    }

    /// Return true when the capture has no groups.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Return one group by index.
    pub fn group(&self, index: usize) -> Option<ByteRange> {
        self.groups.get(index).copied().flatten()
    }

    /// Return all groups, including the complete match.
    pub fn groups(&self) -> &[Option<ByteRange>] {
        &self.groups
    }
}

/// One regular-expression compile error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileErrorKind {
    /// The pattern has invalid syntax.
    Syntax,
    /// The pattern exceeds a fixed resource limit.
    Limit,
}

/// A stable compile error for the Loom runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    kind: CompileErrorKind,
    message: String,
}

impl CompileError {
    /// Return the error kind.
    pub fn kind(&self) -> CompileErrorKind {
        self.kind
    }

    /// Return the diagnostic detail.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CompileError {}

/// A compiled Loom regular expression.
#[derive(Clone)]
pub struct Regex {
    source: Arc<str>,
    engine: meta::Regex,
}

impl Regex {
    /// Compile one pattern with fixed syntax and memory limits.
    pub fn compile(pattern: &str) -> Result<Self, CompileError> {
        if pattern.len() > MAX_PATTERN_BYTES {
            return Err(limit_error("the regular expression is too large"));
        }
        let config = meta::Regex::config()
            .match_kind(MatchKind::LeftmostFirst)
            .nfa_size_limit(Some(MAX_NFA_BYTES))
            .onepass_size_limit(Some(MAX_ONEPASS_BYTES))
            .hybrid_cache_capacity(MAX_HYBRID_CACHE_BYTES)
            .dfa_size_limit(Some(MAX_DFA_BYTES))
            .pool_capacity(1)
            .backtrack(false)
            .utf8_empty(true);
        let syntax = syntax::Config::new()
            .unicode(true)
            .utf8(true)
            .nest_limit(MAX_NESTING_DEPTH)
            .octal(false);
        let engine = meta::Regex::builder()
            .configure(config)
            .syntax(syntax)
            .build(pattern)
            .map_err(map_build_error)?;
        if engine.captures_len() > MAX_CAPTURES {
            return Err(limit_error("the regular expression has too many captures"));
        }
        Ok(Self {
            source: Arc::from(pattern),
            engine,
        })
    }

    /// Return the source pattern.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Return the compiled program size.
    pub fn memory_usage(&self) -> usize {
        self.engine.memory_usage()
    }

    /// Return the number of groups, including the complete match.
    pub fn capture_count(&self) -> usize {
        self.engine.captures_len()
    }

    /// Resolve one named group.
    pub fn group_index(&self, name: &str) -> Option<usize> {
        self.engine.group_info().to_index(PatternID::ZERO, name)
    }

    /// Visit every named capture with its index.
    pub fn capture_names(&self) -> impl Iterator<Item = (usize, &str)> {
        self.engine
            .group_info()
            .pattern_names(PatternID::ZERO)
            .enumerate()
            .filter_map(|(index, name)| name.map(|name| (index, name)))
    }

    /// Test whether the text contains a match.
    pub fn is_match(&self, text: &str) -> bool {
        self.engine.is_match(text)
    }

    /// Find the first match.
    pub fn find(&self, text: &str) -> Option<ByteRange> {
        self.engine.find(text).map(|matched| ByteRange {
            start: matched.start(),
            end: matched.end(),
        })
    }

    /// Capture the first match.
    pub fn captures(&self, text: &str) -> Option<Captures> {
        let mut captures = self.engine.create_captures();
        self.engine.captures(text, &mut captures);
        captures.get_match()?;
        let groups = (0..captures.group_len())
            .map(|index| {
                captures.get_group(index).map(|span| ByteRange {
                    start: span.start,
                    end: span.end,
                })
            })
            .collect();
        Some(Captures { groups })
    }

    /// Visit all non-overlapping matches.
    pub fn visit_matches<B>(
        &self,
        text: &str,
        mut visitor: impl FnMut(ByteRange) -> ControlFlow<B>,
    ) -> ControlFlow<B> {
        for matched in self.engine.find_iter(text) {
            visitor(ByteRange {
                start: matched.start(),
                end: matched.end(),
            })?;
        }
        ControlFlow::Continue(())
    }

    /// Count all non-overlapping matches.
    pub fn count(&self, text: &str) -> usize {
        self.engine.find_iter(text).count()
    }

    /// Return the non-matching ranges between all matches.
    pub fn split_ranges(&self, text: &str) -> Vec<ByteRange> {
        self.split_range_iter(text).collect()
    }

    /// Iterate over non-matching ranges between all matches.
    pub fn split_range_iter<'a>(&'a self, text: &'a str) -> impl Iterator<Item = ByteRange> + 'a {
        self.engine.split(text).map(|span| ByteRange {
            start: span.start,
            end: span.end,
        })
    }

    /// Replace every match and enforce one result byte limit.
    pub fn replace_all(
        &self,
        text: &str,
        replacement: &str,
        max_bytes: usize,
    ) -> Result<String, ReplaceError> {
        let parts = replacement_parts(replacement)?;
        let mut output = String::new();
        let mut copied = 0;
        for captures in self.engine.captures_iter(text) {
            let Some(complete) = captures.get_match() else {
                continue;
            };
            push_bounded(&mut output, &text[copied..complete.start()], max_bytes)?;
            for part in &parts {
                match *part {
                    ReplacementPart::Text(value) => {
                        push_bounded(&mut output, value, max_bytes)?;
                    }
                    ReplacementPart::Index(index) => {
                        if let Some(span) = captures.get_group(index) {
                            push_bounded(&mut output, &text[span], max_bytes)?;
                        }
                    }
                    ReplacementPart::Name(name) => {
                        let index = self.group_index(name);
                        if let Some(span) = index.and_then(|index| captures.get_group(index)) {
                            push_bounded(&mut output, &text[span], max_bytes)?;
                        }
                    }
                }
            }
            copied = complete.end();
        }
        push_bounded(&mut output, &text[copied..], max_bytes)?;
        Ok(output)
    }
}

#[derive(Debug, Clone, Copy)]
enum ReplacementPart<'a> {
    Text(&'a str),
    Index(usize),
    Name(&'a str),
}

fn replacement_parts(replacement: &str) -> Result<Vec<ReplacementPart<'_>>, ReplaceError> {
    let mut parts = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = replacement[cursor..].find('$') {
        let marker = cursor + relative;
        if marker > cursor {
            push_part(
                &mut parts,
                ReplacementPart::Text(&replacement[cursor..marker]),
            )?;
        }
        let suffix = &replacement[marker + 1..];
        if suffix.starts_with('$') {
            push_part(&mut parts, ReplacementPart::Text("$"))?;
            cursor = marker + 2;
            continue;
        }
        if let Some(braced) = suffix.strip_prefix('{') {
            if let Some(end) = braced.find('}') {
                push_reference(&mut parts, &braced[..end])?;
                cursor = marker + 3 + end;
                continue;
            }
        }
        let length = match suffix.as_bytes().first().copied() {
            Some(byte) if byte.is_ascii_digit() => {
                suffix.bytes().take_while(u8::is_ascii_digit).count()
            }
            Some(byte) if byte.is_ascii_alphabetic() || byte == b'_' => suffix
                .bytes()
                .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                .count(),
            _ => 0,
        };
        if length == 0 {
            push_part(&mut parts, ReplacementPart::Text("$"))?;
            cursor = marker + 1;
        } else {
            push_reference(&mut parts, &suffix[..length])?;
            cursor = marker + 1 + length;
        }
    }
    if cursor < replacement.len() {
        push_part(&mut parts, ReplacementPart::Text(&replacement[cursor..]))?;
    }
    Ok(parts)
}

fn push_reference<'a>(
    parts: &mut Vec<ReplacementPart<'a>>,
    reference: &'a str,
) -> Result<(), ReplaceError> {
    let part = if !reference.is_empty() && reference.bytes().all(|byte| byte.is_ascii_digit()) {
        match reference.parse::<usize>() {
            Ok(index) => ReplacementPart::Index(index),
            Err(_) => ReplacementPart::Name(reference),
        }
    } else {
        ReplacementPart::Name(reference)
    };
    push_part(parts, part)
}

fn push_part<'a>(
    parts: &mut Vec<ReplacementPart<'a>>,
    part: ReplacementPart<'a>,
) -> Result<(), ReplaceError> {
    if parts.len() >= MAX_REPLACEMENT_PARTS {
        return Err(ReplaceError::Limit);
    }
    parts.try_reserve(1).map_err(|_| ReplaceError::Allocation)?;
    parts.push(part);
    Ok(())
}

fn push_bounded(output: &mut String, text: &str, max: usize) -> Result<(), ReplaceError> {
    let length = output
        .len()
        .checked_add(text.len())
        .ok_or(ReplaceError::Limit)?;
    if length > max {
        return Err(ReplaceError::Limit);
    }
    output
        .try_reserve(text.len())
        .map_err(|_| ReplaceError::Allocation)?;
    output.push_str(text);
    Ok(())
}

impl fmt::Debug for Regex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Regex")
            .field("source", &self.source)
            .field("memory_usage", &self.memory_usage())
            .finish()
    }
}

impl PartialEq for Regex {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for Regex {}

fn limit_error(message: &str) -> CompileError {
    CompileError {
        kind: CompileErrorKind::Limit,
        message: message.to_string(),
    }
}

fn map_build_error(error: meta::BuildError) -> CompileError {
    let syntax_limit = error.syntax_error().is_some_and(|error| match error {
        regex_syntax::Error::Parse(error) => matches!(
            error.kind(),
            regex_syntax::ast::ErrorKind::CaptureLimitExceeded
                | regex_syntax::ast::ErrorKind::NestLimitExceeded(_)
        ),
        _ => false,
    });
    let kind = if error.size_limit().is_some() || syntax_limit {
        CompileErrorKind::Limit
    } else {
        CompileErrorKind::Syntax
    };
    CompileError {
        kind,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_unicode_text_with_leftmost_first_rules() {
        let regex = Regex::compile(r"sam|samwise|\p{Greek}+").unwrap();
        assert_eq!(
            regex.find("samwise αβ"),
            Some(ByteRange { start: 0, end: 3 })
        );
        assert_eq!(regex.find("x αβ"), Some(ByteRange { start: 2, end: 6 }));
    }

    #[test]
    fn returns_numbered_and_named_captures() {
        let regex = Regex::compile(r"(?P<name>[a-z]+)-([0-9]+)").unwrap();
        let captures = regex.captures("x ab-42 y").unwrap();
        assert_eq!(captures.complete(), ByteRange { start: 2, end: 7 });
        assert_eq!(captures.group(1), Some(ByteRange { start: 2, end: 4 }));
        assert_eq!(captures.group(2), Some(ByteRange { start: 5, end: 7 }));
        assert_eq!(regex.group_index("name"), Some(1));
    }

    #[test]
    fn visits_empty_matches_only_at_utf8_boundaries() {
        let regex = Regex::compile("").unwrap();
        let mut ranges = Vec::new();
        let _: ControlFlow<()> = regex.visit_matches("☃", |range| {
            ranges.push(range);
            ControlFlow::Continue(())
        });
        assert_eq!(
            ranges,
            vec![
                ByteRange { start: 0, end: 0 },
                ByteRange { start: 3, end: 3 }
            ]
        );
    }

    #[test]
    fn rejects_non_regular_features() {
        for pattern in [r"(a)\1", r"a(?=b)", r"(?(1)a|b)"] {
            let error = Regex::compile(pattern).unwrap_err();
            assert_eq!(error.kind(), CompileErrorKind::Syntax);
        }
    }

    #[test]
    fn enforces_pattern_and_capture_limits() {
        let long = "a".repeat(MAX_PATTERN_BYTES + 1);
        assert_eq!(
            Regex::compile(&long).unwrap_err().kind(),
            CompileErrorKind::Limit
        );

        let captures = "()".repeat(MAX_CAPTURES);
        assert_eq!(
            Regex::compile(&captures).unwrap_err().kind(),
            CompileErrorKind::Limit
        );

        let nesting = format!(
            "{}a{}",
            "(?:".repeat(MAX_NESTING_DEPTH as usize + 1),
            ")".repeat(MAX_NESTING_DEPTH as usize + 1)
        );
        assert_eq!(
            Regex::compile(&nesting).unwrap_err().kind(),
            CompileErrorKind::Limit
        );
    }

    #[test]
    fn replaces_numbered_and_named_groups() {
        let regex = Regex::compile(r"(?P<word>[a-z]+)-([0-9]+)").unwrap();
        assert_eq!(
            regex
                .replace_all("a-1 b-22", "${word}:$2 ($$)", 64)
                .unwrap(),
            "a:1 ($) b:22 ($)"
        );
        assert_eq!(regex.replace_all("a-1", "$1", 0), Err(ReplaceError::Limit));
        assert_eq!(regex.replace_all("a-1", "$2x", 64).unwrap(), "1x");
    }

    #[test]
    fn bounds_replacement_plans() {
        let regex = Regex::compile("a").unwrap();
        let replacement = "$1".repeat(MAX_REPLACEMENT_PARTS + 1);
        assert_eq!(
            regex.replace_all("a", &replacement, usize::MAX),
            Err(ReplaceError::Limit)
        );
    }
}
