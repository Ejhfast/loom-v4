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
// Version 17 binds each interface to one immutable ABI bundle.
const VERSION: u16 = 18;
const LINKAGE_MAGIC: &[u8; 4] = b"LMLK";

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
    Bytes,
    FileHandle,
    ResourceHandle,
    HostResource,
    Fault,
    Request,
    PolicyTable,
    Vm,
    /// The frozen canonical graph digest type.
    Digest,
    /// One type parameter of the enclosing signature.
    Var(u32),
    /// One associated type selected through a nominal interface.
    Projection {
        base: Box<IfaceType>,
        interface: QualName,
        assoc: String,
    },
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
    Callback {
        params: Vec<IfaceType>,
        param_muts: Vec<bool>,
        ret: Box<IfaceType>,
        row: Vec<IfaceRow>,
    },
    Run(Box<IfaceType>),
    Wait(Box<IfaceType>),
    PendingCall(Box<IfaceType>, Box<IfaceType>),
    Handle(Box<IfaceType>, Box<IfaceType>),
    Op(u32, Box<IfaceType>),
    /// One verified snapshot image with no checked result type.
    VmSnapshot,
    /// One snapshot of a machine world, typed by the terminal result
    /// type of its root machine.
    RunSnapshot(Box<IfaceType>),
}

/// One callable signature, without `self`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceFn {
    pub type_params: u32,
    pub type_bounds: Vec<Vec<IfaceInterfaceUse>>,
    pub effect_params: u32,
    pub params: Vec<IfaceType>,
    pub param_muts: Vec<bool>,
    /// The declared parameter names, for labeled arguments.
    pub param_names: Vec<String>,
    pub ret: IfaceType,
    pub row: Vec<IfaceRow>,
}

/// One applied nominal interface in a module interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceInterfaceUse {
    pub interface: QualName,
    pub types: Vec<IfaceType>,
    pub rows: Vec<Vec<IfaceRow>>,
}

/// One associated type in an exported interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceAssociated {
    pub name: String,
    pub bounds: Vec<IfaceInterfaceUse>,
}

/// One method requirement in an exported interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceInterfaceMethod {
    pub name: String,
    pub mut_self: bool,
    pub params: Vec<IfaceType>,
    pub param_muts: Vec<bool>,
    pub param_names: Vec<String>,
    pub ret: IfaceType,
    pub row: Vec<IfaceRow>,
}

/// The exported surface of one nominal interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceInterface {
    pub type_params: u32,
    pub effect_params: u32,
    pub generic_is_effect: Vec<bool>,
    pub parents: Vec<IfaceInterfaceUse>,
    pub type_bounds: Vec<Vec<IfaceInterfaceUse>>,
    pub associated: Vec<IfaceAssociated>,
    pub methods: Vec<IfaceInterfaceMethod>,
}

/// One class-owned conformance in a module interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceConformance {
    pub application: IfaceInterfaceUse,
    pub associated: Vec<IfaceType>,
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
    /// True when the class cannot have a subclass.
    pub is_final: bool,
    pub type_params: u32,
    pub type_bounds: Vec<Vec<IfaceInterfaceUse>>,
    pub conformances: Vec<IfaceConformance>,
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
    Interface(IfaceInterface),
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

/// The target kind of one late source binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IfaceSlotKind {
    Function,
    Method,
    Class,
}

impl IfaceSlotKind {
    fn tag(self) -> u8 {
        match self {
            IfaceSlotKind::Function => 0,
            IfaceSlotKind::Method => 1,
            IfaceSlotKind::Class => 2,
        }
    }

    fn from_tag(tag: u8) -> Option<IfaceSlotKind> {
        match tag {
            0 => Some(IfaceSlotKind::Function),
            1 => Some(IfaceSlotKind::Method),
            2 => Some(IfaceSlotKind::Class),
            _ => None,
        }
    }
}

/// One position-independent published binding in a module interface.
///
/// The named export or class member supplies the full contract. This
/// record adds its linkage mode and stable slot key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IfaceSlotSpec {
    pub binding: String,
    /// The body-independent contract identity of this binding.
    pub contract_hash: [u8; 32],
    pub key: [u8; 32],
    pub kind: IfaceSlotKind,
    /// True when compiled callers must read this slot.
    pub late: bool,
}

/// One decoded interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub abi_version: u32,
    pub compiler_abi_version: u32,
    /// The exact operation bundle used to build this interface.
    pub bundle_digest: [u8; 32],
    /// The module path, for example `mathlib.matrix`.
    pub module_path: String,
    pub semantic_hash: [u8; 32],
    pub exports: Vec<ExportEntry>,
    /// Published bindings and their linkage modes.
    pub slots: Vec<IfaceSlotSpec>,
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
    let bundle = lm_abi::standard_bundle();
    interface_hash_with_bundle(&bundle, kind, name, item)
}

/// Return one export contract hash under an immutable ABI bundle.
pub fn interface_hash_with_bundle(
    bundle: &lm_abi::AbiBundle,
    kind: ExportKind,
    name: &str,
    item: &IfaceItem,
) -> [u8; 32] {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(TAG_IFACE);
    bytes.extend_from_slice(&COMPILER_ABI_VERSION.to_le_bytes());
    bytes.extend_from_slice(&bundle.digest());
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
    let bundle = lm_abi::standard_bundle();
    build_interface_with_bundle(module, identity, module_path, items, &bundle)
}

/// Build one interface under an immutable ABI bundle.
pub fn build_interface_with_bundle(
    module: &Module,
    identity: &ModuleIdentity,
    module_path: &str,
    items: &[IfaceItem],
    bundle: &lm_abi::AbiBundle,
) -> Result<Interface, String> {
    if items.len() != module.exports.len() {
        return Err("the interface items do not align with the export table".to_string());
    }
    let mut exports = Vec::with_capacity(items.len());
    for (export, item) in module.exports.iter().zip(items) {
        let def_hash = if export.kind.is_class() {
            identity.class_hashes[export.def as usize]
        } else if export.kind.is_interface() {
            identity.interface_hashes[export.def as usize]
        } else {
            identity.func_hashes[export.def as usize]
        };
        exports.push(ExportEntry {
            kind: export.kind,
            name: export.name.clone(),
            item: item.clone(),
            iface_hash: interface_hash_with_bundle(bundle, export.kind, &export.name, item),
            def_hash,
        });
    }
    Ok(Interface {
        abi_version: lm_abi::ABI_VERSION,
        compiler_abi_version: COMPILER_ABI_VERSION,
        bundle_digest: bundle.digest(),
        module_path: module_path.to_string(),
        semantic_hash: identity.semantic_hash,
        exports,
        slots: Vec::new(),
    })
}

/// Validate one decoded interface against its verified source module.
pub fn validate_interface(
    module: &Module,
    identity: &ModuleIdentity,
    interface: &Interface,
) -> Result<(), String> {
    let bundle = lm_abi::standard_bundle();
    validate_interface_with_bundle(module, identity, interface, &bundle)
}

/// Validate one decoded interface under an immutable ABI bundle.
pub fn validate_interface_with_bundle(
    module: &Module,
    identity: &ModuleIdentity,
    interface: &Interface,
    bundle: &lm_abi::AbiBundle,
) -> Result<(), String> {
    if interface.abi_version != lm_abi::ABI_VERSION {
        return Err("the interface has another operation ABI version".to_string());
    }
    if interface.compiler_abi_version != COMPILER_ABI_VERSION {
        return Err("the interface has another compiler ABI version".to_string());
    }
    if interface.bundle_digest != bundle.digest() {
        return Err("the interface has another ABI bundle".to_string());
    }
    if interface.semantic_hash != identity.semantic_hash {
        return Err("the interface names another module identity".to_string());
    }
    if interface.exports.len() != module.exports.len() {
        return Err("the interface export count differs from the module".to_string());
    }
    for (entry, export) in interface.exports.iter().zip(&module.exports) {
        if entry.kind != export.kind || entry.name != export.name {
            return Err("an interface export differs from the module export".to_string());
        }
        if entry.iface_hash
            != interface_hash_with_bundle(bundle, entry.kind, &entry.name, &entry.item)
        {
            return Err("an interface export has an invalid interface hash".to_string());
        }
        let definition = if export.kind.is_class() {
            identity.class_hashes.get(export.def as usize)
        } else if export.kind.is_interface() {
            identity.interface_hashes.get(export.def as usize)
        } else {
            identity.func_hashes.get(export.def as usize)
        }
        .ok_or_else(|| "an interface export names no module definition".to_string())?;
        if entry.def_hash != *definition {
            return Err("an interface export has another definition hash".to_string());
        }
    }
    if interface.slots.len() > module.slots.len() {
        return Err("the interface has more slots than the module".to_string());
    }
    for spec in &interface.slots {
        if spec.key != crate::slot_key(&spec.binding, &spec.contract_hash) {
            return Err("an interface slot has an invalid key".to_string());
        }
        let slot = module
            .slots
            .iter()
            .find(|slot| slot.key == spec.key)
            .ok_or_else(|| "an interface slot has no module slot".to_string())?;
        if slot.contract_hash != spec.contract_hash {
            return Err("an interface slot has another contract identity".to_string());
        }
        let agrees = matches!(
            (spec.kind, &slot.contract),
            (IfaceSlotKind::Function, crate::SlotContract::Function(_))
                | (IfaceSlotKind::Method, crate::SlotContract::Method(_))
                | (IfaceSlotKind::Class, crate::SlotContract::Class { .. })
        );
        if !agrees {
            return Err("an interface slot has another contract kind".to_string());
        }
    }
    Ok(())
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

fn encode_interface_use(out: &mut Vec<u8>, application: &IfaceInterfaceUse) {
    encode_qual(out, &application.interface);
    write_u32(out, application.types.len() as u32);
    for ty in &application.types {
        encode_type(out, ty);
    }
    write_u32(out, application.rows.len() as u32);
    for row in &application.rows {
        encode_row(out, row);
    }
}

fn encode_bounds(out: &mut Vec<u8>, bounds: &[Vec<IfaceInterfaceUse>]) {
    write_u32(out, bounds.len() as u32);
    for parameter in bounds {
        write_u32(out, parameter.len() as u32);
        for bound in parameter {
            encode_interface_use(out, bound);
        }
    }
}

fn encode_type(out: &mut Vec<u8>, ty: &IfaceType) {
    match ty {
        IfaceType::Unit => out.push(0),
        IfaceType::Bool => out.push(1),
        IfaceType::Int => out.push(2),
        IfaceType::Str => out.push(3),
        IfaceType::Fault => out.push(6),
        IfaceType::Request => out.push(7),
        IfaceType::PolicyTable => out.push(8),
        IfaceType::Vm => out.push(9),
        IfaceType::Digest => out.push(19),
        IfaceType::Var(i) => {
            out.push(10);
            write_u32(out, *i);
        }
        IfaceType::Projection {
            base,
            interface,
            assoc,
        } => {
            out.push(27);
            encode_type(out, base);
            encode_qual(out, interface);
            write_str(out, assoc);
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
        IfaceType::Callback {
            params,
            param_muts,
            ret,
            row,
        } => {
            out.push(28);
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
        IfaceType::Run(t) => {
            out.push(16);
            encode_type(out, t);
        }
        IfaceType::Wait(t) => {
            out.push(26);
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
        IfaceType::VmSnapshot => out.push(21),
        IfaceType::RunSnapshot(t) => {
            out.push(22);
            encode_type(out, t);
        }
        IfaceType::Bytes => out.push(23),
        IfaceType::FileHandle => out.push(24),
        IfaceType::ResourceHandle => out.push(25),
        IfaceType::HostResource => out.push(29),
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
    encode_bounds(out, &sig.type_bounds);
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
            out.push(u8::from(class.is_final));
            write_u32(out, class.type_params);
            encode_bounds(out, &class.type_bounds);
            write_u32(out, class.conformances.len() as u32);
            for conformance in &class.conformances {
                encode_interface_use(out, &conformance.application);
                write_u32(out, conformance.associated.len() as u32);
                for ty in &conformance.associated {
                    encode_type(out, ty);
                }
            }
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
        IfaceItem::Interface(interface) => {
            out.push(2);
            write_u32(out, interface.type_params);
            write_u32(out, interface.effect_params);
            write_u32(out, interface.generic_is_effect.len() as u32);
            for marker in &interface.generic_is_effect {
                out.push(u8::from(*marker));
            }
            write_u32(out, interface.parents.len() as u32);
            for parent in &interface.parents {
                encode_interface_use(out, parent);
            }
            encode_bounds(out, &interface.type_bounds);
            write_u32(out, interface.associated.len() as u32);
            for associated in &interface.associated {
                write_str(out, &associated.name);
                write_u32(out, associated.bounds.len() as u32);
                for bound in &associated.bounds {
                    encode_interface_use(out, bound);
                }
            }
            write_u32(out, interface.methods.len() as u32);
            for method in &interface.methods {
                write_str(out, &method.name);
                out.push(u8::from(method.mut_self));
                write_u32(out, method.params.len() as u32);
                for ty in &method.params {
                    encode_type(out, ty);
                }
                write_u32(out, method.param_muts.len() as u32);
                for marker in &method.param_muts {
                    out.push(u8::from(*marker));
                }
                write_u32(out, method.param_names.len() as u32);
                for name in &method.param_names {
                    write_str(out, name);
                }
                encode_type(out, &method.ret);
                encode_row(out, &method.row);
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
    out.extend_from_slice(&interface.bundle_digest);
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
    out.extend_from_slice(LINKAGE_MAGIC);
    let mut slots = interface.slots.clone();
    slots.sort();
    write_u32(&mut out, slots.len() as u32);
    for slot in slots {
        write_str(&mut out, &slot.binding);
        out.extend_from_slice(&slot.contract_hash);
        out.extend_from_slice(&slot.key);
        out.push(slot.kind.tag());
        out.push(u8::from(slot.late));
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

fn decode_interface_use(cur: &mut crate::Cursor<'_>) -> Result<IfaceInterfaceUse, DecodeError> {
    let interface = decode_qual(cur)?;
    let type_count = cur.len()?;
    let mut types = Vec::with_capacity(type_count);
    for _ in 0..type_count {
        types.push(decode_type(cur, 0)?);
    }
    let row_count = cur.len()?;
    let mut rows = Vec::with_capacity(row_count);
    for _ in 0..row_count {
        rows.push(decode_row(cur)?);
    }
    Ok(IfaceInterfaceUse {
        interface,
        types,
        rows,
    })
}

fn decode_bounds(cur: &mut crate::Cursor<'_>) -> Result<Vec<Vec<IfaceInterfaceUse>>, DecodeError> {
    let parameter_count = cur.len()?;
    let mut bounds = Vec::with_capacity(parameter_count);
    for _ in 0..parameter_count {
        let bound_count = cur.len()?;
        let mut parameter = Vec::with_capacity(bound_count);
        for _ in 0..bound_count {
            parameter.push(decode_interface_use(cur)?);
        }
        bounds.push(parameter);
    }
    Ok(bounds)
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
        6 => IfaceType::Fault,
        7 => IfaceType::Request,
        8 => IfaceType::PolicyTable,
        9 => IfaceType::Vm,
        19 => IfaceType::Digest,
        10 => IfaceType::Var(cur.u32()?),
        27 => IfaceType::Projection {
            base: Box::new(decode_type(cur, depth + 1)?),
            interface: decode_qual(cur)?,
            assoc: cur.string()?,
        },
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
        28 => {
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
            IfaceType::Callback {
                params,
                param_muts,
                ret: Box::new(ret),
                row,
            }
        }
        16 => IfaceType::Run(Box::new(decode_type(cur, depth + 1)?)),
        26 => IfaceType::Wait(Box::new(decode_type(cur, depth + 1)?)),
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
        21 => IfaceType::VmSnapshot,
        22 => IfaceType::RunSnapshot(Box::new(decode_type(cur, depth + 1)?)),
        23 => IfaceType::Bytes,
        24 => IfaceType::FileHandle,
        25 => IfaceType::ResourceHandle,
        29 => IfaceType::HostResource,
        other => return Err(DecodeError::BadTypeTag(other)),
    };
    Ok(ty)
}

fn decode_fn(cur: &mut crate::Cursor<'_>) -> Result<IfaceFn, DecodeError> {
    let type_params = cur.u32()?;
    let effect_params = cur.u32()?;
    let type_bounds = decode_bounds(cur)?;
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
        type_bounds,
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
            let is_final = cur.flag()?;
            let type_params = cur.u32()?;
            let type_bounds = decode_bounds(cur)?;
            let conformance_count = cur.len()?;
            let mut conformances = Vec::with_capacity(conformance_count);
            for _ in 0..conformance_count {
                let application = decode_interface_use(cur)?;
                let associated_count = cur.len()?;
                let mut associated = Vec::with_capacity(associated_count);
                for _ in 0..associated_count {
                    associated.push(decode_type(cur, 0)?);
                }
                conformances.push(IfaceConformance {
                    application,
                    associated,
                });
            }
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
                is_final,
                type_params,
                type_bounds,
                conformances,
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
        2 => {
            let type_params = cur.u32()?;
            let effect_params = cur.u32()?;
            let generic_count = cur.len()?;
            let mut generic_is_effect = Vec::with_capacity(generic_count);
            for _ in 0..generic_count {
                generic_is_effect.push(cur.flag()?);
            }
            let parent_count = cur.len()?;
            let mut parents = Vec::with_capacity(parent_count);
            for _ in 0..parent_count {
                parents.push(decode_interface_use(cur)?);
            }
            let type_bounds = decode_bounds(cur)?;
            let associated_count = cur.len()?;
            let mut associated = Vec::with_capacity(associated_count);
            for _ in 0..associated_count {
                let name = cur.string()?;
                let bound_count = cur.len()?;
                if bound_count > cur.remaining() / 16 {
                    return Err(DecodeError::BadLength);
                }
                let mut bounds = Vec::with_capacity(bound_count);
                for _ in 0..bound_count {
                    bounds.push(decode_interface_use(cur)?);
                }
                associated.push(IfaceAssociated { name, bounds });
            }
            let method_count = cur.len()?;
            let mut methods = Vec::with_capacity(method_count);
            for _ in 0..method_count {
                let name = cur.string()?;
                let mut_self = cur.flag()?;
                let param_count = cur.len()?;
                let mut params = Vec::with_capacity(param_count);
                for _ in 0..param_count {
                    params.push(decode_type(cur, 0)?);
                }
                if cur.len()? != param_count {
                    return Err(DecodeError::BadLength);
                }
                let mut param_muts = Vec::with_capacity(param_count);
                for _ in 0..param_count {
                    param_muts.push(cur.flag()?);
                }
                if cur.len()? != param_count {
                    return Err(DecodeError::BadLength);
                }
                let mut param_names = Vec::with_capacity(param_count);
                for _ in 0..param_count {
                    param_names.push(cur.string()?);
                }
                let ret = decode_type(cur, 0)?;
                let row = decode_row(cur)?;
                methods.push(IfaceInterfaceMethod {
                    name,
                    mut_self,
                    params,
                    param_muts,
                    param_names,
                    ret,
                    row,
                });
            }
            Ok(IfaceItem::Interface(IfaceInterface {
                type_params,
                effect_params,
                generic_is_effect,
                parents,
                type_bounds,
                associated,
                methods,
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
    let mut bundle_digest = [0u8; 32];
    bundle_digest.copy_from_slice(cur.take(32)?);
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
        let agrees = match (&item, kind) {
            (IfaceItem::Class(_), kind) => kind.is_class(),
            (IfaceItem::Interface(_), kind) => kind.is_interface(),
            (IfaceItem::Func(_), ExportKind::Function) => true,
            _ => false,
        };
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
    if cur.take(4)? != LINKAGE_MAGIC {
        return Err(DecodeError::TrailingBytes);
    }
    let slot_count = cur.len()?;
    const MIN_SLOT_BYTES: usize = 4 + 32 + 32 + 1 + 1;
    if slot_count > cur.remaining() / MIN_SLOT_BYTES {
        return Err(DecodeError::BadLength);
    }
    let mut slots = Vec::with_capacity(slot_count);
    for _ in 0..slot_count {
        let binding = cur.string()?;
        let mut contract_hash = [0u8; 32];
        contract_hash.copy_from_slice(cur.take(32)?);
        let mut key = [0u8; 32];
        key.copy_from_slice(cur.take(32)?);
        let kind = IfaceSlotKind::from_tag(cur.u8()?).ok_or(DecodeError::BadSlot)?;
        let late = match cur.u8()? {
            0 => false,
            1 => true,
            _ => return Err(DecodeError::BadSlot),
        };
        slots.push(IfaceSlotSpec {
            binding,
            contract_hash,
            key,
            kind,
            late,
        });
    }
    let mut canonical = slots.clone();
    canonical.sort();
    if slots != canonical {
        return Err(DecodeError::BadSlot);
    }
    if cur.pos != bytes.len() {
        return Err(DecodeError::TrailingBytes);
    }
    Ok(Interface {
        abi_version,
        compiler_abi_version,
        bundle_digest,
        module_path,
        semantic_hash,
        exports,
        slots,
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
        IfaceType::Bytes => "Bytes".to_string(),
        IfaceType::FileHandle => "FileHandle".to_string(),
        IfaceType::ResourceHandle => "ResourceHandle".to_string(),
        IfaceType::HostResource => "HostResource".to_string(),
        IfaceType::Fault => "Fault".to_string(),
        IfaceType::Request => "Request".to_string(),
        IfaceType::PolicyTable => "PolicyTable".to_string(),
        IfaceType::Vm => "Vm".to_string(),
        IfaceType::Digest => "Digest".to_string(),
        IfaceType::Var(i) => format!("${i}"),
        IfaceType::Projection { base, assoc, .. } => {
            format!("{}.{}", type_text(base), assoc)
        }
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
        IfaceType::Callback {
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
            let mut out = format!("nonescaping ({}) -> {}", parts.join(", "), type_text(ret));
            if !row.is_empty() {
                out.push_str(" with ");
                out.push_str(&row_text(row));
            }
            out
        }
        IfaceType::Run(t) => format!("Run[{}]", type_text(t)),
        IfaceType::Wait(t) => format!("Wait[{}]", type_text(t)),
        IfaceType::PendingCall(a, r) => {
            format!("PendingCall[{}, {}]", type_text(a), type_text(r))
        }
        IfaceType::Op(op, f) => format!("Op[op{}, {}]", op, type_text(f)),
        IfaceType::Handle(m, r) => format!("Handle[{}, {}]", type_text(m), type_text(r)),
        IfaceType::VmSnapshot => "VmSnapshot".to_string(),
        IfaceType::RunSnapshot(t) => format!("RunSnapshot[{}]", type_text(t)),
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
            if class.is_final {
                out.push_str("final ");
            }
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
        IfaceItem::Interface(interface) => format!(
            "[{} type, {} effect, {} associated, {} methods]",
            interface.type_params,
            interface.effect_params,
            interface.associated.len(),
            interface.methods.len()
        ),
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
