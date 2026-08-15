//! The module interface (`.lmi`): the export table with signatures
//! and definition hashes, plus the module semantic hash and the ABI
//! versions. The build tool emits it beside the artifact; week 6
//! fulfills `use` import slots from it.

use crate::identity::{ModuleIdentity, COMPILER_ABI_VERSION};
use crate::{BcRow, BcType, DecodeError, Module};

const MAGIC: &[u8; 4] = b"LMIF";
const VERSION: u16 = 1;

pub use crate::ExportKind;

/// One export: the name, the kind, the rendered full signature, and
/// the definition hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportEntry {
    pub kind: ExportKind,
    pub name: String,
    pub signature: String,
    pub hash: [u8; 32],
}

/// One decoded interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub abi_version: u32,
    pub compiler_abi_version: u32,
    pub semantic_hash: [u8; 32],
    pub exports: Vec<ExportEntry>,
}

/// Build the interface of one module. `exports` lists the exported
/// top-level names in declaration order with their kinds; the build
/// tool derives the list from the source module.
pub fn build_interface(
    module: &Module,
    identity: &ModuleIdentity,
    exports: &[(ExportKind, String)],
) -> Result<Interface, String> {
    let mut entries = Vec::with_capacity(exports.len());
    for (kind, name) in exports {
        let entry = match kind {
            ExportKind::Function => {
                let idx = module
                    .funcs
                    .iter()
                    .position(|f| &f.name == name)
                    .ok_or_else(|| format!("no function named `{name}`"))?;
                ExportEntry {
                    kind: *kind,
                    name: name.clone(),
                    signature: func_signature(module, idx as u32),
                    hash: identity.func_hashes[idx],
                }
            }
            ExportKind::Class | ExportKind::Enum | ExportKind::EnumCase => {
                let idx = module
                    .classes
                    .iter()
                    .position(|c| &c.name == name)
                    .ok_or_else(|| format!("no class named `{name}`"))?;
                ExportEntry {
                    kind: *kind,
                    name: name.clone(),
                    signature: class_signature(module, idx as u32),
                    hash: identity.class_hashes[idx],
                }
            }
        };
        entries.push(entry);
    }
    Ok(Interface {
        abi_version: lm_abi::ABI_VERSION,
        compiler_abi_version: COMPILER_ABI_VERSION,
        semantic_hash: identity.semantic_hash,
        exports: entries,
    })
}

/// Encode one interface deterministically.
pub fn encode_interface(interface: &Interface) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&interface.abi_version.to_le_bytes());
    out.extend_from_slice(&interface.compiler_abi_version.to_le_bytes());
    out.extend_from_slice(&interface.semantic_hash);
    out.extend_from_slice(&(interface.exports.len() as u32).to_le_bytes());
    for entry in &interface.exports {
        out.push(entry.kind.tag());
        out.extend_from_slice(&(entry.name.len() as u32).to_le_bytes());
        out.extend_from_slice(entry.name.as_bytes());
        out.extend_from_slice(&(entry.signature.len() as u32).to_le_bytes());
        out.extend_from_slice(entry.signature.as_bytes());
        out.extend_from_slice(&entry.hash);
    }
    out
}

/// Decode one interface. Structure only; length fields are bounded by
/// the input before any allocation is sized from them.
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
    let mut semantic_hash = [0u8; 32];
    semantic_hash.copy_from_slice(cur.take(32)?);
    let count = cur.len()?;
    let mut exports = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = match cur.u8()? {
            0 => ExportKind::Function,
            1 => ExportKind::Class,
            2 => ExportKind::Enum,
            3 => ExportKind::EnumCase,
            other => return Err(DecodeError::BadTypeTag(other)),
        };
        let name = cur.string()?;
        let signature = cur.string()?;
        let mut hash = [0u8; 32];
        hash.copy_from_slice(cur.take(32)?);
        exports.push(ExportEntry {
            kind,
            name,
            signature,
            hash,
        });
    }
    if cur.pos != bytes.len() {
        return Err(DecodeError::TrailingBytes);
    }
    Ok(Interface {
        abi_version,
        compiler_abi_version,
        semantic_hash,
        exports,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Render one interface as stable readable text.
pub fn dump_interface(interface: &Interface) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "interface");
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
            entry.signature
        );
        let _ = writeln!(out, "    hash {}", hex(&entry.hash));
    }
    out
}

// ----------------------------------------------------------------
// Signature rendering.
// ----------------------------------------------------------------

fn row_text(module: &Module, row: &[BcRow]) -> String {
    let parts: Vec<String> = row
        .iter()
        .map(|elem| match elem {
            BcRow::Op(idx) => module
                .strings
                .get(*idx as usize)
                .cloned()
                .unwrap_or_else(|| format!("s{idx}")),
            BcRow::Var(v) => format!("e{v}"),
        })
        .collect();
    parts.join(", ")
}

fn type_text(module: &Module, idx: u32) -> String {
    match &module.types[idx as usize] {
        BcType::Unit => "()".to_string(),
        BcType::Bool => "Bool".to_string(),
        BcType::Int => "Int".to_string(),
        BcType::Str => "String".to_string(),
        BcType::StringBuilder => "StringBuilder".to_string(),
        BcType::ByteBuffer => "ByteBuffer".to_string(),
        BcType::Class(c) => module.classes[*c as usize].name.clone(),
        BcType::Inst(c, args) => {
            let parts: Vec<String> = args.iter().map(|a| type_text(module, *a)).collect();
            format!("{}[{}]", module.classes[*c as usize].name, parts.join(", "))
        }
        BcType::List(e) => format!("[{}]", type_text(module, *e)),
        BcType::Map(k, v) => {
            format!("{{{}: {}}}", type_text(module, *k), type_text(module, *v))
        }
        BcType::Tuple(elems) => {
            let parts: Vec<String> = elems.iter().map(|e| type_text(module, *e)).collect();
            if parts.len() == 1 {
                format!("({},)", parts[0])
            } else {
                format!("({})", parts.join(", "))
            }
        }
        BcType::Fn(params, muts, ret, row) => {
            let parts: Vec<String> = params
                .iter()
                .zip(muts.iter())
                .map(|(p, m)| {
                    if *m {
                        format!("mut {}", type_text(module, *p))
                    } else {
                        type_text(module, *p)
                    }
                })
                .collect();
            let mut out = format!("({}) -> {}", parts.join(", "), type_text(module, *ret));
            if !row.is_empty() {
                out.push_str(" with ");
                out.push_str(&row_text(module, row));
            }
            out
        }
        BcType::Var(i) => format!("${i}"),
        BcType::Fault => "Fault".to_string(),
        BcType::Request => "Request".to_string(),
        BcType::PolicyTable => "PolicyTable".to_string(),
        BcType::EmptyVm => "EmptyVm".to_string(),
        BcType::Vm(t) => format!("Vm[{}]", type_text(module, *t)),
        BcType::PendingCall(a, r) => format!(
            "PendingCall[{}, {}]",
            type_text(module, *a),
            type_text(module, *r)
        ),
        BcType::Op(op, f) => format!("Op[op{}, {}]", op, type_text(module, *f)),
    }
}

/// The full signature text of one function, generics and row
/// included. Type parameters print as `$0..`; effect parameters as
/// `e0..`.
fn func_signature(module: &Module, idx: u32) -> String {
    let func = &module.funcs[idx as usize];
    let mut out = String::new();
    if func.type_params > 0 || func.effect_params > 0 {
        let mut parts: Vec<String> = (0..func.type_params).map(|i| format!("${i}")).collect();
        parts.extend((0..func.effect_params).map(|i| format!("effect e{i}")));
        out.push('[');
        out.push_str(&parts.join(", "));
        out.push(']');
    }
    let params: Vec<String> = func
        .params
        .iter()
        .zip(func.param_muts.iter())
        .map(|(p, m)| {
            if *m {
                format!("mut {}", type_text(module, *p))
            } else {
                type_text(module, *p)
            }
        })
        .collect();
    out.push('(');
    out.push_str(&params.join(", "));
    out.push_str(") -> ");
    out.push_str(&type_text(module, func.ret));
    if !func.row.is_empty() {
        out.push_str(" with ");
        out.push_str(&row_text(module, &func.row));
    }
    out
}

/// The signature text of one class or enum: the generic arity, the
/// parent, and the field layout.
fn class_signature(module: &Module, idx: u32) -> String {
    let class = &module.classes[idx as usize];
    let mut out = String::new();
    if class.type_params > 0 {
        let parts: Vec<String> = (0..class.type_params).map(|i| format!("${i}")).collect();
        out.push('[');
        out.push_str(&parts.join(", "));
        out.push(']');
    }
    if let Some(p) = class.parent() {
        out.push_str(" < ");
        out.push_str(&module.classes[p as usize].name);
    }
    let fields: Vec<String> = class
        .fields
        .iter()
        .map(|(name, ty)| format!("{name}: {}", type_text(module, *ty)))
        .collect();
    out.push('{');
    out.push_str(&fields.join(", "));
    out.push('}');
    out
}
