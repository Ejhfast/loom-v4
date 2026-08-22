//! The canonical digest encoding.
//!
//! The encoder writes one deterministic byte string for a frozen
//! graph. It reads the traversal order from the engine, so field,
//! index, and insertion order come from the one shape walker. An
//! object appears once, at its ordinal; every later encounter is a
//! back-reference to that ordinal.
//!
//! Nothing in this module names a numeric code slot or a numeric
//! class slot: both cross as the verified semantic hash the caller
//! supplies. A digest therefore stays equal across allocation orders,
//! across heaps, and across host process runs.

use crate::engine::{walk, GraphLimits, Visitor};
use lm_abi::FaultCode;
use lm_heap::{GraphScratch, Heap, Object};
use lm_value::{ObjRef, Value};

/// The domain separator of the canonical value encoding. A change to
/// the encoding must change this string.
const DOMAIN: &[u8] = b"lm-value-digest-v3\0";

/// Value tags inside the canonical encoding.
const V_UNIT: u8 = 0x00;
const V_BOOL: u8 = 0x01;
const V_INT: u8 = 0x02;
const V_OP: u8 = 0x03;
/// A reference to an object, written as its traversal ordinal. The
/// tag is the back-reference marker of specification 10.3: the first
/// encounter defines the ordinal, and every later encounter repeats
/// it.
const V_REF: u8 = 0x04;
const V_CHAR: u8 = 0x05;
const V_EMPTY_CASE: u8 = 0x06;
const V_OPTION_SOME: u8 = 0x07;
const V_OPTION_NONE: u8 = 0x08;

/// The static case named by one native `Option` type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestOptionCase {
    Family,
    Some,
    None,
}

/// The semantic form of one native `Option` type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DigestOption {
    pub case: DigestOptionCase,
    pub family: u32,
    pub payload: u32,
}

/// The verified semantic identity of transferred code and classes.
///
/// A closure holds a numeric function slot and an instance holds a
/// numeric class slot. Both slots belong to one linked program, so
/// the canonical encoding replaces them with the definition hash the
/// identity layer proved.
pub trait CodeIdentity {
    /// The stable identity of one operation slot.
    fn op_hash(&self, op: u32) -> Result<[u8; 32], FaultCode> {
        lm_abi::standard_bundle()
            .op_identity(op)
            .ok_or(FaultCode::BoundaryViolation)
    }

    /// The definition hash of one function slot.
    fn func_hash(&self, func: u32) -> Result<[u8; 32], FaultCode>;
    /// The definition hash of one class slot.
    fn class_hash(&self, class: u32) -> Result<[u8; 32], FaultCode>;
    /// The content hash of one closed type slot.
    fn type_hash(&self, ty: u32) -> Result<[u8; 32], FaultCode>;

    /// Resolve one closed type as a native `Option` type.
    fn option_shape(&mut self, _: u32) -> Result<Option<DigestOption>, FaultCode> {
        Ok(None)
    }

    /// Give one static type for each value stored by `object`.
    fn child_types(
        &mut self,
        object: &Object,
        _: Option<u32>,
    ) -> Result<Vec<Option<u32>>, FaultCode> {
        let count = match object {
            Object::Instance { fields, .. } => fields.len(),
            Object::List { items, .. } | Object::Tuple { items } => items.len(),
            Object::Map { entries, .. } => entries.len().saturating_mul(2),
            Object::Closure { captures, .. } => captures.len(),
            Object::DynValue { .. } => 1,
            _ => 0,
        };
        Ok(vec![None; count])
    }
}

/// The digest hash function.
///
/// Specification 10.3 names BLAKE3-256, and this function calls the
/// vendored official `blake3` crate. The scope is the value digest
/// only: bytecode, artifact, interface, and build-cache identity stay
/// on the SHA-256 of `lm-abi`. The `DOMAIN` prefix above gives the
/// domain separation that specification 17.9 asks for.
/// `docs/notes/week7.md` records the decision.
pub fn hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// Hash several byte slices without joining them first.
pub fn hash_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(part);
    }
    *hasher.finalize().as_bytes()
}

/// A visitor that rejects a graph the digest cannot encode.
struct DigestCheck<'h> {
    heap: &'h Heap,
}

impl Visitor for DigestCheck<'_> {
    fn enter(&mut self, r: ObjRef, _: u32, object: &Object) -> Result<(), FaultCode> {
        if !self.heap.is_frozen(r) {
            // A digest of a mutable graph would not stay true.
            return Err(FaultCode::UnsendableValue);
        }
        if !object.shape().digestible {
            // A live resource or a holder-local descriptor.
            return Err(FaultCode::BoundaryViolation);
        }
        Ok(())
    }
}

/// Compute the canonical digest of `value` in `heap`.
pub fn compute(
    heap: &Heap,
    scratch: &mut GraphScratch,
    value: Value,
    expected: Option<u32>,
    codes: &mut dyn CodeIdentity,
    limits: &GraphLimits,
) -> Result<[u8; 32], FaultCode> {
    let roots: Vec<ObjRef> = value.as_obj().into_iter().collect();
    walk(heap, scratch, &roots, limits, &mut DigestCheck { heap })?;
    let order = scratch.order();
    let mut out: Vec<u8> = Vec::with_capacity(DOMAIN.len() + 64 + order.len() * 16);
    let mut expected_by_slot = vec![None; heap.slot_count()];
    out.extend_from_slice(DOMAIN);
    encode_value(
        &mut out,
        value,
        expected,
        &mut expected_by_slot,
        scratch,
        codes,
    )?;
    count(&mut out, order.len())?;
    for r in order {
        let expected = expected_by_slot.get(r.slot as usize).copied().flatten();
        encode_object(
            &mut out,
            heap.get(*r),
            expected,
            &mut expected_by_slot,
            scratch,
            codes,
        )?;
    }
    Ok(hash(&out))
}

/// Write one value. An object becomes its traversal ordinal, so the
/// encoding records sharing and cycles without repeating a subgraph.
fn encode_value(
    out: &mut Vec<u8>,
    value: Value,
    expected: Option<u32>,
    expected_by_slot: &mut [Option<u32>],
    scratch: &GraphScratch,
    codes: &mut dyn CodeIdentity,
) -> Result<(), FaultCode> {
    if let Some(expected) = expected {
        if let Some(option) = codes.option_shape(expected)? {
            return encode_option(out, value, option, expected_by_slot, scratch, codes);
        }
    }
    match value {
        Value::Unit => out.push(V_UNIT),
        Value::Bool(v) => {
            out.push(V_BOOL);
            out.push(u8::from(v));
        }
        Value::Int(v) => {
            out.push(V_INT);
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::Char(value) => {
            out.push(V_CHAR);
            out.extend_from_slice(&u32::from(value).to_le_bytes());
        }
        Value::Op(slot) => {
            // An operation crosses by manifest identity, never by
            // its dense slot.
            out.push(V_OP);
            out.extend_from_slice(&codes.op_hash(slot)?);
        }
        Value::EmptyCase { ty, arm } => {
            out.push(V_EMPTY_CASE);
            out.extend_from_slice(&codes.type_hash(ty)?);
            out.extend_from_slice(&arm.to_le_bytes());
        }
        Value::Obj(r) => {
            out.push(V_REF);
            out.extend_from_slice(&scratch.ordinal(r.slot).to_le_bytes());
            if let Some(expected) = expected {
                let slot = expected_by_slot
                    .get_mut(r.slot as usize)
                    .ok_or(FaultCode::BoundaryViolation)?;
                if slot.is_none() {
                    *slot = Some(expected);
                }
            }
        }
        Value::Callback(_) | Value::Uninit => {
            // A field without a first assignment has no canonical
            // encoding.
            return Err(FaultCode::BoundaryViolation);
        }
    }
    Ok(())
}

/// Write one native `Option` wrapper with its semantic family type.
fn encode_option(
    out: &mut Vec<u8>,
    value: Value,
    option: DigestOption,
    expected_by_slot: &mut [Option<u32>],
    scratch: &GraphScratch,
    codes: &mut dyn CodeIdentity,
) -> Result<(), FaultCode> {
    let stored_none = matches!(
        value,
        Value::EmptyCase { ty, arm: 1 } if ty == option.family
    );
    let is_none = match option.case {
        DigestOptionCase::Family => stored_none,
        DigestOptionCase::Some => false,
        DigestOptionCase::None => true,
    };
    if is_none {
        if !stored_none {
            return Err(FaultCode::BoundaryViolation);
        }
        out.push(V_OPTION_NONE);
        out.extend_from_slice(&codes.type_hash(option.family)?);
        return Ok(());
    }
    if stored_none {
        return Err(FaultCode::BoundaryViolation);
    }
    out.push(V_OPTION_SOME);
    out.extend_from_slice(&codes.type_hash(option.family)?);
    encode_value(
        out,
        value,
        Some(option.payload),
        expected_by_slot,
        scratch,
        codes,
    )
}

/// Write one length prefix.
///
/// Every payload carries its length, so the encoding stays
/// unambiguous. A length past the 32-bit prefix has no encoding, so
/// it rejects instead of wrapping into a different graph.
fn count(out: &mut Vec<u8>, n: usize) -> Result<(), FaultCode> {
    let n = u32::try_from(n).map_err(|_| FaultCode::BoundaryViolation)?;
    out.extend_from_slice(&n.to_le_bytes());
    Ok(())
}

/// Write one object: its shape tag and then its canonical payload.
fn encode_object(
    out: &mut Vec<u8>,
    object: &Object,
    expected: Option<u32>,
    expected_by_slot: &mut [Option<u32>],
    scratch: &GraphScratch,
    codes: &mut dyn CodeIdentity,
) -> Result<(), FaultCode> {
    let child_types = codes.child_types(object, expected)?;
    let mut child_types = child_types.into_iter();
    out.push(object.tag());
    match object {
        Object::Str(text) | Object::Substring(text) => {
            count(out, text.len())?;
            out.extend_from_slice(text.as_bytes());
        }
        // The witness is provenance, not content. A digest that read
        // it would separate two structurally equal values, so the
        // encoding names the class and the fields alone.
        Object::Instance { class, fields, .. } => {
            out.extend_from_slice(&codes.class_hash(*class)?);
            count(out, fields.len())?;
            for field in fields {
                encode_value(
                    out,
                    *field,
                    child_types.next().flatten(),
                    expected_by_slot,
                    scratch,
                    codes,
                )?;
            }
        }
        Object::List { items, .. } | Object::Tuple { items } => {
            count(out, items.len())?;
            for item in items {
                encode_value(
                    out,
                    *item,
                    child_types.next().flatten(),
                    expected_by_slot,
                    scratch,
                    codes,
                )?;
            }
        }
        Object::Map { entries, .. } => {
            // Insertion order, key before value. The derived lookup
            // index never enters the encoding.
            count(out, entries.len())?;
            for entry in entries {
                encode_value(
                    out,
                    entry.key,
                    child_types.next().flatten(),
                    expected_by_slot,
                    scratch,
                    codes,
                )?;
                encode_value(
                    out,
                    entry.value,
                    child_types.next().flatten(),
                    expected_by_slot,
                    scratch,
                    codes,
                )?;
            }
        }
        // The witness stays outside the encoding for the same reason.
        Object::Closure { func, captures, .. } => {
            out.extend_from_slice(&codes.func_hash(*func)?);
            count(out, captures.len())?;
            for capture in captures {
                encode_value(
                    out,
                    *capture,
                    child_types.next().flatten(),
                    expected_by_slot,
                    scratch,
                    codes,
                )?;
            }
        }
        Object::NativeFault {
            code,
            message,
            op,
            trace,
        } => {
            let name = code.to_string();
            count(out, name.len())?;
            out.extend_from_slice(name.as_bytes());
            count(out, message.len())?;
            out.extend_from_slice(message.as_bytes());
            match op {
                None => out.push(0),
                Some(slot) => {
                    out.push(1);
                    out.extend_from_slice(&codes.op_hash(*slot)?);
                }
            }
            count(out, trace.len())?;
            for site in trace {
                out.extend_from_slice(&codes.func_hash(site.function)?);
                out.extend_from_slice(&site.block.to_le_bytes());
                out.extend_from_slice(&site.instruction.to_le_bytes());
            }
        }
        Object::NativeDigest(bytes) => out.extend_from_slice(bytes),
        Object::Bytes(bytes) => {
            count(out, bytes.len())?;
            out.extend_from_slice(bytes);
        }
        Object::DynValue { value, ty } => {
            out.extend_from_slice(&codes.type_hash(*ty)?);
            encode_value(out, *value, Some(*ty), expected_by_slot, scratch, codes)?;
        }
        // The walk rejected every other shape as nondigestible.
        _ => return Err(FaultCode::BoundaryViolation),
    }
    Ok(())
}
