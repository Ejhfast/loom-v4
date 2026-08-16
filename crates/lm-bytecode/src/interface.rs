//! The module interface (`.lmi`).
//!
//! An interface holds one entry per exported top-level definition:
//! the name, the kind, the full structural signature, the interface
//! hash, and the definition hash. A compiler checks an importing
//! module against this file alone, and the build tool fulfills every
//! import slot from it.
//!
//! Signatures reference a class by qualified name, never by a
//! module-local index, so an interface is position-independent. The
//! empty module path names a core class, which every module carries.
//!
//! Two hashes answer two questions:
//!
//! - the **interface hash** answers "does this export still present
//!   the same surface?". It covers the name, the kind, and the
//!   signature, and no body. An import slot pins it, so an edit to an
//!   exported body never rebuilds a dependent module.
//! - the **definition hash** answers "is this the same
//!   implementation?" (specification 3.7). The build cache and the
//!   linker use it.

use crate::hash::sha256;
use crate::identity::{ModuleIdentity, COMPILER_ABI_VERSION};
use crate::{DecodeError, Module};

pub use crate::ExportKind;

const MAGIC: &[u8; 4] = b"LMIF";
const VERSION: u16 = 3;

/// The domain tag of the interface hash.
const TAG_IFACE: &[u8] = b"lm-iface-v1\0";

/// The largest nesting depth of one interface type. A deeper type
/// rejects, so a crafted file cannot grow the host stack.
const MAX_TYPE_DEPTH: u32 = 32;

/// One qualified definition name. An empty module path names a core
/// definition, which every module carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualName {
    pub module: String,
    pub name: String,
}

impl QualName {
    pub fn new(module: impl Into<String>, name: impl Into<String>) -> QualName {
        QualName {
            module: module.into(),
            name: name.into(),
        }
    }

    /// True when the name comes from the core image.
    pub fn is_core(&self) -> bool {
        self.module.is_empty()
    }

    pub fn text(&self) -> String {
        if self.module.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.module, self.name)
        }
    }
}

/// One effect-row element of an interface signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IfaceRow {
    /// An operation or group named by the manifest name.
    Op(String),
    /// One effect parameter of the enclosing signature.
    Var(u32),
}

/// One type inside an interface signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IfaceType {
    Unit,
    Bool,
    Int,
    Str,
    StringBuilder,
    ByteBuffer,
    Fault,
    Request,
    PolicyTable,
    EmptyVm,
    /// The frozen canonical graph digest type.
    Digest,
    /// One type parameter of the enclosing signature.
    Var(u32),
    /// A class or enum instance named by qualified name.
    Named {
        class: QualName,
        args: Vec<IfaceType>,
    },
    List(Box<IfaceType>),
    Map(Box<IfaceType>, Box<IfaceType>),
    Tuple(Vec<IfaceType>),
    Fn {
        params: Vec<IfaceType>,
        param_muts: Vec<bool>,
        ret: Box<IfaceType>,
        row: Vec<IfaceRow>,
    },
    Vm(Box<IfaceType>),
    PendingCall(Box<IfaceType>, Box<IfaceType>),
    Handle(Box<IfaceType>, Box<IfaceType>),
    Op(u32, Box<IfaceType>),
}

/// One callable signature, without `self`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceFn {
    pub type_params: u32,
    pub effect_params: u32,
    pub params: Vec<IfaceType>,
    pub param_muts: Vec<bool>,
    /// The declared parameter names, for labeled arguments.
    pub param_names: Vec<String>,
    pub ret: IfaceType,
    pub row: Vec<IfaceRow>,
}

/// One method of an exported class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceMethod {
    pub name: String,
    /// True when the method needs a mutable receiver.
    pub mut_self: bool,
    pub sig: IfaceFn,
}

/// One field of an exported class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceField {
    pub name: String,
    pub ty: IfaceType,
    /// True when the declaration carries a default expression.
    pub has_default: bool,
}

/// The declaration kind of an exported class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfaceClassKind {
    Normal,
    EnumParent,
    EnumCase,
}

impl IfaceClassKind {
    fn tag(self) -> u8 {
        match self {
            IfaceClassKind::Normal => 0,
            IfaceClassKind::EnumParent => 1,
            IfaceClassKind::EnumCase => 2,
        }
    }
}

/// The exported surface of one class or enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceClass {
    pub kind: IfaceClassKind,
    pub type_params: u32,
    pub parent: Option<QualName>,
    /// The full field layout: inherited fields first.
    pub fields: Vec<IfaceField>,
    /// The layout index where the own fields start.
    pub own_start: u32,
    pub methods: Vec<IfaceMethod>,
    /// The declared `init` signature, when the class has one.
    pub init: Option<IfaceFn>,
    /// The short arm names of an enum parent, in arm order.
    pub arms: Vec<String>,
    /// The short arm name of an enum case, for example `Some`.
    pub arm_short: String,
    /// The enum family of a case.
    pub family: Option<QualName>,
}

/// The exported surface of one definition.
///
/// The two variants differ in size. An interface item is build-time
/// data, and the tool reads one item once per export. The extra
/// allocation of a box would cost more than the unused bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum IfaceItem {
    Func(IfaceFn),
    Class(IfaceClass),
}

/// One export: the name, the kind, the signature, and the two hashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportEntry {
    pub kind: ExportKind,
    pub name: String,
    pub item: IfaceItem,
    /// The interface hash an import slot pins.
    pub iface_hash: [u8; 32],
    /// The definition hash of the implementation.
    pub def_hash: [u8; 32],
}

/// One decoded interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub abi_version: u32,
    pub compiler_abi_version: u32,
    /// The module path, for example `mathlib.matrix`.
    pub module_path: String,
    pub semantic_hash: [u8; 32],
    pub exports: Vec<ExportEntry>,
}

impl Interface {
    /// Find one export by name.
    pub fn find(&self, name: &str) -> Option<&ExportEntry> {
        self.exports.iter().find(|e| e.name == name)
    }
}

/// The interface hash of one export: the kind, the name, and the
/// signature, with the compiler ABI version and the operation
/// manifest. No body takes part.
pub fn interface_hash(kind: ExportKind, name: &str, item: &IfaceItem) -> [u8; 32] {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(TAG_IFACE);
    bytes.extend_from_slice(&COMPILER_ABI_VERSION.to_le_bytes());
    bytes.extend_from_slice(&lm_abi::manifest_digest());
    bytes.push(kind.tag());
    write_str(&mut bytes, name);
    encode_item(&mut bytes, item);
    sha256(&bytes)
}

/// Build one interface from the exported items and the computed
/// identity. `items` aligns with `module.exports`.
pub fn build_interface(
    module: &Module,
    identity: &ModuleIdentity,
    module_path: &str,
    items: &[IfaceItem],
) -> Result<Interface, String> {
    if items.len() != module.exports.len() {
        return Err("the interface items do not align with the export table".to_string());
    }
    let mut exports = Vec::with_capacity(items.len());
    for (export, item) in module.exports.iter().zip(items) {
        let def_hash = if export.kind.is_class() {
            identity.class_hashes[export.def as usize]
        } else {
            identity.func_hashes[export.def as usize]
        };
        exports.push(ExportEntry {
            kind: export.kind,
            name: export.name.clone(),
            item: item.clone(),
            iface_hash: interface_hash(export.kind, &export.name, item),
            def_hash,
        });
    }
    Ok(Interface {
        abi_version: lm_abi::ABI_VERSION,
        compiler_abi_version: COMPILER_ABI_VERSION,
        module_path: module_path.to_string(),
        semantic_hash: identity.semantic_hash,
        exports,
    })
}

// ----------------------------------------------------------------
// Encoding.
// ----------------------------------------------------------------

fn write_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn write_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn encode_qual(out: &mut Vec<u8>, q: &QualName) {
    write_str(out, &q.module);
    write_str(out, &q.name);
}

fn encode_row(out: &mut Vec<u8>, row: &[IfaceRow]) {
    write_u32(out, row.len() as u32);
    for elem in row {
        match elem {
            IfaceRow::Op(name) => {
                out.push(0);
                write_str(out, name);
            }
            IfaceRow::Var(v) => {
                out.push(1);
                write_u32(out, *v);
            }
        }
    }
}

fn encode_type(out: &mut Vec<u8>, ty: &IfaceType) {
    match ty {
        IfaceType::Unit => out.push(0),
        IfaceType::Bool => out.push(1),
        IfaceType::Int => out.push(2),
        IfaceType::Str => out.push(3),
        IfaceType::StringBuilder => out.push(4),
        IfaceType::ByteBuffer => out.push(5),
        IfaceType::Fault => out.push(6),
        IfaceType::Request => out.push(7),
        IfaceType::PolicyTable => out.push(8),
        IfaceType::EmptyVm => out.push(9),
        IfaceType::Digest => out.push(19),
        IfaceType::Var(i) => {
            out.push(10);
            write_u32(out, *i);
        }
        IfaceType::Named { class, args } => {
            out.push(11);
            encode_qual(out, class);
            write_u32(out, args.len() as u32);
            for a in args {
                encode_type(out, a);
            }
        }
        IfaceType::List(e) => {
            out.push(12);
            encode_type(out, e);
        }
        IfaceType::Map(k, v) => {
            out.push(13);
            encode_type(out, k);
            encode_type(out, v);
        }
        IfaceType::Tuple(elems) => {
            out.push(14);
            write_u32(out, elems.len() as u32);
            for e in elems {
                encode_type(out, e);
            }
        }
        IfaceType::Fn {
            params,
            param_muts,
            ret,
            row,
        } => {
            out.push(15);
            write_u32(out, params.len() as u32);
            for p in params {
                encode_type(out, p);
            }
            for m in param_muts {
                out.push(u8::from(*m));
            }
            encode_type(out, ret);
            encode_row(out, row);
        }
        IfaceType::Vm(t) => {
            out.push(16);
            encode_type(out, t);
        }
        IfaceType::PendingCall(a, r) => {
            out.push(17);
            encode_type(out, a);
            encode_type(out, r);
        }
        IfaceType::Op(op, f) => {
            out.push(18);
            write_u32(out, *op);
            encode_type(out, f);
        }
        IfaceType::Handle(m, r) => {
            out.push(20);
            encode_type(out, m);
            encode_type(out, r);
        }
    }
}

/// Encode one signature.
///
/// The marker and name vectors carry their own counts. The decoder
/// forces all three counts equal, so one encoding never stands for
/// two signatures and the interface hash stays unambiguous.
fn encode_fn(out: &mut Vec<u8>, sig: &IfaceFn) {
    write_u32(out, sig.type_params);
    write_u32(out, sig.effect_params);
    write_u32(out, sig.params.len() as u32);
    for p in &sig.params {
        encode_type(out, p);
    }
    write_u32(out, sig.param_muts.len() as u32);
    for m in &sig.param_muts {
        out.push(u8::from(*m));
    }
    write_u32(out, sig.param_names.len() as u32);
    for n in &sig.param_names {
        write_str(out, n);
    }
    encode_type(out, &sig.ret);
    encode_row(out, &sig.row);
}

fn encode_item(out: &mut Vec<u8>, item: &IfaceItem) {
    match item {
        IfaceItem::Func(sig) => {
            out.push(0);
            encode_fn(out, sig);
        }
        IfaceItem::Class(class) => {
            out.push(1);
            out.push(class.kind.tag());
            write_u32(out, class.type_params);
            match &class.parent {
                None => out.push(0),
                Some(p) => {
                    out.push(1);
                    encode_qual(out, p);
                }
            }
            write_u32(out, class.fields.len() as u32);
            for field in &class.fields {
                write_str(out, &field.name);
                encode_type(out, &field.ty);
                out.push(u8::from(field.has_default));
            }
            write_u32(out, class.own_start);
            write_u32(out, class.methods.len() as u32);
            for method in &class.methods {
                write_str(out, &method.name);
                out.push(u8::from(method.mut_self));
                encode_fn(out, &method.sig);
            }
            match &class.init {
                None => out.push(0),
                Some(sig) => {
                    out.push(1);
                    encode_fn(out, sig);
                }
            }
            write_u32(out, class.arms.len() as u32);
            for arm in &class.arms {
                write_str(out, arm);
            }
            write_str(out, &class.arm_short);
            match &class.family {
                None => out.push(0),
                Some(f) => {
                    out.push(1);
                    encode_qual(out, f);
                }
            }
        }
    }
}

/// Encode one interface deterministically.
pub fn encode_interface(interface: &Interface) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    write_u32(&mut out, interface.abi_version);
    write_u32(&mut out, interface.compiler_abi_version);
    write_str(&mut out, &interface.module_path);
    out.extend_from_slice(&interface.semantic_hash);
    write_u32(&mut out, interface.exports.len() as u32);
    for entry in &interface.exports {
        out.push(entry.kind.tag());
        write_str(&mut out, &entry.name);
        out.extend_from_slice(&entry.iface_hash);
        out.extend_from_slice(&entry.def_hash);
        encode_item(&mut out, &entry.item);
    }
    out
}

// ----------------------------------------------------------------
// Decoding.
// ----------------------------------------------------------------

fn decode_row(cur: &mut crate::Cursor<'_>) -> Result<Vec<IfaceRow>, DecodeError> {
    let count = cur.len()?;
    let mut row = Vec::with_capacity(count);
    for _ in 0..count {
        match cur.u8()? {
            0 => row.push(IfaceRow::Op(cur.string()?)),
            1 => row.push(IfaceRow::Var(cur.u32()?)),
            other => return Err(DecodeError::BadRowTag(other)),
        }
    }
    Ok(row)
}

fn decode_qual(cur: &mut crate::Cursor<'_>) -> Result<QualName, DecodeError> {
    let module = cur.string()?;
    let name = cur.string()?;
    Ok(QualName { module, name })
}

/// Decode one type. The depth is capped, so a crafted file cannot
/// grow the host stack.
fn decode_type(cur: &mut crate::Cursor<'_>, depth: u32) -> Result<IfaceType, DecodeError> {
    if depth > MAX_TYPE_DEPTH {
        return Err(DecodeError::BadLength);
    }
    let tag = cur.u8()?;
    let ty = match tag {
        0 => IfaceType::Unit,
        1 => IfaceType::Bool,
        2 => IfaceType::Int,
        3 => IfaceType::Str,
        4 => IfaceType::StringBuilder,
        5 => IfaceType::ByteBuffer,
        6 => IfaceType::Fault,
        7 => IfaceType::Request,
        8 => IfaceType::PolicyTable,
        9 => IfaceType::EmptyVm,
        19 => IfaceType::Digest,
        10 => IfaceType::Var(cur.u32()?),
        11 => {
            let class = decode_qual(cur)?;
            let count = cur.len()?;
            let mut args = Vec::with_capacity(count);
            for _ in 0..count {
                args.push(decode_type(cur, depth + 1)?);
            }
            IfaceType::Named { class, args }
        }
        12 => IfaceType::List(Box::new(decode_type(cur, depth + 1)?)),
        13 => {
            let k = decode_type(cur, depth + 1)?;
            let v = decode_type(cur, depth + 1)?;
            IfaceType::Map(Box::new(k), Box::new(v))
        }
        14 => {
            let count = cur.len()?;
            let mut elems = Vec::with_capacity(count);
            for _ in 0..count {
                elems.push(decode_type(cur, depth + 1)?);
            }
            IfaceType::Tuple(elems)
        }
        15 => {
            let count = cur.len()?;
            let mut params = Vec::with_capacity(count);
            for _ in 0..count {
                params.push(decode_type(cur, depth + 1)?);
            }
            let mut param_muts = Vec::with_capacity(count);
            for _ in 0..count {
                param_muts.push(cur.flag()?);
            }
            let ret = decode_type(cur, depth + 1)?;
            let row = decode_row(cur)?;
            IfaceType::Fn {
                params,
                param_muts,
                ret: Box::new(ret),
                row,
            }
        }
        16 => IfaceType::Vm(Box::new(decode_type(cur, depth + 1)?)),
        17 => {
            let a = decode_type(cur, depth + 1)?;
            let r = decode_type(cur, depth + 1)?;
            IfaceType::PendingCall(Box::new(a), Box::new(r))
        }
        18 => {
            let op = cur.u32()?;
            let f = decode_type(cur, depth + 1)?;
            IfaceType::Op(op, Box::new(f))
        }
        20 => {
            let m = decode_type(cur, depth + 1)?;
            let r = decode_type(cur, depth + 1)?;
            IfaceType::Handle(Box::new(m), Box::new(r))
        }
        other => return Err(DecodeError::BadTypeTag(other)),
    };
    Ok(ty)
}

fn decode_fn(cur: &mut crate::Cursor<'_>) -> Result<IfaceFn, DecodeError> {
    let type_params = cur.u32()?;
    let effect_params = cur.u32()?;
    let count = cur.len()?;
    let mut params = Vec::with_capacity(count);
    for _ in 0..count {
        params.push(decode_type(cur, 0)?);
    }
    // The three counts must agree, so one signature has one encoding.
    if cur.len()? != count {
        return Err(DecodeError::BadLength);
    }
    let mut param_muts = Vec::with_capacity(count);
    for _ in 0..count {
        param_muts.push(cur.flag()?);
    }
    if cur.len()? != count {
        return Err(DecodeError::BadLength);
    }
    let mut param_names = Vec::with_capacity(count);
    for _ in 0..count {
        param_names.push(cur.string()?);
    }
    let ret = decode_type(cur, 0)?;
    let row = decode_row(cur)?;
    Ok(IfaceFn {
        type_params,
        effect_params,
        params,
        param_muts,
        param_names,
        ret,
        row,
    })
}

fn decode_item(cur: &mut crate::Cursor<'_>) -> Result<IfaceItem, DecodeError> {
    match cur.u8()? {
        0 => Ok(IfaceItem::Func(decode_fn(cur)?)),
        1 => {
            let kind = match cur.u8()? {
                0 => IfaceClassKind::Normal,
                1 => IfaceClassKind::EnumParent,
                2 => IfaceClassKind::EnumCase,
                other => return Err(DecodeError::BadClassKind(other)),
            };
            let type_params = cur.u32()?;
            let parent = match cur.u8()? {
                0 => None,
                1 => Some(decode_qual(cur)?),
                other => return Err(DecodeError::BadFlag(other)),
            };
            let field_count = cur.len()?;
            let mut fields = Vec::with_capacity(field_count);
            for _ in 0..field_count {
                let name = cur.string()?;
                let ty = decode_type(cur, 0)?;
                let has_default = cur.flag()?;
                fields.push(IfaceField {
                    name,
                    ty,
                    has_default,
                });
            }
            let own_start = cur.u32()?;
            if own_start as usize > fields.len() {
                return Err(DecodeError::BadLength);
            }
            let method_count = cur.len()?;
            let mut methods = Vec::with_capacity(method_count);
            for _ in 0..method_count {
                let name = cur.string()?;
                let mut_self = cur.flag()?;
                let sig = decode_fn(cur)?;
                methods.push(IfaceMethod {
                    name,
                    mut_self,
                    sig,
                });
            }
            let init = match cur.u8()? {
                0 => None,
                1 => Some(decode_fn(cur)?),
                other => return Err(DecodeError::BadFlag(other)),
            };
            let arm_count = cur.len()?;
            let mut arms = Vec::with_capacity(arm_count);
            for _ in 0..arm_count {
                arms.push(cur.string()?);
            }
            let arm_short = cur.string()?;
            let family = match cur.u8()? {
                0 => None,
                1 => Some(decode_qual(cur)?),
                other => return Err(DecodeError::BadFlag(other)),
            };
            Ok(IfaceItem::Class(IfaceClass {
                kind,
                type_params,
                parent,
                fields,
                own_start,
                methods,
                init,
                arms,
                arm_short,
                family,
            }))
        }
        other => Err(DecodeError::BadTypeTag(other)),
    }
}

/// Decode one interface. Structure only; every length field is
/// bounded by the input before an allocation is sized from it.
pub fn decode_interface(bytes: &[u8]) -> Result<Interface, DecodeError> {
    let mut cur = crate::Cursor { bytes, pos: 0 };
    if cur.take(4)? != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let version = cur.u16()?;
    if version != VERSION {
        return Err(DecodeError::BadVersion(version));
    }
    let abi_version = cur.u32()?;
    let compiler_abi_version = cur.u32()?;
    let module_path = cur.string()?;
    let mut semantic_hash = [0u8; 32];
    semantic_hash.copy_from_slice(cur.take(32)?);
    let count = cur.len()?;
    let mut exports = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = ExportKind::from_tag(cur.u8()?).ok_or(DecodeError::BadExport)?;
        let name = cur.string()?;
        let mut iface_hash = [0u8; 32];
        iface_hash.copy_from_slice(cur.take(32)?);
        let mut def_hash = [0u8; 32];
        def_hash.copy_from_slice(cur.take(32)?);
        let item = decode_item(&mut cur)?;
        // The kind and the item must agree, so a later pass never
        // reads a class item as a function.
        let agrees = matches!(
            (kind.is_class(), &item),
            (true, IfaceItem::Class(_)) | (false, IfaceItem::Func(_))
        );
        if !agrees {
            return Err(DecodeError::BadExport);
        }
        exports.push(ExportEntry {
            kind,
            name,
            item,
            iface_hash,
            def_hash,
        });
    }
    if cur.pos != bytes.len() {
        return Err(DecodeError::TrailingBytes);
    }
    Ok(Interface {
        abi_version,
        compiler_abi_version,
        module_path,
        semantic_hash,
        exports,
    })
}

// ----------------------------------------------------------------
// Readable rendering.
// ----------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn row_text(row: &[IfaceRow]) -> String {
    let parts: Vec<String> = row
        .iter()
        .map(|e| match e {
            IfaceRow::Op(name) => name.clone(),
            IfaceRow::Var(v) => format!("e{v}"),
        })
        .collect();
    parts.join(", ")
}

/// Render one interface type as stable readable text.
pub fn type_text(ty: &IfaceType) -> String {
    match ty {
        IfaceType::Unit => "()".to_string(),
        IfaceType::Bool => "Bool".to_string(),
        IfaceType::Int => "Int".to_string(),
        IfaceType::Str => "String".to_string(),
        IfaceType::StringBuilder => "StringBuilder".to_string(),
        IfaceType::ByteBuffer => "ByteBuffer".to_string(),
        IfaceType::Fault => "Fault".to_string(),
        IfaceType::Request => "Request".to_string(),
        IfaceType::PolicyTable => "PolicyTable".to_string(),
        IfaceType::EmptyVm => "EmptyVm".to_string(),
        IfaceType::Digest => "Digest".to_string(),
        IfaceType::Var(i) => format!("${i}"),
        IfaceType::Named { class, args } => {
            if args.is_empty() {
                class.text()
            } else {
                let parts: Vec<String> = args.iter().map(type_text).collect();
                format!("{}[{}]", class.text(), parts.join(", "))
            }
        }
        IfaceType::List(e) => format!("[{}]", type_text(e)),
        IfaceType::Map(k, v) => format!("{{{}: {}}}", type_text(k), type_text(v)),
        IfaceType::Tuple(elems) => {
            let parts: Vec<String> = elems.iter().map(type_text).collect();
            if parts.len() == 1 {
                format!("({},)", parts[0])
            } else {
                format!("({})", parts.join(", "))
            }
        }
        IfaceType::Fn {
            params,
            param_muts,
            ret,
            row,
        } => {
            let parts: Vec<String> = params
                .iter()
                .zip(param_muts.iter())
                .map(|(p, m)| {
                    if *m {
                        format!("mut {}", type_text(p))
                    } else {
                        type_text(p)
                    }
                })
                .collect();
            let mut out = format!("({}) -> {}", parts.join(", "), type_text(ret));
            if !row.is_empty() {
                out.push_str(" with ");
                out.push_str(&row_text(row));
            }
            out
        }
        IfaceType::Vm(t) => format!("Vm[{}]", type_text(t)),
        IfaceType::PendingCall(a, r) => {
            format!("PendingCall[{}, {}]", type_text(a), type_text(r))
        }
        IfaceType::Op(op, f) => format!("Op[op{}, {}]", op, type_text(f)),
        IfaceType::Handle(m, r) => format!("Handle[{}, {}]", type_text(m), type_text(r)),
    }
}

/// Render one callable signature as stable readable text.
pub fn fn_text(sig: &IfaceFn) -> String {
    let mut out = String::new();
    if sig.type_params > 0 || sig.effect_params > 0 {
        let mut parts: Vec<String> = (0..sig.type_params).map(|i| format!("${i}")).collect();
        parts.extend((0..sig.effect_params).map(|i| format!("effect e{i}")));
        out.push('[');
        out.push_str(&parts.join(", "));
        out.push(']');
    }
    let params: Vec<String> = sig
        .params
        .iter()
        .zip(sig.param_muts.iter())
        .zip(sig.param_names.iter())
        .map(|((p, m), n)| {
            let ty = type_text(p);
            match (m, n.is_empty()) {
                (true, false) => format!("{n}: mut {ty}"),
                (true, true) => format!("mut {ty}"),
                (false, false) => format!("{n}: {ty}"),
                (false, true) => ty,
            }
        })
        .collect();
    out.push('(');
    out.push_str(&params.join(", "));
    out.push_str(") -> ");
    out.push_str(&type_text(&sig.ret));
    if !sig.row.is_empty() {
        out.push_str(" with ");
        out.push_str(&row_text(&sig.row));
    }
    out
}

/// The one-line signature text of one export.
pub fn item_text(item: &IfaceItem) -> String {
    match item {
        IfaceItem::Func(sig) => fn_text(sig),
        IfaceItem::Class(class) => {
            let mut out = String::new();
            if class.type_params > 0 {
                let parts: Vec<String> = (0..class.type_params).map(|i| format!("${i}")).collect();
                out.push('[');
                out.push_str(&parts.join(", "));
                out.push(']');
            }
            if let Some(p) = &class.parent {
                out.push_str(" < ");
                out.push_str(&p.text());
            }
            let fields: Vec<String> = class
                .fields
                .iter()
                .map(|f| {
                    let mark = if f.has_default { " = ..." } else { "" };
                    format!("{}: {}{}", f.name, type_text(&f.ty), mark)
                })
                .collect();
            out.push('{');
            out.push_str(&fields.join(", "));
            out.push('}');
            out
        }
    }
}

/// Render one interface as stable readable text.
pub fn dump_interface(interface: &Interface) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "interface {}", interface.module_path);
    let _ = writeln!(out, "semantic {}", hex(&interface.semantic_hash));
    let _ = writeln!(
        out,
        "abi {} compiler-abi {}",
        interface.abi_version, interface.compiler_abi_version
    );
    let _ = writeln!(out, "exports {}", interface.exports.len());
    for entry in &interface.exports {
        let _ = writeln!(
            out,
            "  {} {} {}",
            entry.kind.text(),
            entry.name,
            item_text(&entry.item)
        );
        let _ = writeln!(out, "    interface {}", hex(&entry.iface_hash));
        let _ = writeln!(out, "    definition {}", hex(&entry.def_hash));
        if let IfaceItem::Class(class) = &entry.item {
            for method in &class.methods {
                let recv = if method.mut_self { "mut self" } else { "self" };
                let _ = writeln!(
                    out,
                    "    def {}({recv}) {}",
                    method.name,
                    fn_text(&method.sig)
                );
            }
        }
    }
    out
}
