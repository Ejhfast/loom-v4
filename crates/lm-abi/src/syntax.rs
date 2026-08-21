//! The portable public syntax record format.

use std::fmt;

pub const MAGIC: [u8; 8] = *b"LMSYNT\0\x01";
pub const FORMAT_VERSION: u16 = 1;
pub const GRAMMAR_MAJOR: u16 = 1;
pub const GRAMMAR_MINOR: u16 = 1;
const HEADER_SIZE: usize = 28;
const RECORD_SIZE: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SyntaxClass {
    Node = 0,
    Token = 1,
    Trivia = 2,
    Invalid = 3,
}

impl SyntaxClass {
    fn from_tag(tag: u8) -> Option<SyntaxClass> {
        Some(match tag {
            0 => SyntaxClass::Node,
            1 => SyntaxClass::Token,
            2 => SyntaxClass::Trivia,
            3 => SyntaxClass::Invalid,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxRecord {
    pub class: SyntaxClass,
    pub kind: u16,
    pub lo: u32,
    pub hi: u32,
    pub child_start: u32,
    pub child_len: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxError {
    Header,
    Version,
    Length,
    Reference,
    Range,
    Kind,
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            SyntaxError::Header => "the syntax data has an invalid header",
            SyntaxError::Version => "the syntax data has another format version",
            SyntaxError::Length => "the syntax data has an invalid length",
            SyntaxError::Reference => "the syntax data has an invalid reference",
            SyntaxError::Range => "the syntax data has an invalid source range",
            SyntaxError::Kind => "the syntax data has an invalid item kind",
        };
        f.write_str(text)
    }
}

pub struct SyntaxView<'a> {
    bytes: &'a [u8],
    source_len: u32,
    items: u32,
    children: u32,
    root: u32,
}

impl<'a> SyntaxView<'a> {
    pub fn new(bytes: &'a [u8], source_len: usize) -> Result<SyntaxView<'a>, SyntaxError> {
        if bytes.len() < HEADER_SIZE || bytes[..8] != MAGIC {
            return Err(SyntaxError::Header);
        }
        if read_u16(bytes, 8)? != FORMAT_VERSION
            || read_u16(bytes, 10)? != GRAMMAR_MAJOR
            || read_u16(bytes, 12)? != GRAMMAR_MINOR
            || read_u16(bytes, 14)? != 0
        {
            return Err(SyntaxError::Version);
        }
        let items = read_u32(bytes, 16)?;
        let children = read_u32(bytes, 20)?;
        let root = read_u32(bytes, 24)?;
        if root >= items {
            return Err(SyntaxError::Reference);
        }
        let record_bytes = (items as usize)
            .checked_mul(RECORD_SIZE)
            .ok_or(SyntaxError::Length)?;
        let child_bytes = (children as usize)
            .checked_mul(4)
            .ok_or(SyntaxError::Length)?;
        let expected = HEADER_SIZE
            .checked_add(record_bytes)
            .and_then(|size| size.checked_add(child_bytes))
            .ok_or(SyntaxError::Length)?;
        if expected != bytes.len() || source_len > u32::MAX as usize {
            return Err(SyntaxError::Length);
        }
        let view = SyntaxView {
            bytes,
            source_len: source_len as u32,
            items,
            children,
            root,
        };
        view.record(root)?;
        Ok(view)
    }

    pub fn root(&self) -> u32 {
        self.root
    }

    pub fn item_count(&self) -> u32 {
        self.items
    }

    pub fn record(&self, index: u32) -> Result<SyntaxRecord, SyntaxError> {
        if index >= self.items {
            return Err(SyntaxError::Reference);
        }
        let at = HEADER_SIZE + index as usize * RECORD_SIZE;
        let class = SyntaxClass::from_tag(self.bytes[at]).ok_or(SyntaxError::Kind)?;
        if self.bytes[at + 1] != 0 {
            return Err(SyntaxError::Kind);
        }
        let kind = read_u16(self.bytes, at + 2)?;
        if syntax_kind_class(kind) != Some(class) {
            return Err(SyntaxError::Kind);
        }
        let lo = read_u32(self.bytes, at + 4)?;
        let hi = read_u32(self.bytes, at + 8)?;
        let child_start = read_u32(self.bytes, at + 12)?;
        let child_len = read_u32(self.bytes, at + 16)?;
        let child_end = child_start
            .checked_add(child_len)
            .ok_or(SyntaxError::Reference)?;
        if lo > hi || hi > self.source_len {
            return Err(SyntaxError::Range);
        }
        if child_end > self.children
            || (matches!(class, SyntaxClass::Token | SyntaxClass::Trivia) && child_len != 0)
        {
            return Err(SyntaxError::Reference);
        }
        Ok(SyntaxRecord {
            class,
            kind,
            lo,
            hi,
            child_start,
            child_len,
        })
    }

    pub fn child(&self, record: SyntaxRecord, offset: u32) -> Result<u32, SyntaxError> {
        if offset >= record.child_len {
            return Err(SyntaxError::Reference);
        }
        let ordinal = record
            .child_start
            .checked_add(offset)
            .ok_or(SyntaxError::Reference)?;
        let base = HEADER_SIZE + self.items as usize * RECORD_SIZE;
        let index = read_u32(self.bytes, base + ordinal as usize * 4)?;
        if index >= self.items {
            return Err(SyntaxError::Reference);
        }
        Ok(index)
    }
}

pub fn encode_syntax(
    records: &[SyntaxRecord],
    children: &[u32],
    root: u32,
) -> Result<Vec<u8>, SyntaxError> {
    if root as usize >= records.len() {
        return Err(SyntaxError::Reference);
    }
    if records.len() > u32::MAX as usize || children.len() > u32::MAX as usize {
        return Err(SyntaxError::Length);
    }
    let record_bytes = records
        .len()
        .checked_mul(RECORD_SIZE)
        .ok_or(SyntaxError::Length)?;
    let child_bytes = children.len().checked_mul(4).ok_or(SyntaxError::Length)?;
    let size = HEADER_SIZE
        .checked_add(record_bytes)
        .and_then(|value| value.checked_add(child_bytes))
        .ok_or(SyntaxError::Length)?;
    let mut out = Vec::new();
    out.try_reserve_exact(size)
        .map_err(|_| SyntaxError::Length)?;
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&GRAMMAR_MAJOR.to_le_bytes());
    out.extend_from_slice(&GRAMMAR_MINOR.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(records.len() as u32).to_le_bytes());
    out.extend_from_slice(&(children.len() as u32).to_le_bytes());
    out.extend_from_slice(&root.to_le_bytes());
    for record in records {
        out.push(record.class as u8);
        out.push(0);
        out.extend_from_slice(&record.kind.to_le_bytes());
        out.extend_from_slice(&record.lo.to_le_bytes());
        out.extend_from_slice(&record.hi.to_le_bytes());
        out.extend_from_slice(&record.child_start.to_le_bytes());
        out.extend_from_slice(&record.child_len.to_le_bytes());
    }
    for child in children {
        out.extend_from_slice(&child.to_le_bytes());
    }
    Ok(out)
}

/// One compact syntax subtree and its former source range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedSyntax {
    pub source_start: u32,
    pub source_end: u32,
    pub records: Vec<u8>,
    pub root: u32,
}

/// One syntax subtree used by a structural node build.
pub struct SyntaxPart<'a> {
    pub source: &'a str,
    pub records: &'a [u8],
    pub index: u32,
}

/// One compact node build result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltSyntax {
    pub source: String,
    pub records: Vec<u8>,
}

/// Build one node from existing immutable syntax subtrees.
pub fn build_syntax_node(kind: u16, parts: &[SyntaxPart<'_>]) -> Result<BuiltSyntax, SyntaxError> {
    let class = syntax_kind_class(kind).ok_or(SyntaxError::Kind)?;
    if !matches!(class, SyntaxClass::Node | SyntaxClass::Invalid) {
        return Err(SyntaxError::Kind);
    }

    let mut source = String::new();
    let mut records = Vec::new();
    let mut children = Vec::new();
    let mut roots = Vec::new();
    roots
        .try_reserve_exact(parts.len())
        .map_err(|_| SyntaxError::Length)?;
    for part in parts {
        let detached = detach_syntax(part.records, part.source.len(), part.index)?;
        let text = part
            .source
            .get(detached.source_start as usize..detached.source_end as usize)
            .ok_or(SyntaxError::Range)?;
        let source_offset = u32::try_from(source.len()).map_err(|_| SyntaxError::Length)?;
        source
            .try_reserve(text.len())
            .map_err(|_| SyntaxError::Length)?;
        source.push_str(text);

        let view = SyntaxView::new(&detached.records, text.len())?;
        records
            .try_reserve_exact(view.item_count() as usize)
            .map_err(|_| SyntaxError::Length)?;
        let base = u32::try_from(records.len()).map_err(|_| SyntaxError::Length)?;
        roots.push(base.checked_add(view.root()).ok_or(SyntaxError::Length)?);
        for index in 0..view.item_count() {
            let record = view.record(index)?;
            children
                .try_reserve_exact(record.child_len as usize)
                .map_err(|_| SyntaxError::Length)?;
            let child_start = u32::try_from(children.len()).map_err(|_| SyntaxError::Length)?;
            for offset in 0..record.child_len {
                let child = view.child(record, offset)?;
                children.push(base.checked_add(child).ok_or(SyntaxError::Length)?);
            }
            records.push(SyntaxRecord {
                class: record.class,
                kind: record.kind,
                lo: source_offset
                    .checked_add(record.lo)
                    .ok_or(SyntaxError::Length)?,
                hi: source_offset
                    .checked_add(record.hi)
                    .ok_or(SyntaxError::Length)?,
                child_start,
                child_len: record.child_len,
            });
        }
    }

    let child_start = u32::try_from(children.len()).map_err(|_| SyntaxError::Length)?;
    let child_len = u32::try_from(roots.len()).map_err(|_| SyntaxError::Length)?;
    children
        .try_reserve_exact(roots.len())
        .map_err(|_| SyntaxError::Length)?;
    children.extend(roots);
    let root = u32::try_from(records.len()).map_err(|_| SyntaxError::Length)?;
    records
        .try_reserve_exact(1)
        .map_err(|_| SyntaxError::Length)?;
    records.push(SyntaxRecord {
        class,
        kind,
        lo: 0,
        hi: u32::try_from(source.len()).map_err(|_| SyntaxError::Length)?,
        child_start,
        child_len,
    });
    Ok(BuiltSyntax {
        source,
        records: encode_syntax(&records, &children, root)?,
    })
}

/// Build one leaf from its kind and exact text.
pub fn build_syntax_leaf(
    class: SyntaxClass,
    kind: u16,
    text: &str,
) -> Result<Vec<u8>, SyntaxError> {
    if syntax_kind_class(kind) != Some(class)
        || !matches!(class, SyntaxClass::Token | SyntaxClass::Trivia)
    {
        return Err(SyntaxError::Kind);
    }
    let hi = u32::try_from(text.len()).map_err(|_| SyntaxError::Length)?;
    encode_syntax(
        &[SyntaxRecord {
            class,
            kind,
            lo: 0,
            hi,
            child_start: 0,
            child_len: 0,
        }],
        &[],
        0,
    )
}

/// Copy one syntax subtree into compact independent records.
pub fn detach_syntax(
    bytes: &[u8],
    source_len: usize,
    index: u32,
) -> Result<DetachedSyntax, SyntaxError> {
    let view = SyntaxView::new(bytes, source_len)?;
    let root = view.record(index)?;
    let item_count = view.item_count() as usize;
    let mut seen = Vec::new();
    seen.try_reserve_exact(item_count)
        .map_err(|_| SyntaxError::Length)?;
    seen.resize(item_count, false);
    let mut order = Vec::new();
    order
        .try_reserve_exact(item_count)
        .map_err(|_| SyntaxError::Length)?;
    let mut stack = Vec::new();
    stack
        .try_reserve_exact(item_count)
        .map_err(|_| SyntaxError::Length)?;
    stack.push(index);
    while let Some(current) = stack.pop() {
        let seen_item = seen
            .get_mut(current as usize)
            .ok_or(SyntaxError::Reference)?;
        if *seen_item {
            return Err(SyntaxError::Reference);
        }
        *seen_item = true;
        order.push(current);
        let record = view.record(current)?;
        if record.lo < root.lo || record.hi > root.hi {
            return Err(SyntaxError::Range);
        }
        for offset in (0..record.child_len).rev() {
            let child = view.child(record, offset)?;
            let child_record = view.record(child)?;
            if child_record.lo < record.lo || child_record.hi > record.hi {
                return Err(SyntaxError::Range);
            }
            stack.push(child);
        }
    }
    let mut mapping = Vec::new();
    mapping
        .try_reserve_exact(item_count)
        .map_err(|_| SyntaxError::Length)?;
    mapping.resize(item_count, u32::MAX);
    for (new, old) in order.iter().enumerate() {
        mapping[*old as usize] = new as u32;
    }
    let mut records = Vec::new();
    let mut children = Vec::new();
    records
        .try_reserve_exact(order.len())
        .map_err(|_| SyntaxError::Length)?;
    let mut child_total = 0usize;
    for old in &order {
        child_total = child_total
            .checked_add(view.record(*old)?.child_len as usize)
            .ok_or(SyntaxError::Length)?;
    }
    children
        .try_reserve_exact(child_total)
        .map_err(|_| SyntaxError::Length)?;
    for old in order {
        let source = view.record(old)?;
        let child_start = children.len() as u32;
        for offset in 0..source.child_len {
            let child = view.child(source, offset)?;
            let mapped = *mapping
                .get(child as usize)
                .filter(|mapped| **mapped != u32::MAX)
                .ok_or(SyntaxError::Reference)?;
            children.push(mapped);
        }
        records.push(SyntaxRecord {
            class: source.class,
            kind: source.kind,
            lo: source.lo - root.lo,
            hi: source.hi - root.lo,
            child_start,
            child_len: source.child_len,
        });
    }
    Ok(DetachedSyntax {
        source_start: root.lo,
        source_end: root.hi,
        records: encode_syntax(&records, &children, 0)?,
        root: 0,
    })
}

fn read_u16(bytes: &[u8], at: usize) -> Result<u16, SyntaxError> {
    let source = bytes.get(at..at + 2).ok_or(SyntaxError::Length)?;
    Ok(u16::from_le_bytes([source[0], source[1]]))
}

fn read_u32(bytes: &[u8], at: usize) -> Result<u32, SyntaxError> {
    let source = bytes.get(at..at + 4).ok_or(SyntaxError::Length)?;
    Ok(u32::from_le_bytes([
        source[0], source[1], source[2], source[3],
    ]))
}

pub const KIND_MODULE: u16 = 1;
pub const KIND_USE: u16 = 2;
pub const KIND_INTERFACE: u16 = 3;
pub const KIND_CLASS: u16 = 4;
pub const KIND_ENUM: u16 = 5;
pub const KIND_FUNCTION: u16 = 6;
pub const KIND_STATEMENT: u16 = 7;
pub const KIND_INVALID: u16 = 8;
pub const KIND_INT: u16 = 100;
pub const KIND_STRING: u16 = 101;
pub const KIND_IDENTIFIER: u16 = 102;
pub const KIND_KEYWORD: u16 = 103;
pub const KIND_LPAREN: u16 = 104;
pub const KIND_RPAREN: u16 = 105;
pub const KIND_LBRACKET: u16 = 106;
pub const KIND_RBRACKET: u16 = 107;
pub const KIND_LBRACE: u16 = 108;
pub const KIND_RBRACE: u16 = 109;
pub const KIND_COMMA: u16 = 110;
pub const KIND_COLON: u16 = 111;
pub const KIND_DOT: u16 = 112;
pub const KIND_PIPE: u16 = 113;
pub const KIND_ARROW: u16 = 114;
pub const KIND_ASSIGN: u16 = 115;
pub const KIND_EQ: u16 = 116;
pub const KIND_NE: u16 = 117;
pub const KIND_LT: u16 = 118;
pub const KIND_LE: u16 = 119;
pub const KIND_GT: u16 = 120;
pub const KIND_GE: u16 = 121;
pub const KIND_PLUS: u16 = 122;
pub const KIND_MINUS: u16 = 123;
pub const KIND_STAR: u16 = 124;
pub const KIND_SLASH: u16 = 125;
pub const KIND_PERCENT: u16 = 126;
pub const KIND_NEWLINE: u16 = 127;
pub const KIND_QUESTION: u16 = 128;
pub const KIND_WHITESPACE: u16 = 500;
pub const KIND_COMMENT: u16 = 501;
pub const KIND_BOM: u16 = 502;

pub fn syntax_kind_class(kind: u16) -> Option<SyntaxClass> {
    Some(match kind {
        KIND_MODULE | KIND_USE | KIND_INTERFACE | KIND_CLASS | KIND_ENUM | KIND_FUNCTION
        | KIND_STATEMENT => SyntaxClass::Node,
        KIND_INVALID => SyntaxClass::Invalid,
        KIND_INT..=KIND_QUESTION => SyntaxClass::Token,
        KIND_WHITESPACE | KIND_COMMENT | KIND_BOM => SyntaxClass::Trivia,
        _ => return None,
    })
}

pub fn syntax_kind_name(kind: u16) -> Option<&'static str> {
    Some(match kind {
        KIND_MODULE => "Module",
        KIND_USE => "UseDeclaration",
        KIND_INTERFACE => "InterfaceDeclaration",
        KIND_CLASS => "ClassDeclaration",
        KIND_ENUM => "EnumDeclaration",
        KIND_FUNCTION => "FunctionDeclaration",
        KIND_STATEMENT => "Statement",
        KIND_INVALID => "InvalidFragment",
        KIND_INT => "IntegerToken",
        KIND_STRING => "StringToken",
        KIND_IDENTIFIER => "IdentifierToken",
        KIND_KEYWORD => "KeywordToken",
        KIND_LPAREN => "LeftParenthesisToken",
        KIND_RPAREN => "RightParenthesisToken",
        KIND_LBRACKET => "LeftBracketToken",
        KIND_RBRACKET => "RightBracketToken",
        KIND_LBRACE => "LeftBraceToken",
        KIND_RBRACE => "RightBraceToken",
        KIND_COMMA => "CommaToken",
        KIND_COLON => "ColonToken",
        KIND_DOT => "DotToken",
        KIND_PIPE => "PipeToken",
        KIND_ARROW => "ArrowToken",
        KIND_ASSIGN => "AssignmentToken",
        KIND_EQ => "EqualToken",
        KIND_NE => "NotEqualToken",
        KIND_LT => "LessToken",
        KIND_LE => "LessEqualToken",
        KIND_GT => "GreaterToken",
        KIND_GE => "GreaterEqualToken",
        KIND_PLUS => "PlusToken",
        KIND_MINUS => "MinusToken",
        KIND_STAR => "StarToken",
        KIND_SLASH => "SlashToken",
        KIND_PERCENT => "PercentToken",
        KIND_QUESTION => "QuestionToken",
        KIND_NEWLINE => "NewlineToken",
        KIND_WHITESPACE => "WhitespaceTrivia",
        KIND_COMMENT => "CommentTrivia",
        KIND_BOM => "ByteOrderMarkTrivia",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(class: SyntaxClass, kind: u16, text: &str) -> Vec<u8> {
        build_syntax_leaf(class, kind, text).expect("the test leaf is valid")
    }

    #[test]
    fn structural_builder_preserves_text_and_children() {
        let integer = leaf(SyntaxClass::Token, KIND_INT, "40");
        let space = leaf(SyntaxClass::Trivia, KIND_WHITESPACE, " ");
        let plus = leaf(SyntaxClass::Token, KIND_PLUS, "+");
        let parts = [
            SyntaxPart {
                source: "40",
                records: &integer,
                index: 0,
            },
            SyntaxPart {
                source: " ",
                records: &space,
                index: 0,
            },
            SyntaxPart {
                source: "+",
                records: &plus,
                index: 0,
            },
        ];
        let built = build_syntax_node(KIND_STATEMENT, &parts).expect("the node is valid");
        assert_eq!(built.source, "40 +");
        let view = SyntaxView::new(&built.records, built.source.len()).expect("valid records");
        let root = view.record(view.root()).expect("the root exists");
        assert_eq!(root.kind, KIND_STATEMENT);
        assert_eq!(root.child_len, 3);
        let second = view
            .record(view.child(root, 1).expect("the child exists"))
            .expect("the child record exists");
        assert_eq!(second.class, SyntaxClass::Trivia);
        assert_eq!((second.lo, second.hi), (2, 3));
    }

    #[test]
    fn structural_builder_rejects_wrong_kind_classes() {
        assert_eq!(
            build_syntax_leaf(SyntaxClass::Token, KIND_WHITESPACE, " "),
            Err(SyntaxError::Kind)
        );
        assert_eq!(build_syntax_node(KIND_PLUS, &[]), Err(SyntaxError::Kind));
    }
}
