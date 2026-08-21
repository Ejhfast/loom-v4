//! Optional source and diagnostic metadata.
//!
//! This data stays outside the semantic and verification regions.

use crate::Module;

const MAGIC: &[u8; 4] = b"LMDB";
const VERSION: u16 = 2;

/// One source file retained by an artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugSource {
    pub path: String,
    pub text: String,
    pub syntax: Vec<u8>,
}

/// The target table used by one definition record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionKind {
    Function,
    Class,
}

/// One source-backed portable definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugDefinition {
    pub kind: DefinitionKind,
    pub target: u32,
    pub source: u32,
    pub lo: u32,
    pub hi: u32,
    pub syntax: u32,
    pub origin: [u8; 32],
}

/// One function-level source map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugFunction {
    pub function: u32,
    pub source: u32,
    pub lo: u32,
    pub hi: u32,
}

/// One source origin selected by a code reification instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugCodeOrigin {
    pub function: u32,
    pub block: u32,
    pub instruction: u32,
    pub origin: [u8; 32],
}

/// The optional debug section of one artifact.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DebugInfo {
    pub sources: Vec<DebugSource>,
    pub definitions: Vec<DebugDefinition>,
    pub functions: Vec<DebugFunction>,
    pub code_origins: Vec<DebugCodeOrigin>,
}

impl DebugInfo {
    /// Append metadata after table relocation.
    pub fn append_relocated(
        &mut self,
        other: &DebugInfo,
        functions: &[u32],
        classes: &[u32],
    ) -> Result<(), DebugError> {
        let source_base = u32::try_from(self.sources.len()).map_err(|_| DebugError::BadLength)?;
        self.sources.extend(other.sources.iter().cloned());
        for definition in &other.definitions {
            let target = match definition.kind {
                DefinitionKind::Function => functions.get(definition.target as usize),
                DefinitionKind::Class => classes.get(definition.target as usize),
            }
            .copied()
            .ok_or(DebugError::BadIndex)?;
            let source = source_base
                .checked_add(definition.source)
                .ok_or(DebugError::BadLength)?;
            self.definitions.push(DebugDefinition {
                kind: definition.kind,
                target,
                source,
                lo: definition.lo,
                hi: definition.hi,
                syntax: definition.syntax,
                origin: definition.origin,
            });
        }
        for function in &other.functions {
            let target = functions
                .get(function.function as usize)
                .copied()
                .ok_or(DebugError::BadIndex)?;
            let source = source_base
                .checked_add(function.source)
                .ok_or(DebugError::BadLength)?;
            self.functions.push(DebugFunction {
                function: target,
                source,
                lo: function.lo,
                hi: function.hi,
            });
        }
        for origin in &other.code_origins {
            let function = functions
                .get(origin.function as usize)
                .copied()
                .ok_or(DebugError::BadIndex)?;
            self.code_origins.push(DebugCodeOrigin {
                function,
                block: origin.block,
                instruction: origin.instruction,
                origin: origin.origin,
            });
        }
        Ok(())
    }
}

/// A malformed debug section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugError {
    BadMagic,
    BadVersion,
    BadLength,
    BadText,
    BadTag,
    BadIndex,
    BadRange,
    BadSyntax,
    NonCanonical,
    TrailingBytes,
}

impl std::fmt::Display for DebugError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            DebugError::BadMagic => "bad debug magic",
            DebugError::BadVersion => "unsupported debug version",
            DebugError::BadLength => "invalid debug length",
            DebugError::BadText => "invalid debug text",
            DebugError::BadTag => "invalid debug tag",
            DebugError::BadIndex => "invalid debug index",
            DebugError::BadRange => "invalid debug source range",
            DebugError::BadSyntax => "invalid debug syntax",
            DebugError::NonCanonical => "non-canonical debug data",
            DebugError::TrailingBytes => "trailing debug bytes",
        };
        f.write_str(text)
    }
}

/// Encode one debug section.
pub fn encode(info: &DebugInfo) -> Vec<u8> {
    if info.sources.is_empty()
        && info.definitions.is_empty()
        && info.functions.is_empty()
        && info.code_origins.is_empty()
    {
        return Vec::new();
    }
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    write_u32(&mut out, info.sources.len() as u32);
    for source in &info.sources {
        write_bytes(&mut out, source.path.as_bytes());
        write_bytes(&mut out, source.text.as_bytes());
        write_bytes(&mut out, &source.syntax);
    }
    write_u32(&mut out, info.definitions.len() as u32);
    for definition in &info.definitions {
        out.push(match definition.kind {
            DefinitionKind::Function => 0,
            DefinitionKind::Class => 1,
        });
        write_u32(&mut out, definition.target);
        write_u32(&mut out, definition.source);
        write_u32(&mut out, definition.lo);
        write_u32(&mut out, definition.hi);
        write_u32(&mut out, definition.syntax);
        out.extend_from_slice(&definition.origin);
    }
    write_u32(&mut out, info.functions.len() as u32);
    for function in &info.functions {
        write_u32(&mut out, function.function);
        write_u32(&mut out, function.source);
        write_u32(&mut out, function.lo);
        write_u32(&mut out, function.hi);
    }
    write_u32(&mut out, info.code_origins.len() as u32);
    for origin in &info.code_origins {
        write_u32(&mut out, origin.function);
        write_u32(&mut out, origin.block);
        write_u32(&mut out, origin.instruction);
        out.extend_from_slice(&origin.origin);
    }
    out
}

/// Decode one optional debug section.
pub fn decode(bytes: &[u8]) -> Result<DebugInfo, DebugError> {
    if bytes.is_empty() {
        return Ok(DebugInfo::default());
    }
    let mut cursor = Cursor { bytes, pos: 0 };
    if cursor.take(4)? != MAGIC {
        return Err(DebugError::BadMagic);
    }
    if cursor.u16()? != VERSION {
        return Err(DebugError::BadVersion);
    }
    let source_count = cursor.count(12)?;
    let mut sources = Vec::with_capacity(source_count);
    for _ in 0..source_count {
        sources.push(DebugSource {
            path: cursor.string()?,
            text: cursor.string()?,
            syntax: cursor.bytes()?.to_vec(),
        });
    }
    let definition_count = cursor.count(53)?;
    let mut definitions = Vec::with_capacity(definition_count);
    for _ in 0..definition_count {
        let kind = match cursor.u8()? {
            0 => DefinitionKind::Function,
            1 => DefinitionKind::Class,
            _ => return Err(DebugError::BadTag),
        };
        let target = cursor.u32()?;
        let source = cursor.u32()?;
        let lo = cursor.u32()?;
        let hi = cursor.u32()?;
        let syntax = cursor.u32()?;
        let mut origin = [0u8; 32];
        origin.copy_from_slice(cursor.take(32)?);
        definitions.push(DebugDefinition {
            kind,
            target,
            source,
            lo,
            hi,
            syntax,
            origin,
        });
    }
    let function_count = cursor.count(16)?;
    let mut functions = Vec::with_capacity(function_count);
    for _ in 0..function_count {
        functions.push(DebugFunction {
            function: cursor.u32()?,
            source: cursor.u32()?,
            lo: cursor.u32()?,
            hi: cursor.u32()?,
        });
    }
    let origin_count = cursor.count(44)?;
    let mut code_origins = Vec::with_capacity(origin_count);
    for _ in 0..origin_count {
        let function = cursor.u32()?;
        let block = cursor.u32()?;
        let instruction = cursor.u32()?;
        let mut origin = [0u8; 32];
        origin.copy_from_slice(cursor.take(32)?);
        code_origins.push(DebugCodeOrigin {
            function,
            block,
            instruction,
            origin,
        });
    }
    if cursor.pos != bytes.len() {
        return Err(DebugError::TrailingBytes);
    }
    Ok(DebugInfo {
        sources,
        definitions,
        functions,
        code_origins,
    })
}

/// Compute one stable source definition origin.
pub fn definition_origin(
    path: &str,
    text: &str,
    kind: DefinitionKind,
    lo: u32,
    hi: u32,
) -> Result<[u8; 32], DebugError> {
    if lo > hi {
        return Err(DebugError::BadRange);
    }
    let selected = text
        .get(lo as usize..hi as usize)
        .ok_or(DebugError::BadRange)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"loom-definition-origin-v1");
    write_bytes(&mut bytes, path.as_bytes());
    bytes.push(match kind {
        DefinitionKind::Function => 0,
        DefinitionKind::Class => 1,
    });
    write_u32(&mut bytes, lo);
    write_u32(&mut bytes, hi);
    write_bytes(&mut bytes, selected.as_bytes());
    Ok(crate::identity::container_hash(&bytes))
}

/// Validate metadata against its decoded module.
pub fn validate(info: &DebugInfo, module: &Module) -> Result<(), DebugError> {
    for source in &info.sources {
        lm_abi::syntax::SyntaxView::new(&source.syntax, source.text.len())
            .map_err(|_| DebugError::BadSyntax)?;
    }
    for definition in &info.definitions {
        let source = info
            .sources
            .get(definition.source as usize)
            .ok_or(DebugError::BadIndex)?;
        if definition.lo > definition.hi || definition.hi as usize > source.text.len() {
            return Err(DebugError::BadRange);
        }
        let view = lm_abi::syntax::SyntaxView::new(&source.syntax, source.text.len())
            .map_err(|_| DebugError::BadSyntax)?;
        let record = view
            .record(definition.syntax)
            .map_err(|_| DebugError::BadIndex)?;
        if record.lo != definition.lo || record.hi != definition.hi {
            return Err(DebugError::BadRange);
        }
        if definition.origin
            != definition_origin(
                &source.path,
                &source.text,
                definition.kind,
                definition.lo,
                definition.hi,
            )?
        {
            return Err(DebugError::BadIndex);
        }
        let valid_target = match definition.kind {
            DefinitionKind::Function => (definition.target as usize) < module.funcs.len(),
            DefinitionKind::Class => (definition.target as usize) < module.classes.len(),
        };
        if !valid_target {
            return Err(DebugError::BadIndex);
        }
    }
    for function in &info.functions {
        let source = info
            .sources
            .get(function.source as usize)
            .ok_or(DebugError::BadIndex)?;
        if function.function as usize >= module.funcs.len()
            || function.lo > function.hi
            || function.hi as usize > source.text.len()
        {
            return Err(DebugError::BadRange);
        }
    }
    for origin in &info.code_origins {
        let function = module
            .funcs
            .get(origin.function as usize)
            .ok_or(DebugError::BadIndex)?;
        let block = function
            .blocks
            .get(origin.block as usize)
            .ok_or(DebugError::BadIndex)?;
        let instruction = block
            .get(origin.instruction as usize)
            .ok_or(DebugError::BadIndex)?;
        let definition = info
            .definitions
            .iter()
            .find(|definition| definition.origin == origin.origin)
            .ok_or(DebugError::BadIndex)?;
        let matches = match (instruction, definition.kind) {
            (
                crate::Instr::Extended(crate::ExtendedInstr::FunctionCode { func }),
                DefinitionKind::Function,
            ) => *func == definition.target,
            (
                crate::Instr::Extended(crate::ExtendedInstr::ClassCode { class }),
                DefinitionKind::Class,
            ) => *class == definition.target,
            _ => false,
        };
        if !matches {
            return Err(DebugError::BadIndex);
        }
    }
    Ok(())
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    write_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], DebugError> {
        let end = self.pos.checked_add(len).ok_or(DebugError::BadLength)?;
        let value = self.bytes.get(self.pos..end).ok_or(DebugError::BadLength)?;
        self.pos = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, DebugError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, DebugError> {
        let mut value = [0u8; 2];
        value.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(value))
    }

    fn u32(&mut self) -> Result<u32, DebugError> {
        let mut value = [0u8; 4];
        value.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(value))
    }

    fn count(&mut self, item_bytes: usize) -> Result<usize, DebugError> {
        let count = self.u32()? as usize;
        if count > self.bytes.len().saturating_sub(self.pos) / item_bytes {
            return Err(DebugError::BadLength);
        }
        Ok(count)
    }

    fn bytes(&mut self) -> Result<&'a [u8], DebugError> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn string(&mut self) -> Result<String, DebugError> {
        let bytes = self.bytes()?;
        let text = std::str::from_utf8(bytes).map_err(|_| DebugError::BadText)?;
        Ok(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_data_round_trips() {
        let syntax = lm_abi::syntax::build_syntax_node(lm_abi::syntax::KIND_FUNCTION, &[])
            .expect("the syntax encodes")
            .records;
        let info = DebugInfo {
            sources: vec![DebugSource {
                path: "sample.lm".to_string(),
                text: "def f()\nend".to_string(),
                syntax,
            }],
            definitions: vec![DebugDefinition {
                kind: DefinitionKind::Function,
                target: 0,
                source: 0,
                lo: 0,
                hi: 11,
                syntax: 0,
                origin: definition_origin(
                    "sample.lm",
                    "def f()\nend",
                    DefinitionKind::Function,
                    0,
                    11,
                )
                .expect("the origin hashes"),
            }],
            functions: vec![DebugFunction {
                function: 0,
                source: 0,
                lo: 0,
                hi: 11,
            }],
            code_origins: Vec::new(),
        };
        assert_eq!(decode(&encode(&info)), Ok(info));
    }

    #[test]
    fn a_large_count_rejects_before_allocation() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&bytes), Err(DebugError::BadLength));
    }
}
