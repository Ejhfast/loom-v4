//! Bytecode formats for the week-1 language slice.
//!
//! This crate defines two forms:
//! - a compact serialized byte format for storage and transfer;
//! - a fixed-size decoded instruction form for the verifier and the VM.
//!
//! The decoder validates structure only. The independent verifier in
//! `lm-verify` validates types, jumps, calls, and stack shapes.

use std::fmt;

/// A primitive value type tag inside function signatures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimTy {
    Unit,
    Bool,
    Int,
    Str,
}

impl PrimTy {
    pub fn tag(self) -> u8 {
        match self {
            PrimTy::Unit => 0,
            PrimTy::Bool => 1,
            PrimTy::Int => 2,
            PrimTy::Str => 3,
        }
    }

    pub fn from_tag(tag: u8) -> Option<PrimTy> {
        match tag {
            0 => Some(PrimTy::Unit),
            1 => Some(PrimTy::Bool),
            2 => Some(PrimTy::Int),
            3 => Some(PrimTy::Str),
            _ => None,
        }
    }
}

impl fmt::Display for PrimTy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            PrimTy::Unit => "()",
            PrimTy::Bool => "Bool",
            PrimTy::Int => "Int",
            PrimTy::Str => "String",
        };
        f.write_str(name)
    }
}

/// One decoded instruction. The form is a fixed-size Rust enum.
///
/// Jump operands name a target basic block, not a raw byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instr {
    /// Push the unit value.
    ConstUnit,
    /// Push a Bool constant.
    ConstBool(bool),
    /// Push an Int constant.
    ConstInt(i64),
    /// Allocate the module string with this pool index and push it.
    ConstStr(u32),
    /// Push the value of a local slot.
    LoadLocal(u32),
    /// Pop one value into a local slot.
    StoreLocal(u32),
    /// Pop and discard one value.
    Pop,
    /// Checked Int add. Overflow faults.
    Add,
    /// Checked Int subtract. Overflow faults.
    Sub,
    /// Checked Int multiply. Overflow faults.
    Mul,
    /// Int division that truncates toward zero. Zero divisor faults.
    Div,
    /// Int remainder with the dividend sign. Zero divisor faults.
    Rem,
    /// Checked Int negation. Overflow faults.
    Neg,
    /// Bool negation.
    Not,
    LtInt,
    LeInt,
    GtInt,
    GeInt,
    EqInt,
    NeInt,
    EqBool,
    NeBool,
    EqStr,
    NeStr,
    /// Direct call of a function by table index.
    Call(u32),
    /// Unconditional jump to a block. Ends the block.
    Jump(u32),
    /// Pop a Bool. Jump to the block when the value is false.
    JumpIfFalse(u32),
    /// Pop a Bool. Jump to the block when the value is true.
    JumpIfTrue(u32),
    /// Pop the result value and return it. Ends the block.
    Return,
}

impl Instr {
    /// Return true when the instruction ends a basic block.
    pub fn is_terminator(&self) -> bool {
        matches!(self, Instr::Jump(_) | Instr::Return)
    }
}

/// One function body as basic blocks of decoded instructions.
#[derive(Debug, Clone, PartialEq)]
pub struct Func {
    pub name: String,
    pub params: Vec<PrimTy>,
    pub ret: PrimTy,
    /// Total local slot count. Parameters use the first slots.
    pub local_count: u32,
    pub blocks: Vec<Vec<Instr>>,
}

/// One decoded module.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub strings: Vec<String>,
    pub funcs: Vec<Func>,
    /// Index of the entry function.
    pub entry: u32,
}

const MAGIC: &[u8; 4] = b"LMBC";
const VERSION: u16 = 1;

// Opcode bytes for the serialized form.
const OP_CONST_UNIT: u8 = 0x00;
const OP_CONST_BOOL: u8 = 0x01;
const OP_CONST_INT: u8 = 0x02;
const OP_CONST_STR: u8 = 0x03;
const OP_LOAD_LOCAL: u8 = 0x04;
const OP_STORE_LOCAL: u8 = 0x05;
const OP_POP: u8 = 0x06;
const OP_ADD: u8 = 0x10;
const OP_SUB: u8 = 0x11;
const OP_MUL: u8 = 0x12;
const OP_DIV: u8 = 0x13;
const OP_REM: u8 = 0x14;
const OP_NEG: u8 = 0x15;
const OP_NOT: u8 = 0x16;
const OP_LT_INT: u8 = 0x20;
const OP_LE_INT: u8 = 0x21;
const OP_GT_INT: u8 = 0x22;
const OP_GE_INT: u8 = 0x23;
const OP_EQ_INT: u8 = 0x24;
const OP_NE_INT: u8 = 0x25;
const OP_EQ_BOOL: u8 = 0x26;
const OP_NE_BOOL: u8 = 0x27;
const OP_EQ_STR: u8 = 0x28;
const OP_NE_STR: u8 = 0x29;
const OP_CALL: u8 = 0x30;
const OP_JUMP: u8 = 0x31;
const OP_JUMP_IF_FALSE: u8 = 0x32;
const OP_JUMP_IF_TRUE: u8 = 0x33;
const OP_RETURN: u8 = 0x34;

/// Encode a module into the compact serialized form.
pub fn encode(module: &Module) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    write_u32(&mut out, module.strings.len() as u32);
    for s in &module.strings {
        write_bytes(&mut out, s.as_bytes());
    }
    write_u32(&mut out, module.funcs.len() as u32);
    for func in &module.funcs {
        write_bytes(&mut out, func.name.as_bytes());
        out.push(func.params.len() as u8);
        for p in &func.params {
            out.push(p.tag());
        }
        out.push(func.ret.tag());
        write_u32(&mut out, func.local_count);
        write_u32(&mut out, func.blocks.len() as u32);
        for block in &func.blocks {
            write_u32(&mut out, block.len() as u32);
            for instr in block {
                encode_instr(&mut out, instr);
            }
        }
    }
    write_u32(&mut out, module.entry);
    out
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    write_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

fn encode_instr(out: &mut Vec<u8>, instr: &Instr) {
    match instr {
        Instr::ConstUnit => out.push(OP_CONST_UNIT),
        Instr::ConstBool(v) => {
            out.push(OP_CONST_BOOL);
            out.push(u8::from(*v));
        }
        Instr::ConstInt(v) => {
            out.push(OP_CONST_INT);
            out.extend_from_slice(&v.to_le_bytes());
        }
        Instr::ConstStr(idx) => {
            out.push(OP_CONST_STR);
            write_u32(out, *idx);
        }
        Instr::LoadLocal(slot) => {
            out.push(OP_LOAD_LOCAL);
            write_u32(out, *slot);
        }
        Instr::StoreLocal(slot) => {
            out.push(OP_STORE_LOCAL);
            write_u32(out, *slot);
        }
        Instr::Pop => out.push(OP_POP),
        Instr::Add => out.push(OP_ADD),
        Instr::Sub => out.push(OP_SUB),
        Instr::Mul => out.push(OP_MUL),
        Instr::Div => out.push(OP_DIV),
        Instr::Rem => out.push(OP_REM),
        Instr::Neg => out.push(OP_NEG),
        Instr::Not => out.push(OP_NOT),
        Instr::LtInt => out.push(OP_LT_INT),
        Instr::LeInt => out.push(OP_LE_INT),
        Instr::GtInt => out.push(OP_GT_INT),
        Instr::GeInt => out.push(OP_GE_INT),
        Instr::EqInt => out.push(OP_EQ_INT),
        Instr::NeInt => out.push(OP_NE_INT),
        Instr::EqBool => out.push(OP_EQ_BOOL),
        Instr::NeBool => out.push(OP_NE_BOOL),
        Instr::EqStr => out.push(OP_EQ_STR),
        Instr::NeStr => out.push(OP_NE_STR),
        Instr::Call(idx) => {
            out.push(OP_CALL);
            write_u32(out, *idx);
        }
        Instr::Jump(block) => {
            out.push(OP_JUMP);
            write_u32(out, *block);
        }
        Instr::JumpIfFalse(block) => {
            out.push(OP_JUMP_IF_FALSE);
            write_u32(out, *block);
        }
        Instr::JumpIfTrue(block) => {
            out.push(OP_JUMP_IF_TRUE);
            write_u32(out, *block);
        }
        Instr::Return => out.push(OP_RETURN),
    }
}

/// A structural decode failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The stream ended before the structure was complete.
    Truncated,
    BadMagic,
    BadVersion(u16),
    BadOpcode(u8),
    BadTypeTag(u8),
    BadUtf8,
    /// Extra bytes follow the encoded module.
    TrailingBytes,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Truncated => write!(f, "the byte stream is truncated"),
            DecodeError::BadMagic => write!(f, "the magic header is not `LMBC`"),
            DecodeError::BadVersion(v) => write!(f, "unsupported bytecode version {v}"),
            DecodeError::BadOpcode(op) => write!(f, "unknown opcode byte 0x{op:02x}"),
            DecodeError::BadTypeTag(t) => write!(f, "unknown type tag {t}"),
            DecodeError::BadUtf8 => write!(f, "a string is not valid UTF-8"),
            DecodeError::TrailingBytes => write!(f, "extra bytes follow the module"),
        }
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(len).ok_or(DecodeError::Truncated)?;
        if end > self.bytes.len() {
            return Err(DecodeError::Truncated);
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i64(&mut self) -> Result<i64, DecodeError> {
        let b = self.take(8)?;
        let mut buf = [0u8; 8];
        buf.copy_from_slice(b);
        Ok(i64::from_le_bytes(buf))
    }

    fn string(&mut self) -> Result<String, DecodeError> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::BadUtf8)
    }
}

/// Decode a serialized module. This checks structure only.
pub fn decode(bytes: &[u8]) -> Result<Module, DecodeError> {
    let mut cur = Cursor { bytes, pos: 0 };
    if cur.take(4)? != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let version = cur.u16()?;
    if version != VERSION {
        return Err(DecodeError::BadVersion(version));
    }
    let string_count = cur.u32()?;
    let mut strings = Vec::new();
    for _ in 0..string_count {
        strings.push(cur.string()?);
    }
    let func_count = cur.u32()?;
    let mut funcs = Vec::new();
    for _ in 0..func_count {
        let name = cur.string()?;
        let param_count = cur.u8()?;
        let mut params = Vec::new();
        for _ in 0..param_count {
            let tag = cur.u8()?;
            params.push(PrimTy::from_tag(tag).ok_or(DecodeError::BadTypeTag(tag))?);
        }
        let ret_tag = cur.u8()?;
        let ret = PrimTy::from_tag(ret_tag).ok_or(DecodeError::BadTypeTag(ret_tag))?;
        let local_count = cur.u32()?;
        let block_count = cur.u32()?;
        let mut blocks = Vec::new();
        for _ in 0..block_count {
            let instr_count = cur.u32()?;
            let mut block = Vec::new();
            for _ in 0..instr_count {
                block.push(decode_instr(&mut cur)?);
            }
            blocks.push(block);
        }
        funcs.push(Func {
            name,
            params,
            ret,
            local_count,
            blocks,
        });
    }
    let entry = cur.u32()?;
    if cur.pos != bytes.len() {
        return Err(DecodeError::TrailingBytes);
    }
    Ok(Module {
        strings,
        funcs,
        entry,
    })
}

fn decode_instr(cur: &mut Cursor<'_>) -> Result<Instr, DecodeError> {
    let op = cur.u8()?;
    let instr = match op {
        OP_CONST_UNIT => Instr::ConstUnit,
        OP_CONST_BOOL => Instr::ConstBool(cur.u8()? != 0),
        OP_CONST_INT => Instr::ConstInt(cur.i64()?),
        OP_CONST_STR => Instr::ConstStr(cur.u32()?),
        OP_LOAD_LOCAL => Instr::LoadLocal(cur.u32()?),
        OP_STORE_LOCAL => Instr::StoreLocal(cur.u32()?),
        OP_POP => Instr::Pop,
        OP_ADD => Instr::Add,
        OP_SUB => Instr::Sub,
        OP_MUL => Instr::Mul,
        OP_DIV => Instr::Div,
        OP_REM => Instr::Rem,
        OP_NEG => Instr::Neg,
        OP_NOT => Instr::Not,
        OP_LT_INT => Instr::LtInt,
        OP_LE_INT => Instr::LeInt,
        OP_GT_INT => Instr::GtInt,
        OP_GE_INT => Instr::GeInt,
        OP_EQ_INT => Instr::EqInt,
        OP_NE_INT => Instr::NeInt,
        OP_EQ_BOOL => Instr::EqBool,
        OP_NE_BOOL => Instr::NeBool,
        OP_EQ_STR => Instr::EqStr,
        OP_NE_STR => Instr::NeStr,
        OP_CALL => Instr::Call(cur.u32()?),
        OP_JUMP => Instr::Jump(cur.u32()?),
        OP_JUMP_IF_FALSE => Instr::JumpIfFalse(cur.u32()?),
        OP_JUMP_IF_TRUE => Instr::JumpIfTrue(cur.u32()?),
        OP_RETURN => Instr::Return,
        other => return Err(DecodeError::BadOpcode(other)),
    };
    Ok(instr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_module() -> Module {
        Module {
            strings: vec!["hello".to_string()],
            funcs: vec![Func {
                name: "main".to_string(),
                params: vec![],
                ret: PrimTy::Int,
                local_count: 1,
                blocks: vec![vec![
                    Instr::ConstInt(41),
                    Instr::ConstInt(1),
                    Instr::Add,
                    Instr::Return,
                ]],
            }],
            entry: 0,
        }
    }

    #[test]
    fn decoded_instruction_form_is_fixed_size() {
        assert_eq!(std::mem::size_of::<Instr>(), 16);
    }

    #[test]
    fn encode_decode_round_trip() {
        let module = sample_module();
        let bytes = encode(&module);
        assert_eq!(decode(&bytes).unwrap(), module);
    }

    #[test]
    fn encode_is_deterministic() {
        assert_eq!(encode(&sample_module()), encode(&sample_module()));
    }

    #[test]
    fn every_truncation_is_rejected() {
        let bytes = encode(&sample_module());
        for len in 0..bytes.len() {
            let result = decode(&bytes[..len]);
            assert!(result.is_err(), "prefix length {len} was accepted");
        }
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut bytes = encode(&sample_module());
        bytes[0] = b'X';
        assert_eq!(decode(&bytes), Err(DecodeError::BadMagic));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = encode(&sample_module());
        bytes.push(0);
        assert_eq!(decode(&bytes), Err(DecodeError::TrailingBytes));
    }

    #[test]
    fn bad_opcode_is_rejected() {
        let module = sample_module();
        let bytes = encode(&module);
        // Replace the `Add` opcode with an unknown opcode byte.
        let mut corrupt = bytes.clone();
        let pos = corrupt.len() - 4 /* entry */ - 1 /* return */ - 1 /* add */;
        assert_eq!(corrupt[pos], 0x10);
        corrupt[pos] = 0xff;
        assert_eq!(decode(&corrupt), Err(DecodeError::BadOpcode(0xff)));
    }
}
