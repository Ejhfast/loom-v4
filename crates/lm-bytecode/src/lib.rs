//! Bytecode formats for the week-2 language slice.
//!
//! This crate defines two forms:
//! - a compact serialized byte format for storage and transfer;
//! - a fixed-size decoded instruction form for the verifier and the VM.
//!
//! The decoder validates structure only. The independent verifier in
//! `lm-verify` validates tables, types, jumps, calls, and stack shapes.

use std::fmt;

/// The sentinel that encodes "no parent class".
pub const NO_PARENT: u32 = u32::MAX;

/// One entry in the module type table.
///
/// Types reference other types by index. A canonical table only
/// references earlier entries and holds no duplicate entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BcType {
    Unit,
    Bool,
    Int,
    Str,
    /// A class instance type. The index names a class-table entry.
    Class(u32),
    /// A list type with one element type index.
    List(u32),
    /// A map type with a key type index and a value type index.
    Map(u32, u32),
    /// A function value type with parameter type indices and a result.
    Fn(Vec<u32>, u32),
    StringBuilder,
    ByteBuffer,
}

/// One class-table entry. Fields hold the full layout: inherited
/// fields first, own fields after them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BcClass {
    pub name: String,
    /// Parent class index, or `NO_PARENT`.
    pub parent: u32,
    /// Full field layout: `(name, type index)`.
    pub fields: Vec<(String, u32)>,
    /// Own method table: `(selector index, function index)`.
    pub methods: Vec<(u32, u32)>,
}

impl BcClass {
    pub fn parent(&self) -> Option<u32> {
        if self.parent == NO_PARENT {
            None
        } else {
            Some(self.parent)
        }
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
    /// String equality by content.
    EqStr,
    NeStr,
    /// Reference identity equality for heap objects.
    EqRef,
    NeRef,
    /// Direct call of a function by table index.
    Call(u32),
    /// Virtual call: pop `argc` arguments over one receiver, select
    /// the method through the runtime class and the selector slot.
    CallVirtual {
        selector: u32,
        argc: u32,
    },
    /// Call a closure value: pop `argc` arguments over the closure.
    CallValue {
        argc: u32,
    },
    /// Allocate a closure over `captures` popped values for `func`.
    MakeClosure {
        func: u32,
        captures: u32,
    },
    /// Push one captured value of the active closure.
    LoadCapture(u32),
    /// Allocate an instance of a class. Fields start without a value.
    New(u32),
    /// Pop an instance and push one field value.
    LoadField(u32),
    /// Pop a value and an instance, then write the field.
    StoreField(u32),
    /// Allocate a list of the given list type from `count` popped values.
    ListNew {
        ty: u32,
        count: u32,
    },
    /// Pop a list and push its length.
    ListLen,
    /// Pop an index and a list, then push the element. Faults
    /// `IndexOutOfBounds` outside the range.
    ListAt,
    /// Pop a value and a list, then append. Pushes unit.
    ListPush,
    /// Allocate a map of the given map type from `count` popped pairs.
    MapNew {
        ty: u32,
        count: u32,
    },
    /// Pop a map and push its entry count.
    MapLen,
    /// Pop a key and a map, then push whether the key exists.
    MapHas,
    /// Pop a key and a map, then push the value. Faults `MissingKey`.
    MapAt,
    /// Pop a value, a key, and a map, then insert or replace. Pushes unit.
    MapPut,
    /// Allocate an empty string builder.
    SbNew,
    /// Pop a string and a builder, append, and push the builder.
    SbAppendStr,
    /// Pop an Int and a builder, append its decimal text, push the builder.
    SbAppendInt,
    /// Pop a Bool and a builder, append its text, push the builder.
    SbAppendBool,
    /// Pop a builder and push its content as a new string.
    SbBuild,
    /// Allocate an empty byte buffer.
    BbNew,
    /// Pop an Int and a buffer, append one byte, push the buffer.
    BbAppend,
    /// Pop a buffer and push its byte length.
    BbLen,
    /// Pop a buffer, decode UTF-8, and push a string. Faults `BadCast`.
    BbBuild,
    /// Pop an object reference, freeze its graph, push the same reference.
    Freeze,
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
    /// Parameter types as type-table indices.
    pub params: Vec<u32>,
    /// Result type as a type-table index.
    pub ret: u32,
    /// Capture types as type-table indices. Only a closure body has
    /// captures. A direct or virtual call target must have none.
    pub captures: Vec<u32>,
    /// Total local slot count. Parameters use the first slots.
    pub local_count: u32,
    pub blocks: Vec<Vec<Instr>>,
}

/// One decoded module.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub strings: Vec<String>,
    pub types: Vec<BcType>,
    /// Global selector names in first-encounter order.
    pub selectors: Vec<String>,
    pub classes: Vec<BcClass>,
    pub funcs: Vec<Func>,
    /// Index of the entry function.
    pub entry: u32,
}

const MAGIC: &[u8; 4] = b"LMBC";
const VERSION: u16 = 2;

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
const OP_EQ_REF: u8 = 0x2a;
const OP_NE_REF: u8 = 0x2b;
const OP_CALL: u8 = 0x30;
const OP_JUMP: u8 = 0x31;
const OP_JUMP_IF_FALSE: u8 = 0x32;
const OP_JUMP_IF_TRUE: u8 = 0x33;
const OP_RETURN: u8 = 0x34;
const OP_CALL_VIRTUAL: u8 = 0x40;
const OP_CALL_VALUE: u8 = 0x41;
const OP_MAKE_CLOSURE: u8 = 0x42;
const OP_LOAD_CAPTURE: u8 = 0x43;
const OP_NEW: u8 = 0x44;
const OP_LOAD_FIELD: u8 = 0x45;
const OP_STORE_FIELD: u8 = 0x46;
const OP_LIST_NEW: u8 = 0x47;
const OP_LIST_LEN: u8 = 0x48;
const OP_LIST_AT: u8 = 0x49;
const OP_LIST_PUSH: u8 = 0x4a;
const OP_MAP_NEW: u8 = 0x4b;
const OP_MAP_LEN: u8 = 0x4c;
const OP_MAP_HAS: u8 = 0x4d;
const OP_MAP_AT: u8 = 0x4e;
const OP_MAP_PUT: u8 = 0x4f;
const OP_SB_NEW: u8 = 0x50;
const OP_SB_APPEND_STR: u8 = 0x51;
const OP_SB_APPEND_INT: u8 = 0x52;
const OP_SB_APPEND_BOOL: u8 = 0x53;
const OP_SB_BUILD: u8 = 0x54;
const OP_BB_NEW: u8 = 0x55;
const OP_BB_APPEND: u8 = 0x56;
const OP_BB_LEN: u8 = 0x57;
const OP_BB_BUILD: u8 = 0x58;
const OP_FREEZE: u8 = 0x59;

// Type tags for the serialized type table.
const TY_UNIT: u8 = 0;
const TY_BOOL: u8 = 1;
const TY_INT: u8 = 2;
const TY_STR: u8 = 3;
const TY_CLASS: u8 = 4;
const TY_LIST: u8 = 5;
const TY_MAP: u8 = 6;
const TY_FN: u8 = 7;
const TY_SB: u8 = 8;
const TY_BB: u8 = 9;

/// Encode a module into the compact serialized form.
pub fn encode(module: &Module) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    write_u32(&mut out, module.strings.len() as u32);
    for s in &module.strings {
        write_bytes(&mut out, s.as_bytes());
    }
    write_u32(&mut out, module.types.len() as u32);
    for ty in &module.types {
        encode_type(&mut out, ty);
    }
    write_u32(&mut out, module.selectors.len() as u32);
    for s in &module.selectors {
        write_bytes(&mut out, s.as_bytes());
    }
    write_u32(&mut out, module.classes.len() as u32);
    for class in &module.classes {
        write_bytes(&mut out, class.name.as_bytes());
        write_u32(&mut out, class.parent);
        write_u32(&mut out, class.fields.len() as u32);
        for (name, ty) in &class.fields {
            write_bytes(&mut out, name.as_bytes());
            write_u32(&mut out, *ty);
        }
        write_u32(&mut out, class.methods.len() as u32);
        for (sel, func) in &class.methods {
            write_u32(&mut out, *sel);
            write_u32(&mut out, *func);
        }
    }
    write_u32(&mut out, module.funcs.len() as u32);
    for func in &module.funcs {
        write_bytes(&mut out, func.name.as_bytes());
        write_u32(&mut out, func.params.len() as u32);
        for p in &func.params {
            write_u32(&mut out, *p);
        }
        write_u32(&mut out, func.ret);
        write_u32(&mut out, func.captures.len() as u32);
        for c in &func.captures {
            write_u32(&mut out, *c);
        }
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

fn encode_type(out: &mut Vec<u8>, ty: &BcType) {
    match ty {
        BcType::Unit => out.push(TY_UNIT),
        BcType::Bool => out.push(TY_BOOL),
        BcType::Int => out.push(TY_INT),
        BcType::Str => out.push(TY_STR),
        BcType::Class(c) => {
            out.push(TY_CLASS);
            write_u32(out, *c);
        }
        BcType::List(e) => {
            out.push(TY_LIST);
            write_u32(out, *e);
        }
        BcType::Map(k, v) => {
            out.push(TY_MAP);
            write_u32(out, *k);
            write_u32(out, *v);
        }
        BcType::Fn(params, ret) => {
            out.push(TY_FN);
            write_u32(out, params.len() as u32);
            for p in params {
                write_u32(out, *p);
            }
            write_u32(out, *ret);
        }
        BcType::StringBuilder => out.push(TY_SB),
        BcType::ByteBuffer => out.push(TY_BB),
    }
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
        Instr::EqRef => out.push(OP_EQ_REF),
        Instr::NeRef => out.push(OP_NE_REF),
        Instr::Call(idx) => {
            out.push(OP_CALL);
            write_u32(out, *idx);
        }
        Instr::CallVirtual { selector, argc } => {
            out.push(OP_CALL_VIRTUAL);
            write_u32(out, *selector);
            write_u32(out, *argc);
        }
        Instr::CallValue { argc } => {
            out.push(OP_CALL_VALUE);
            write_u32(out, *argc);
        }
        Instr::MakeClosure { func, captures } => {
            out.push(OP_MAKE_CLOSURE);
            write_u32(out, *func);
            write_u32(out, *captures);
        }
        Instr::LoadCapture(idx) => {
            out.push(OP_LOAD_CAPTURE);
            write_u32(out, *idx);
        }
        Instr::New(class) => {
            out.push(OP_NEW);
            write_u32(out, *class);
        }
        Instr::LoadField(field) => {
            out.push(OP_LOAD_FIELD);
            write_u32(out, *field);
        }
        Instr::StoreField(field) => {
            out.push(OP_STORE_FIELD);
            write_u32(out, *field);
        }
        Instr::ListNew { ty, count } => {
            out.push(OP_LIST_NEW);
            write_u32(out, *ty);
            write_u32(out, *count);
        }
        Instr::ListLen => out.push(OP_LIST_LEN),
        Instr::ListAt => out.push(OP_LIST_AT),
        Instr::ListPush => out.push(OP_LIST_PUSH),
        Instr::MapNew { ty, count } => {
            out.push(OP_MAP_NEW);
            write_u32(out, *ty);
            write_u32(out, *count);
        }
        Instr::MapLen => out.push(OP_MAP_LEN),
        Instr::MapHas => out.push(OP_MAP_HAS),
        Instr::MapAt => out.push(OP_MAP_AT),
        Instr::MapPut => out.push(OP_MAP_PUT),
        Instr::SbNew => out.push(OP_SB_NEW),
        Instr::SbAppendStr => out.push(OP_SB_APPEND_STR),
        Instr::SbAppendInt => out.push(OP_SB_APPEND_INT),
        Instr::SbAppendBool => out.push(OP_SB_APPEND_BOOL),
        Instr::SbBuild => out.push(OP_SB_BUILD),
        Instr::BbNew => out.push(OP_BB_NEW),
        Instr::BbAppend => out.push(OP_BB_APPEND),
        Instr::BbLen => out.push(OP_BB_LEN),
        Instr::BbBuild => out.push(OP_BB_BUILD),
        Instr::Freeze => out.push(OP_FREEZE),
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
    /// A table length field is larger than the remaining input allows.
    BadLength,
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
            DecodeError::BadLength => write!(f, "a length field exceeds the input size"),
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

    /// Read a table length and reject counts the input cannot contain.
    /// Each counted element needs at least one byte.
    fn len(&mut self) -> Result<usize, DecodeError> {
        let count = self.u32()? as usize;
        if count > self.bytes.len().saturating_sub(self.pos) {
            return Err(DecodeError::BadLength);
        }
        Ok(count)
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
    let string_count = cur.len()?;
    let mut strings = Vec::with_capacity(string_count);
    for _ in 0..string_count {
        strings.push(cur.string()?);
    }
    let type_count = cur.len()?;
    let mut types = Vec::with_capacity(type_count);
    for _ in 0..type_count {
        types.push(decode_type(&mut cur)?);
    }
    let selector_count = cur.len()?;
    let mut selectors = Vec::with_capacity(selector_count);
    for _ in 0..selector_count {
        selectors.push(cur.string()?);
    }
    let class_count = cur.len()?;
    let mut classes = Vec::with_capacity(class_count);
    for _ in 0..class_count {
        let name = cur.string()?;
        let parent = cur.u32()?;
        let field_count = cur.len()?;
        let mut fields = Vec::with_capacity(field_count);
        for _ in 0..field_count {
            let fname = cur.string()?;
            let fty = cur.u32()?;
            fields.push((fname, fty));
        }
        let method_count = cur.len()?;
        let mut methods = Vec::with_capacity(method_count);
        for _ in 0..method_count {
            let sel = cur.u32()?;
            let func = cur.u32()?;
            methods.push((sel, func));
        }
        classes.push(BcClass {
            name,
            parent,
            fields,
            methods,
        });
    }
    let func_count = cur.len()?;
    let mut funcs = Vec::with_capacity(func_count);
    for _ in 0..func_count {
        let name = cur.string()?;
        let param_count = cur.len()?;
        let mut params = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            params.push(cur.u32()?);
        }
        let ret = cur.u32()?;
        let capture_count = cur.len()?;
        let mut captures = Vec::with_capacity(capture_count);
        for _ in 0..capture_count {
            captures.push(cur.u32()?);
        }
        let local_count = cur.u32()?;
        let block_count = cur.len()?;
        let mut blocks = Vec::with_capacity(block_count);
        for _ in 0..block_count {
            let instr_count = cur.len()?;
            let mut block = Vec::with_capacity(instr_count);
            for _ in 0..instr_count {
                block.push(decode_instr(&mut cur)?);
            }
            blocks.push(block);
        }
        funcs.push(Func {
            name,
            params,
            ret,
            captures,
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
        types,
        selectors,
        classes,
        funcs,
        entry,
    })
}

fn decode_type(cur: &mut Cursor<'_>) -> Result<BcType, DecodeError> {
    let tag = cur.u8()?;
    let ty = match tag {
        TY_UNIT => BcType::Unit,
        TY_BOOL => BcType::Bool,
        TY_INT => BcType::Int,
        TY_STR => BcType::Str,
        TY_CLASS => BcType::Class(cur.u32()?),
        TY_LIST => BcType::List(cur.u32()?),
        TY_MAP => BcType::Map(cur.u32()?, cur.u32()?),
        TY_FN => {
            let count = cur.len()?;
            let mut params = Vec::with_capacity(count);
            for _ in 0..count {
                params.push(cur.u32()?);
            }
            let ret = cur.u32()?;
            BcType::Fn(params, ret)
        }
        TY_SB => BcType::StringBuilder,
        TY_BB => BcType::ByteBuffer,
        other => return Err(DecodeError::BadTypeTag(other)),
    };
    Ok(ty)
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
        OP_EQ_REF => Instr::EqRef,
        OP_NE_REF => Instr::NeRef,
        OP_CALL => Instr::Call(cur.u32()?),
        OP_CALL_VIRTUAL => Instr::CallVirtual {
            selector: cur.u32()?,
            argc: cur.u32()?,
        },
        OP_CALL_VALUE => Instr::CallValue { argc: cur.u32()? },
        OP_MAKE_CLOSURE => Instr::MakeClosure {
            func: cur.u32()?,
            captures: cur.u32()?,
        },
        OP_LOAD_CAPTURE => Instr::LoadCapture(cur.u32()?),
        OP_NEW => Instr::New(cur.u32()?),
        OP_LOAD_FIELD => Instr::LoadField(cur.u32()?),
        OP_STORE_FIELD => Instr::StoreField(cur.u32()?),
        OP_LIST_NEW => Instr::ListNew {
            ty: cur.u32()?,
            count: cur.u32()?,
        },
        OP_LIST_LEN => Instr::ListLen,
        OP_LIST_AT => Instr::ListAt,
        OP_LIST_PUSH => Instr::ListPush,
        OP_MAP_NEW => Instr::MapNew {
            ty: cur.u32()?,
            count: cur.u32()?,
        },
        OP_MAP_LEN => Instr::MapLen,
        OP_MAP_HAS => Instr::MapHas,
        OP_MAP_AT => Instr::MapAt,
        OP_MAP_PUT => Instr::MapPut,
        OP_SB_NEW => Instr::SbNew,
        OP_SB_APPEND_STR => Instr::SbAppendStr,
        OP_SB_APPEND_INT => Instr::SbAppendInt,
        OP_SB_APPEND_BOOL => Instr::SbAppendBool,
        OP_SB_BUILD => Instr::SbBuild,
        OP_BB_NEW => Instr::BbNew,
        OP_BB_APPEND => Instr::BbAppend,
        OP_BB_LEN => Instr::BbLen,
        OP_BB_BUILD => Instr::BbBuild,
        OP_FREEZE => Instr::Freeze,
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
            types: vec![
                BcType::Unit,
                BcType::Int,
                BcType::Str,
                BcType::Class(0),
                BcType::List(1),
                BcType::Map(2, 1),
                BcType::Fn(vec![1], 1),
                BcType::StringBuilder,
                BcType::ByteBuffer,
            ],
            selectors: vec!["add".to_string()],
            classes: vec![BcClass {
                name: "Counter".to_string(),
                parent: NO_PARENT,
                fields: vec![("value".to_string(), 1)],
                methods: vec![(0, 1)],
            }],
            funcs: vec![
                Func {
                    name: "main".to_string(),
                    params: vec![],
                    ret: 1,
                    captures: vec![],
                    local_count: 1,
                    blocks: vec![vec![
                        Instr::ConstInt(41),
                        Instr::ConstInt(1),
                        Instr::Add,
                        Instr::Return,
                    ]],
                },
                Func {
                    name: "add".to_string(),
                    params: vec![3, 1],
                    ret: 1,
                    captures: vec![],
                    local_count: 2,
                    blocks: vec![vec![Instr::LoadLocal(1), Instr::Return]],
                },
            ],
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
    fn round_trips_every_instruction() {
        let instrs = vec![
            Instr::ConstUnit,
            Instr::ConstBool(true),
            Instr::ConstInt(-5),
            Instr::ConstStr(0),
            Instr::LoadLocal(1),
            Instr::StoreLocal(1),
            Instr::Pop,
            Instr::Add,
            Instr::EqStr,
            Instr::EqRef,
            Instr::NeRef,
            Instr::Call(0),
            Instr::CallVirtual {
                selector: 0,
                argc: 1,
            },
            Instr::CallValue { argc: 2 },
            Instr::MakeClosure {
                func: 1,
                captures: 0,
            },
            Instr::LoadCapture(0),
            Instr::New(0),
            Instr::LoadField(0),
            Instr::StoreField(0),
            Instr::ListNew { ty: 4, count: 2 },
            Instr::ListLen,
            Instr::ListAt,
            Instr::ListPush,
            Instr::MapNew { ty: 5, count: 1 },
            Instr::MapLen,
            Instr::MapHas,
            Instr::MapAt,
            Instr::MapPut,
            Instr::SbNew,
            Instr::SbAppendStr,
            Instr::SbAppendInt,
            Instr::SbAppendBool,
            Instr::SbBuild,
            Instr::BbNew,
            Instr::BbAppend,
            Instr::BbLen,
            Instr::BbBuild,
            Instr::Freeze,
            Instr::Jump(0),
            Instr::JumpIfFalse(0),
            Instr::JumpIfTrue(0),
            Instr::Return,
        ];
        let mut module = sample_module();
        module.funcs[0].blocks = vec![instrs.clone()];
        let bytes = encode(&module);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.funcs[0].blocks[0], instrs);
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
    fn old_version_is_rejected() {
        let mut bytes = encode(&sample_module());
        bytes[4] = 1;
        bytes[5] = 0;
        assert_eq!(decode(&bytes), Err(DecodeError::BadVersion(1)));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = encode(&sample_module());
        bytes.push(0);
        assert_eq!(decode(&bytes), Err(DecodeError::TrailingBytes));
    }

    #[test]
    fn huge_length_field_is_rejected() {
        let mut bytes = encode(&sample_module());
        // The string count is at offset 6.
        bytes[6..10].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&bytes), Err(DecodeError::BadLength));
    }

    #[test]
    fn bad_type_tag_is_rejected() {
        let module = Module {
            strings: vec![],
            types: vec![BcType::Unit],
            selectors: vec![],
            classes: vec![],
            funcs: vec![],
            entry: 0,
        };
        let mut bytes = encode(&module);
        // The single type tag sits directly after the type count.
        let pos = 4 + 2 + 4 + 4;
        assert_eq!(bytes[pos], TY_UNIT);
        bytes[pos] = 0xee;
        assert_eq!(decode(&bytes), Err(DecodeError::BadTypeTag(0xee)));
    }

    #[test]
    fn bad_opcode_is_rejected() {
        let module = sample_module();
        let bytes = encode(&module);
        // Find the `Add` opcode byte and replace it.
        let pos = bytes
            .iter()
            .position(|b| *b == OP_ADD)
            .expect("sample has Add");
        let mut corrupt = bytes.clone();
        corrupt[pos] = 0xff;
        assert!(decode(&corrupt).is_err());
    }
}
