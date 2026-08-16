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
const DOMAIN: &[u8] = b"lm-value-digest-v1\0";

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

/// The verified semantic identity of transferred code and classes.
///
/// A closure holds a numeric function slot and an instance holds a
/// numeric class slot. Both slots belong to one linked program, so
/// the canonical encoding replaces them with the definition hash the
/// identity layer proved.
pub trait CodeIdentity {
    /// The definition hash of one function slot.
    fn func_hash(&self, func: u32) -> Result<[u8; 32], FaultCode>;
    /// The definition hash of one class slot.
    fn class_hash(&self, class: u32) -> Result<[u8; 32], FaultCode>;
}

/// The digest hash function.
///
/// Specification 10.3 names BLAKE3-256. The workspace hand-rolls its
/// hashes and takes no external dependency, so the interim function
/// is the SHA-256 of `lm-abi`. The encoding above is hash-agnostic:
/// only this one call changes when BLAKE3 lands. `docs/notes/week7.md`
/// records the open question.
pub fn hash(bytes: &[u8]) -> [u8; 32] {
    lm_abi::sha256(bytes)
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
    codes: &dyn CodeIdentity,
    limits: &GraphLimits,
) -> Result<[u8; 32], FaultCode> {
    let roots: Vec<ObjRef> = value.as_obj().into_iter().collect();
    walk(heap, scratch, &roots, limits, &mut DigestCheck { heap })?;
    let order = scratch.order();
    let mut out: Vec<u8> = Vec::with_capacity(DOMAIN.len() + 64 + order.len() * 16);
    out.extend_from_slice(DOMAIN);
    encode_value(&mut out, value, scratch)?;
    count(&mut out, order.len())?;
    for r in order {
        encode_object(&mut out, heap.get(*r), scratch, codes)?;
    }
    Ok(hash(&out))
}

/// Write one value. An object becomes its traversal ordinal, so the
/// encoding records sharing and cycles without repeating a subgraph.
fn encode_value(out: &mut Vec<u8>, value: Value, scratch: &GraphScratch) -> Result<(), FaultCode> {
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
        Value::Op(slot) => {
            // An operation crosses by manifest identity, never by
            // its dense slot.
            out.push(V_OP);
            out.extend_from_slice(&lm_abi::op_identity(slot));
        }
        Value::Obj(r) => {
            out.push(V_REF);
            out.extend_from_slice(&scratch.ordinal(r.slot).to_le_bytes());
        }
        Value::Uninit => {
            // A field without a first assignment has no canonical
            // encoding.
            return Err(FaultCode::BoundaryViolation);
        }
    }
    Ok(())
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
    scratch: &GraphScratch,
    codes: &dyn CodeIdentity,
) -> Result<(), FaultCode> {
    out.push(object.tag());
    match object {
        Object::Str(text) => {
            count(out, text.len())?;
            out.extend_from_slice(text.as_bytes());
        }
        Object::Instance { class, fields } => {
            out.extend_from_slice(&codes.class_hash(*class)?);
            count(out, fields.len())?;
            for field in fields {
                encode_value(out, *field, scratch)?;
            }
        }
        Object::List { items } | Object::Tuple { items } => {
            count(out, items.len())?;
            for item in items {
                encode_value(out, *item, scratch)?;
            }
        }
        Object::Map { entries, .. } => {
            // Insertion order, key before value. The derived lookup
            // index never enters the encoding.
            count(out, entries.len())?;
            for (key, value) in entries {
                encode_value(out, *key, scratch)?;
                encode_value(out, *value, scratch)?;
            }
        }
        Object::Closure { func, captures } => {
            out.extend_from_slice(&codes.func_hash(*func)?);
            count(out, captures.len())?;
            for capture in captures {
                encode_value(out, *capture, scratch)?;
            }
        }
        Object::NativeFault { code, message, op } => {
            let name = code.to_string();
            count(out, name.len())?;
            out.extend_from_slice(name.as_bytes());
            count(out, message.len())?;
            out.extend_from_slice(message.as_bytes());
            match op {
                None => out.push(0),
                Some(slot) => {
                    out.push(1);
                    out.extend_from_slice(&lm_abi::op_identity(*slot));
                }
            }
        }
        Object::NativeDigest(bytes) => out.extend_from_slice(bytes),
        // The walk rejected every other shape as nondigestible.
        _ => return Err(FaultCode::BoundaryViolation),
    }
    Ok(())
}
