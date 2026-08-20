//! The canonical snapshot codec (specification 17.9).
//!
//! The encoder writes one byte string for one image: a fixed magic and
//! version prefix, a section table in ascending kind order, the
//! section payloads in that same order, and a domain-separated
//! BLAKE3-256 container hash. Fixed fields are little-endian and every
//! count is a canonical LEB128 integer. The encoding names code and
//! classes by verified definition hash, and it names machines and
//! objects by traversal ordinal, so the bytes never depend on a heap
//! slot, a scheduler identifier, or an allocation order.
//!
//! The decoder protects the host from the byte stream, and it does no
//! more than that. It proves the container properties of
//! `docs/specs/sidecar/snapshot-image-admission.md` section 4: the frame, the
//! canonical integers, the section bounds, the container hash, and one
//! `Image` representation for every wire tag. It reads no program, and
//! it establishes no interpreter invariant. `admit` does that, because
//! an editor can build the same invalid states with no container
//! behind them.
//!
//! **The decoder rule.** No count in the container ever sizes an
//! allocation before the reader checks it against the load limits and
//! against the bytes that remain. Every element of every counted list
//! costs at least one byte, so `count > remaining` rejects at once.

use super::{
    AdmissionBudget, Image, ImageBlock, ImageCallback, ImageError, ImageFrame, ImageLimits,
    ImageMachine, ImageMailbox, ImageObject, ImagePending, ImagePolicyCursor, ImageReason,
    ImageRoutedRequest, ImageState, ImageTerminal, ImageWaitEntry, ImageWaitSource, LoadLimits,
    Origin, SnapshotFail, SnapshotImage, FORMAT_VERSION, MAGIC, SECTION_CODE, SECTION_HEADER,
    SECTION_HEAPS, SECTION_MACHINES, SECTION_TYPES,
};
use crate::LoadedModule;
use lm_abi::FaultCode;
use lm_bytecode::closed::{ClosedRow, ClosedType, TypeEnv};
use lm_bytecode::identity::COMPILER_ABI_VERSION;
use lm_heap::{MapIndex, NativeByteBuffer, NativeStringBuilder, Object, StructuralEpoch};
use lm_value::{CallbackRef, ObjRef, TypeEnvId, Value, Witness};
use std::cell::Cell;

/// One aggregate allocation ledger for a decoded container.
#[derive(Debug)]
pub struct DecodeBudget {
    limit: usize,
    used: Cell<usize>,
}

impl DecodeBudget {
    /// Create one allocation ledger with an exact limit.
    pub fn new(limit: usize) -> DecodeBudget {
        DecodeBudget {
            limit,
            used: Cell::new(0),
        }
    }

    /// The logical allocation cost already charged.
    pub fn used(&self) -> usize {
        self.used.get()
    }

    /// The logical allocation cost that remains.
    pub fn remaining(&self) -> usize {
        self.limit.saturating_sub(self.used.get())
    }

    fn charge(&self, bytes: usize, what: &str) -> Result<(), ImageError> {
        let next =
            self.used.get().checked_add(bytes).ok_or_else(|| {
                ImageError::new(ImageReason::LimitExceeded, "decode cost overflow")
            })?;
        if next > self.limit {
            return Err(ImageError::new(
                ImageReason::LimitExceeded,
                format!("the decoded {what} passes the allocation budget"),
            ));
        }
        self.used.set(next);
        Ok(())
    }
}

/// The domain separator of the container hash.
const HASH_DOMAIN: &[u8] = b"lm-snapshot-container-v1\0";

/// The value tags of the canonical encoding.
const V_UNIT: u8 = 0;
const V_BOOL: u8 = 1;
const V_INT: u8 = 2;
const V_OP: u8 = 3;
const V_OBJ: u8 = 4;
const V_UNINIT: u8 = 5;
const V_CHAR: u8 = 6;
const V_EMPTY_CASE: u8 = 7;
const V_CALLBACK: u8 = 8;

/// The container hash of one byte prefix.
pub fn container_hash(prefix: &[u8]) -> [u8; 32] {
    lm_graph::digest::hash_parts(&[HASH_DOMAIN, prefix])
}

fn stored_container_hash(bytes: &[u8]) -> [u8; 32] {
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&bytes[bytes.len() - 32..]);
    hash
}

// ---------------------------------------------------------------
// The writer.
// ---------------------------------------------------------------

struct Out {
    bytes: Vec<u8>,
    limit: usize,
    /// The first operation slot the image named that the manifest has
    /// not.
    ///
    /// An `Image` is editable data, so it may name any slot. The
    /// encoder writes a placeholder identity for such a slot and
    /// reports the slot afterwards, so encoding an invalid image
    /// fails instead of indexing the manifest out of range.
    bad_op: Option<u32>,
}

impl Out {
    fn over_limit(&self) -> bool {
        self.bytes.len() > self.limit
    }

    fn u8(&mut self, v: u8) {
        self.bytes.push(v);
    }

    fn u32(&mut self, v: u32) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    fn i64(&mut self, v: i64) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    fn hash(&mut self, v: &[u8; 32]) {
        self.bytes.extend_from_slice(v);
    }

    /// The identity of one operation slot the image names.
    fn op_identity(&mut self, slot: u32) -> [u8; 32] {
        if slot >= lm_abi::OP_COUNT {
            self.bad_op = self.bad_op.or(Some(slot));
            return [0u8; 32];
        }
        lm_abi::op_identity(slot)
    }

    /// The finished bytes, or the reason the image has no encoding.
    fn into_bytes(self) -> Result<Vec<u8>, SnapshotFail> {
        match self.bad_op {
            None => Ok(self.bytes),
            Some(slot) => Err(SnapshotFail::Fault(
                FaultCode::BoundaryViolation,
                format!("the image names operation slot {slot}, which the manifest has not"),
            )),
        }
    }

    /// One canonical LEB128 unsigned integer.
    fn leb(&mut self, mut v: u64) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.bytes.push(byte);
                return;
            }
            self.bytes.push(byte | 0x80);
        }
    }

    fn str(&mut self, text: &str) {
        self.leb(text.len() as u64);
        self.bytes.extend_from_slice(text.as_bytes());
    }

    fn opt(&mut self, v: Option<u32>) {
        match v {
            None => self.leb(0),
            Some(x) => self.leb(x as u64 + 1),
        }
    }

    fn value(&mut self, v: Value) {
        match v {
            Value::Unit => self.u8(V_UNIT),
            Value::Bool(b) => {
                self.u8(V_BOOL);
                self.u8(u8::from(b));
            }
            Value::Int(i) => {
                self.u8(V_INT);
                self.i64(i);
            }
            Value::Char(value) => {
                self.u8(V_CHAR);
                self.u32(u32::from(value));
            }
            Value::Op(op) => {
                self.u8(V_OP);
                let id = self.op_identity(op);
                self.hash(&id);
            }
            Value::EmptyCase { ty, arm } => {
                self.u8(V_EMPTY_CASE);
                self.leb(ty as u64);
                self.leb(arm as u64);
            }
            Value::Obj(r) => {
                self.u8(V_OBJ);
                self.leb(r.slot as u64);
            }
            Value::Callback(reference) => {
                self.u8(V_CALLBACK);
                self.leb(reference.slot as u64);
            }
            Value::Uninit => self.u8(V_UNINIT),
        }
    }

    fn values(&mut self, values: &[Value]) {
        self.leb(values.len() as u64);
        for v in values {
            self.value(*v);
        }
    }
}

/// Encode one image into canonical container bytes.
///
/// `limit` bounds the container. The writer stops as soon as the
/// buffer passes it, so a runaway world never builds a huge buffer.
pub fn encode(image: &Image, limit: usize) -> Result<Vec<u8>, SnapshotFail> {
    let header = section_header(image);
    let code = section_code(image);
    let types = section_types(image, limit)?;
    let heaps = section_heaps(image, limit)?;
    let machines = section_machines(image, limit)?;
    let payloads = [
        (SECTION_HEADER, header),
        (SECTION_CODE, code),
        (SECTION_TYPES, types),
        (SECTION_HEAPS, heaps),
        (SECTION_MACHINES, machines),
    ];
    // The section table carries three fixed 32-bit fields per entry,
    // so its length depends on the entry count alone. A variable-width
    // offset would depend on the table length that holds it, and that
    // circle has no canonical answer.
    let table_len = 1 + SECTION_ENTRY_BYTES * payloads.len();
    let mut offsets: Vec<u64> = Vec::with_capacity(payloads.len());
    let mut at = (prefix_len() + table_len) as u64;
    for (_, payload) in &payloads {
        offsets.push(at);
        at += payload.len() as u64;
    }
    if at + 32 > limit as u64 || at + 32 > u32::MAX as u64 {
        return Err(SnapshotFail::LimitExceeded);
    }
    let mut out = Out {
        bytes: Vec::new(),
        limit,
        bad_op: None,
    };
    out.bytes.extend_from_slice(&MAGIC);
    out.u32(FORMAT_VERSION);
    out.u32(lm_abi::ABI_VERSION);
    out.u32(COMPILER_ABI_VERSION);
    out.u32(lm_verify::VERIFIER_VERSION);
    // The entry count is one byte for every table this format writes,
    // and the reader proves it.
    out.u8(payloads.len() as u8);
    for (idx, (kind, payload)) in payloads.iter().enumerate() {
        out.u32(*kind);
        out.u32(offsets[idx] as u32);
        out.u32(payload.len() as u32);
    }
    debug_assert_eq!(out.bytes.len(), prefix_len() + table_len);
    for (_, payload) in &payloads {
        out.bytes.extend_from_slice(payload);
        if out.over_limit() {
            return Err(SnapshotFail::LimitExceeded);
        }
    }
    let hash = container_hash(&out.bytes);
    out.bytes.extend_from_slice(&hash);
    if out.over_limit() {
        return Err(SnapshotFail::LimitExceeded);
    }
    out.into_bytes()
}

/// The fixed prefix length: magic plus four version fields.
fn prefix_len() -> usize {
    MAGIC.len() + 4 * 4
}

/// The fixed byte length of one section-table entry: kind, offset,
/// and length, each a little-endian 32-bit field.
const SECTION_ENTRY_BYTES: usize = 12;

fn section_header(image: &Image) -> Vec<u8> {
    let mut out = Out {
        bytes: Vec::new(),
        limit: usize::MAX,
        bad_op: None,
    };
    out.leb(image.machines.len() as u64);
    // The root ordinal. The traversal starts at the root, so it is
    // always zero; the reader still checks it.
    out.leb(0);
    out.hash(&image.module_semantic);
    out.hash(&image.result_type);
    out.bytes
}

fn section_code(image: &Image) -> Vec<u8> {
    let mut out = Out {
        bytes: Vec::new(),
        limit: usize::MAX,
        bad_op: None,
    };
    out.leb(image.funcs.len() as u64);
    for (slot, hash) in &image.funcs {
        out.leb(*slot as u64);
        out.hash(hash);
    }
    out.leb(image.classes.len() as u64);
    for (slot, hash) in &image.classes {
        out.leb(*slot as u64);
        out.hash(hash);
    }
    out.bytes
}

/// The closed type table and the type environment table.
///
/// A node names a class by its numeric slot and an effect name by its
/// module string slot, exactly as a heap object names a class. The
/// code manifest carries the definition hash of every class the image
/// names, and admission proves every slot.
fn section_types(image: &Image, limit: usize) -> Result<Vec<u8>, SnapshotFail> {
    let mut out = Out {
        bytes: Vec::new(),
        limit,
        bad_op: None,
    };
    out.leb(image.types.len() as u64);
    for node in &image.types {
        encode_closed_type(&mut out, node);
        if out.over_limit() {
            return Err(SnapshotFail::LimitExceeded);
        }
    }
    // Ordinal zero is the empty environment. It carries no payload, so
    // the table writes the entries after it.
    out.leb(image.envs.len().saturating_sub(1) as u64);
    for env in image.envs.iter().skip(1) {
        out.leb(env.types.len() as u64);
        for ty in &env.types {
            out.leb(*ty as u64);
        }
        out.leb(env.rows.len() as u64);
        for row in &env.rows {
            encode_row(&mut out, row);
        }
        if out.over_limit() {
            return Err(SnapshotFail::LimitExceeded);
        }
    }
    out.into_bytes()
}

fn encode_row(out: &mut Out, row: &ClosedRow) {
    out.leb(row.len() as u64);
    for slot in row {
        out.leb(*slot as u64);
    }
}

fn encode_closed_type(out: &mut Out, node: &ClosedType) {
    out.u8(lm_bytecode::closed::tag_of(node));
    let list = |out: &mut Out, ids: &[u32]| {
        out.leb(ids.len() as u64);
        for id in ids {
            out.leb(*id as u64);
        }
    };
    match node {
        ClosedType::Class(c) => out.leb(*c as u64),
        ClosedType::Inst(c, args) => {
            out.leb(*c as u64);
            list(out, args);
        }
        ClosedType::List(e) | ClosedType::Vm(e) | ClosedType::Wait(e) | ClosedType::Snapshot(e) => {
            out.leb(*e as u64)
        }
        ClosedType::Map(a, b) | ClosedType::PendingCall(a, b) | ClosedType::Handle(a, b) => {
            out.leb(*a as u64);
            out.leb(*b as u64);
        }
        ClosedType::Tuple(elems) => list(out, elems),
        ClosedType::Fn(params, muts, ret, row) | ClosedType::Callback(params, muts, ret, row) => {
            out.leb(params.len() as u64);
            for (param, mutable) in params.iter().zip(muts.iter()) {
                out.u8(u8::from(*mutable));
                out.leb(*param as u64);
            }
            out.leb(*ret as u64);
            encode_row(out, row);
        }
        ClosedType::Op(op, f) => {
            let id = out.op_identity(*op);
            out.hash(&id);
            out.leb(*f as u64);
        }
        _ => {}
    }
}

fn section_heaps(image: &Image, limit: usize) -> Result<Vec<u8>, SnapshotFail> {
    let mut out = Out {
        bytes: Vec::new(),
        limit,
        bad_op: None,
    };
    for machine in &image.machines {
        out.leb(machine.objects.len() as u64);
        for entry in &machine.objects {
            out.u8(u8::from(entry.frozen));
            encode_object(&mut out, &entry.object);
            if out.over_limit() {
                return Err(SnapshotFail::LimitExceeded);
            }
        }
    }
    out.into_bytes()
}

fn encode_object(out: &mut Out, object: &Object) {
    out.u8(object.tag());
    match object {
        Object::Str(text) => out.str(text),
        Object::Instance { class, fields, env } => {
            out.leb(*class as u64);
            out.leb(env.env().0 as u64);
            out.values(fields);
        }
        Object::List { items, epoch } => {
            out.u64(u64::from(epoch.0));
            out.values(items);
        }
        Object::Map { entries, index } => {
            out.u64(u64::from(index.epoch.0));
            out.leb(entries.len() as u64);
            for (key, value) in entries {
                out.value(*key);
                out.value(*value);
            }
        }
        Object::Tuple { items } => out.values(items),
        Object::Closure {
            func,
            captures,
            env,
        } => {
            out.leb(*func as u64);
            out.leb(env.env().0 as u64);
            out.values(captures);
        }
        Object::StrBuilder(builder) => match builder.buffer() {
            Some(text) => {
                out.u8(1);
                out.str(text);
            }
            None => out.u8(0),
        },
        Object::ByteBuf(bytes) => {
            if let Some(bytes) = bytes.buffer() {
                out.u8(1);
                out.leb(bytes.len() as u64);
                out.bytes.extend_from_slice(bytes);
            } else {
                out.u8(0);
            }
        }
        Object::Bytes(bytes) => {
            out.leb(bytes.len() as u64);
            out.bytes.extend_from_slice(bytes);
        }
        Object::Substring(text) => out.str(text),
        Object::NativeVm { vm } | Object::NativeTable { vm } => out.leb(*vm as u64),
        Object::NativeRequest { vm, ordinal } => {
            out.leb(*vm as u64);
            out.u64(*ordinal);
        }
        Object::NativeCall { vm, ordinal, op } => {
            out.leb(*vm as u64);
            out.u64(*ordinal);
            let id = out.op_identity(*op);
            out.hash(&id);
        }
        Object::NativeFault { code, message, op } => {
            out.str(&code.to_string());
            out.str(message);
            match op {
                None => out.u8(0),
                Some(slot) => {
                    out.u8(1);
                    let id = out.op_identity(*slot);
                    out.hash(&id);
                }
            }
        }
        Object::NativeHandle { proc, generation } => {
            out.leb(*proc as u64);
            out.u32(*generation);
        }
        Object::NativeDigest(bytes) => out.hash(bytes),
        Object::NativeSnapshot(image) => {
            out.leb(image.len() as u64);
            out.bytes.extend_from_slice(image);
        }
        Object::NativeFileHandle { resource } => out.u64(*resource),
        Object::NativeResourceHandle { surface, resource } => {
            out.leb(*surface as u64);
            out.u64(*resource);
        }
        Object::NativeWait { owner, token } => {
            out.leb(*owner as u64);
            out.u64(*token);
        }
        Object::NativeTcpStream { resource }
        | Object::NativeTcpListener { resource }
        | Object::NativeTlsStream { resource } => {
            out.u64(*resource);
        }
    }
}

fn section_machines(image: &Image, limit: usize) -> Result<Vec<u8>, SnapshotFail> {
    let mut out = Out {
        bytes: Vec::new(),
        limit,
        bad_op: None,
    };
    for machine in &image.machines {
        out.opt(machine.parent);
        out.u8(machine.state.tag());
        let mut flags = 0u8;
        if machine.scheduler_owned {
            flags |= 1;
        }
        if machine.paused {
            flags |= 2;
        }
        if machine.is_proc {
            flags |= 4;
        }
        out.u8(flags);
        // The machine witness: the body function and the environment
        // of its activation.
        out.opt(machine.body_func);
        out.leb(machine.witness as u64);
        out.u32(machine.generation);
        out.u64(machine.fuel);
        out.u64(machine.next_ordinal);
        out.u64(machine.next_wait);
        out.leb(machine.waits.len() as u64);
        for wait in &machine.waits {
            out.u64(wait.token);
            out.u8(u8::from(wait.linked));
            match wait.source {
                ImageWaitSource::Receive => out.u8(0),
                ImageWaitSource::Drive { target } => {
                    out.u8(1);
                    out.leb(target as u64);
                }
                ImageWaitSource::Choice { first, second } => {
                    out.u8(2);
                    out.u64(first);
                    out.u64(second);
                }
            }
        }
        out.u32(machine.children);
        encode_limits(&mut out, &machine.limits);
        out.leb(machine.callbacks.len() as u64);
        for callback in &machine.callbacks {
            out.leb(callback.func as u64);
            out.values(&callback.captures);
            out.leb(callback.env as u64);
            out.leb(callback.owner_depth as u64);
        }
        out.leb(machine.frames.len() as u64);
        for frame in &machine.frames {
            out.leb(frame.func as u64);
            out.leb(frame.block as u64);
            out.leb(frame.ip as u64);
            out.leb(frame.base_local as u64);
            out.leb(frame.base_operand as u64);
            match frame.closure {
                None => out.u8(0),
                Some(value) => {
                    out.u8(1);
                    out.value(value);
                }
            }
            out.leb(frame.env as u64);
        }
        out.values(&machine.locals);
        out.values(&machine.operands);
        out.leb(machine.literals.len() as u64);
        for literal in &machine.literals {
            out.opt(*literal);
        }
        out.opt(machine.start_body);
        match &machine.pending {
            None => out.u8(0),
            Some(pending) => {
                out.u8(1);
                let id = out.op_identity(pending.op);
                out.hash(&id);
                out.values(&pending.args);
                out.u64(pending.ordinal);
            }
        }
        out.opt(machine.nested);
        match machine.routed {
            None => out.u8(0),
            Some(route) => {
                out.u8(1);
                out.leb(route.target as u64);
                match route.cursor {
                    ImagePolicyCursor::Table(table) => {
                        out.u8(0);
                        out.leb(table as u64);
                    }
                    ImagePolicyCursor::Binding => out.u8(1),
                    ImagePolicyCursor::Root => out.u8(2),
                }
            }
        }
        match &machine.terminal {
            None => out.u8(0),
            Some(ImageTerminal::Done(value)) => {
                out.u8(1);
                out.value(*value);
            }
            Some(ImageTerminal::Fault(rec)) => {
                out.u8(2);
                out.str(&rec.code.to_string());
                out.str(&rec.message);
                match rec.op {
                    None => out.u8(0),
                    Some(slot) => {
                        out.u8(1);
                        let id = out.op_identity(slot);
                        out.hash(&id);
                    }
                }
            }
        }
        out.u32(machine.mailbox.limit);
        out.u8(u8::from(machine.mailbox.closed));
        out.u64(machine.mailbox.accepted);
        out.u64(machine.mailbox.delivered);
        out.values(&machine.mailbox.queue);
        match machine.block {
            None => out.u8(0),
            Some(ImageBlock::Receive) => out.u8(1),
            Some(ImageBlock::Send { target }) => {
                out.u8(2);
                out.leb(target as u64);
            }
            Some(ImageBlock::Done { target }) => {
                out.u8(3);
                out.leb(target as u64);
            }
            Some(ImageBlock::Wait { token }) => {
                out.u8(4);
                out.u64(token);
            }
            Some(ImageBlock::Snapshot {
                target,
                remaining,
                retry,
            }) => {
                out.u8(5);
                out.leb(target as u64);
                out.u64(remaining);
                out.u8(u8::from(retry));
            }
        }
        if out.over_limit() {
            return Err(SnapshotFail::LimitExceeded);
        }
    }
    out.into_bytes()
}

fn encode_limits(out: &mut Out, limits: &ImageLimits) {
    out.u64(limits.fuel);
    out.u32(limits.max_frames);
    out.u32(limits.max_stack_values);
    out.u64(limits.heap_bytes);
    out.u64(limits.max_objects);
    out.u64(limits.max_edges);
    out.u64(limits.max_graph_bytes);
    out.u64(limits.max_work);
    out.u32(limits.max_children);
    out.u32(limits.max_resources);
    out.u32(limits.mailbox_limit);
}

// ---------------------------------------------------------------
// The reader.
// ---------------------------------------------------------------

struct Cursor<'b, 'd> {
    bytes: &'b [u8],
    at: usize,
    /// The end of the section the reader is inside.
    end: usize,
    budget: &'d DecodeBudget,
}

type Read<T> = Result<T, ImageError>;

fn err<T>(reason: ImageReason, detail: impl Into<String>) -> Read<T> {
    Err(ImageError::new(reason, detail))
}

impl<'b, 'd> Cursor<'b, 'd> {
    fn new(bytes: &'b [u8], budget: &'d DecodeBudget) -> Cursor<'b, 'd> {
        Cursor {
            bytes,
            at: 0,
            end: bytes.len(),
            budget,
        }
    }

    fn remaining(&self) -> usize {
        self.end - self.at
    }

    fn take(&mut self, n: usize) -> Read<&'b [u8]> {
        if n > self.remaining() {
            return err(
                ImageReason::Truncated,
                format!(
                    "the reader needs {n} more bytes and holds {}",
                    self.remaining()
                ),
            );
        }
        let out = &self.bytes[self.at..self.at + n];
        self.at += n;
        Ok(out)
    }

    fn u8(&mut self) -> Read<u8> {
        Ok(self.take(1)?[0])
    }

    fn flag(&mut self) -> Read<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => err(
                ImageReason::NonCanonicalInteger,
                format!("a flag byte is {other}, which is not 0 or 1"),
            ),
        }
    }

    fn u32(&mut self) -> Read<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Read<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn i64(&mut self) -> Read<i64> {
        Ok(self.u64()? as i64)
    }

    fn hash(&mut self) -> Read<[u8; 32]> {
        let b = self.take(32)?;
        let mut out = [0u8; 32];
        out.copy_from_slice(b);
        Ok(out)
    }

    /// One canonical LEB128 unsigned integer.
    ///
    /// The reader rejects a non-minimal encoding and an encoding past
    /// 64 bits, so one integer has exactly one byte string.
    fn leb(&mut self) -> Read<u64> {
        let mut value: u64 = 0;
        let mut shift = 0u32;
        loop {
            let byte = self.u8()?;
            let payload = (byte & 0x7f) as u64;
            if shift >= 64 || (shift == 63 && payload > 1) {
                return err(
                    ImageReason::NonCanonicalInteger,
                    "an integer does not fit in 64 bits",
                );
            }
            value |= payload << shift;
            if byte & 0x80 == 0 {
                // A multi-byte encoding whose last byte is zero is not
                // minimal.
                if shift > 0 && payload == 0 {
                    return err(
                        ImageReason::NonCanonicalInteger,
                        "an integer is not in minimal LEB128 form",
                    );
                }
                return Ok(value);
            }
            shift += 7;
        }
    }

    /// One count. Every element of a counted list costs at least one
    /// byte, so a count past the remaining bytes rejects before any
    /// allocation. `cap` is the load limit for this list.
    fn count(&mut self, cap: u64, what: &str) -> Read<usize> {
        let n = self.leb()?;
        if n > cap {
            return err(
                ImageReason::LimitExceeded,
                format!("the {what} count {n} passes the load limit {cap}"),
            );
        }
        if n > self.remaining() as u64 {
            return err(
                ImageReason::Truncated,
                format!(
                    "the {what} count {n} passes the {} bytes that remain",
                    self.remaining()
                ),
            );
        }
        Ok(n as usize)
    }

    fn str(&mut self, cap: u32) -> Read<String> {
        let n = self.count(cap as u64, "string byte")?;
        let bytes = self.take(n)?;
        let copy = self.copy_bytes(bytes, "string")?;
        String::from_utf8(copy)
            .map_err(|_| ImageError::new(ImageReason::Layout, "a string is not valid UTF-8"))
    }

    /// Reserve one decoded vector before its first element.
    fn vector<T>(&self, count: usize, what: &str) -> Read<Vec<T>> {
        let bytes = std::mem::size_of::<T>()
            .checked_mul(count)
            .ok_or_else(|| ImageError::new(ImageReason::LimitExceeded, "decode cost overflow"))?;
        self.budget.charge(bytes, what)?;
        let mut values = Vec::new();
        values.try_reserve_exact(count).map_err(|_| {
            ImageError::new(
                ImageReason::LimitExceeded,
                format!("the decoded {what} allocation failed"),
            )
        })?;
        Ok(values)
    }

    /// Copy decoded bytes through a fallible reservation.
    fn copy_bytes(&self, source: &[u8], what: &str) -> Read<Vec<u8>> {
        let mut bytes = self.vector(source.len(), what)?;
        bytes.extend_from_slice(source);
        Ok(bytes)
    }

    /// One optional ordinal. `count` is the exclusive upper bound, so
    /// a list of zero entries admits `None` alone.
    fn opt(&mut self, count: u64, what: &str) -> Read<Option<u32>> {
        let raw = self.leb()?;
        if raw == 0 {
            return Ok(None);
        }
        let value = raw - 1;
        if value >= count {
            return err(
                ImageReason::Reference,
                format!("the {what} ordinal {value} names no entry of {count}"),
            );
        }
        Ok(Some(value as u32))
    }
}

/// The context the decoder carries: the load limits and the counts it
/// already read.
struct Ctx {
    limits: LoadLimits,
    machine_count: u32,
    /// The number of type environment entries the image carries.
    env_count: u32,
}

/// Load one external snapshot container.
///
/// The call decodes the container, admits the result against the exact
/// verified module, and seals the admitted image with its canonical
/// bytes. It runs once per byte string; a later restore reads the
/// admitted image and repeats nothing (specification 17.8).
pub fn load_external(
    bytes: &[u8],
    loaded: &LoadedModule,
    limits: LoadLimits,
) -> Result<SnapshotImage, ImageError> {
    if bytes.len() > limits.max_bytes {
        return err(
            ImageReason::LimitExceeded,
            format!(
                "the container holds {} bytes and the load limit is {}",
                bytes.len(),
                limits.max_bytes
            ),
        );
    }
    let decode_budget = DecodeBudget::new(limits.max_alloc_bytes);
    let (image, hash) = decode_inner(bytes, limits, &decode_budget)?;
    decode_budget.charge(bytes.len(), "container copy")?;
    let mut admission_budget = AdmissionBudget::default();
    let identity = super::admit::prove(&image, loaded, &mut admission_budget)?;
    // The decoder accepts one byte string for one image, so the bytes
    // it received are the canonical bytes of the admitted image.
    let mut owned = Vec::new();
    owned.try_reserve_exact(bytes.len()).map_err(|_| {
        ImageError::new(
            ImageReason::LimitExceeded,
            "the container copy allocation failed",
        )
    })?;
    owned.extend_from_slice(bytes);
    Ok(SnapshotImage {
        bytes: std::sync::Arc::new(owned),
        world: std::sync::Arc::new(image),
        hash,
        identity,
        origin: Origin::ExternalContainer,
    })
}

/// Seal one image one consistent cut produced.
///
/// The cut copies a stopped verified world, so the admission invariant
/// holds by construction and this path runs no graph check
/// (specification section 7.2). The constructor stays inside the
/// snapshot module, so no host code can promote an arbitrary image
/// through it.
pub(super) fn from_trusted_capture(
    image: Image,
    identity: super::AdmissionIdentity,
    limit: usize,
) -> Result<SnapshotImage, SnapshotFail> {
    let bytes = encode(&image, limit)?;
    let hash = stored_container_hash(&bytes);
    Ok(SnapshotImage {
        bytes: std::sync::Arc::new(bytes),
        world: std::sync::Arc::new(image),
        hash,
        identity,
        origin: Origin::TrustedCapture,
    })
}

/// Seal one admitted image with its canonical bytes.
///
/// `admit` calls this after the proof succeeds, so the bytes and the
/// admitted world always agree.
pub(super) fn seal_admitted(
    image: Image,
    identity: super::AdmissionIdentity,
    limit: usize,
) -> Result<SnapshotImage, ImageError> {
    // The encoder has two failures, and they break two rules. A
    // container past its byte limit breaks the limit rule. An
    // operation slot the manifest has not breaks the code rule, and
    // reporting it as a limit names the wrong rule.
    let bytes = encode(&image, limit).map_err(|error| match error {
        SnapshotFail::LimitExceeded => ImageError::admission(
            ImageReason::LimitExceeded,
            "the admitted image passes the container byte limit",
        ),
        SnapshotFail::Fault(_, detail) => ImageError::admission(ImageReason::Code, detail),
        SnapshotFail::ResourceActive { kind, .. } => ImageError::admission(
            ImageReason::State,
            format!("the admitted image holds a live {kind} attachment"),
        ),
    })?;
    let hash = stored_container_hash(&bytes);
    Ok(SnapshotImage {
        bytes: std::sync::Arc::new(bytes),
        world: std::sync::Arc::new(image),
        hash,
        identity,
        origin: Origin::ExternalContainer,
    })
}

/// Decode one container into editable image data.
///
/// The call proves container properties alone. It reads no program and
/// it establishes no interpreter invariant, so its result is ordinary
/// `Image` data that `admit` must still prove.
pub fn decode(bytes: &[u8], limits: LoadLimits) -> Result<Image, ImageError> {
    let mut budget = DecodeBudget::new(limits.max_alloc_bytes);
    decode_with_budget(bytes, limits, &mut budget)
}

/// Decode one container with a caller-owned allocation ledger.
pub fn decode_with_budget(
    bytes: &[u8],
    limits: LoadLimits,
    budget: &mut DecodeBudget,
) -> Result<Image, ImageError> {
    decode_inner(bytes, limits, budget).map(|(image, _)| image)
}

fn decode_inner(
    bytes: &[u8],
    limits: LoadLimits,
    budget: &DecodeBudget,
) -> Result<(Image, [u8; 32]), ImageError> {
    if bytes.len() > limits.max_bytes {
        return err(
            ImageReason::LimitExceeded,
            format!(
                "the container holds {} bytes and the load limit is {}",
                bytes.len(),
                limits.max_bytes
            ),
        );
    }
    if bytes.len() < prefix_len() + 32 {
        return err(
            ImageReason::Truncated,
            "the container is shorter than its frame",
        );
    }
    budget.charge(std::mem::size_of::<Image>(), "image record")?;
    let mut cur = Cursor::new(bytes, budget);
    let magic = cur.take(MAGIC.len())?;
    if magic != MAGIC {
        return err(ImageReason::Magic, "the container magic is not `LMSNAP`");
    }
    let format = cur.u32()?;
    let abi_version = cur.u32()?;
    let compiler_abi = cur.u32()?;
    let verifier_version = cur.u32()?;
    if format != FORMAT_VERSION {
        return err(
            ImageReason::Version,
            format!(
                "the container format version is {format}, and this build reads {FORMAT_VERSION}"
            ),
        );
    }
    if abi_version != lm_abi::ABI_VERSION
        || compiler_abi != COMPILER_ABI_VERSION
        || verifier_version != lm_verify::VERIFIER_VERSION
    {
        return err(
            ImageReason::Version,
            "the container names another ABI, compiler, or verifier version",
        );
    }
    // The container hash covers every byte before it. It is checked
    // before any section is read, so a damaged container never reaches
    // the structural rules.
    let body = bytes.len() - 32;
    let mut stored = [0u8; 32];
    stored.copy_from_slice(&bytes[body..]);
    if container_hash(&bytes[..body]) != stored {
        return err(
            ImageReason::ContainerHash,
            "the stored container hash does not match the bytes",
        );
    }
    // The section table. The kinds are ascending, the payloads follow
    // the table without a gap, and the last payload ends at the hash.
    let want = [
        SECTION_HEADER,
        SECTION_CODE,
        SECTION_TYPES,
        SECTION_HEAPS,
        SECTION_MACHINES,
    ];
    let count = cur.u8()? as usize;
    if count != want.len() {
        return err(
            ImageReason::SectionBounds,
            format!(
                "the section table holds {count} entries and the format writes {}",
                want.len()
            ),
        );
    }
    let mut table: Vec<(u32, u64, u64)> = cur.vector(count, "section table")?;
    for _ in 0..count {
        let kind = cur.u32()?;
        let offset = cur.u32()? as u64;
        let length = cur.u32()? as u64;
        table.push((kind, offset, length));
    }
    let table_end = cur.at;
    if table.iter().map(|e| e.0).ne(want.iter().copied()) {
        return err(
            ImageReason::SectionBounds,
            "the section table is not the canonical section list",
        );
    }
    let mut at = table_end as u64;
    for (kind, offset, length) in &table {
        if *offset != at {
            return err(
                ImageReason::SectionBounds,
                format!("section {kind} starts at {offset} and the previous section ends at {at}"),
            );
        }
        at += *length;
        if at > body as u64 {
            return err(
                ImageReason::SectionBounds,
                format!("section {kind} reaches past the container"),
            );
        }
    }
    if at != body as u64 {
        return err(
            ImageReason::Trailing,
            "bytes remain between the last section and the container hash",
        );
    }
    let section = |idx: usize| -> Cursor<'_, '_> {
        let (_, offset, length) = table[idx];
        Cursor {
            bytes,
            at: offset as usize,
            end: (offset + length) as usize,
            budget,
        }
    };
    // Section 1: the header. The machine count is checked against the
    // load limit before any machine vector exists.
    let mut header = section(0);
    let machine_count = header.count(limits.max_machines as u64, "machine")?;
    let root = header.leb()?;
    if root != 0 {
        // One image has one byte string, so the canonical root ordinal
        // is a container rule.
        return err(
            ImageReason::SectionBounds,
            "the root machine ordinal of a canonical image is zero",
        );
    }
    let module_semantic = header.hash()?;
    let result_type = header.hash()?;
    if header.remaining() != 0 {
        return err(
            ImageReason::Trailing,
            "the header section holds extra bytes",
        );
    }
    // Section 2: the code manifest. The decoder reads no program, so
    // it proves the canonical ascending order alone. Admission proves
    // that every slot exists and carries its definition hash.
    let mut code = section(1);
    let func_count = code.count(limits.max_code_slots as u64, "function")?;
    let mut funcs: Vec<(u32, [u8; 32])> = code.vector(func_count, "function manifest")?;
    let mut last: Option<u32> = None;
    for _ in 0..func_count {
        let slot = code.leb()?;
        let hash = code.hash()?;
        let slot = u32::try_from(slot)
            .map_err(|_| ImageError::new(ImageReason::Code, "a function slot is too large"))?;
        if last.is_some_and(|l| slot <= l) {
            return err(ImageReason::Code, "the function manifest is not ascending");
        }
        last = Some(slot);
        funcs.push((slot, hash));
    }
    let class_count = code.count(limits.max_code_slots as u64, "class")?;
    let mut classes: Vec<(u32, [u8; 32])> = code.vector(class_count, "class manifest")?;
    let mut last: Option<u32> = None;
    for _ in 0..class_count {
        let slot = code.leb()?;
        let hash = code.hash()?;
        let slot = u32::try_from(slot)
            .map_err(|_| ImageError::new(ImageReason::Code, "a class slot is too large"))?;
        if last.is_some_and(|l| slot <= l) {
            return err(ImageReason::Code, "the class manifest is not ascending");
        }
        last = Some(slot);
        classes.push((slot, hash));
    }
    if code.remaining() != 0 {
        return err(ImageReason::Trailing, "the code section holds extra bytes");
    }
    // Section 3: the closed type table and the environment table. Both
    // precede the heaps, because a heap object names an environment by
    // its ordinal here.
    let mut type_section = section(2);
    let (types, envs) = decode_types(&mut type_section, &limits)?;
    if type_section.remaining() != 0 {
        return err(ImageReason::Trailing, "the type section holds extra bytes");
    }
    let ctx = Ctx {
        limits,
        machine_count: machine_count as u32,
        env_count: envs.len() as u32,
    };
    // Section 4: the heaps, one per machine, in ordinal order.
    let mut heaps = section(3);
    let mut all_objects: Vec<Vec<ImageObject>> =
        heaps.vector(machine_count, "machine heap table")?;
    for _ in 0..machine_count {
        let count = heaps.count(ctx.limits.max_objects as u64, "object")?;
        let mut objects: Vec<ImageObject> = heaps.vector(count, "heap object table")?;
        for _ in 0..count {
            let frozen = heaps.flag()?;
            let object = decode_object(&mut heaps, &ctx, count as u32)?;
            objects.push(ImageObject { frozen, object });
        }
        all_objects.push(objects);
    }
    if heaps.remaining() != 0 {
        return err(ImageReason::Trailing, "the heap section holds extra bytes");
    }
    // Section 5: the machine records.
    let mut records = section(4);
    let mut machines: Vec<ImageMachine> = records.vector(machine_count, "machine table")?;
    for objects in all_objects {
        let machine = decode_machine(&mut records, &ctx, objects)?;
        machines.push(machine);
    }
    if records.remaining() != 0 {
        return err(
            ImageReason::Trailing,
            "the machine section holds extra bytes",
        );
    }
    Ok((
        Image {
            format,
            abi_version,
            compiler_abi,
            verifier_version,
            module_semantic,
            result_type,
            funcs,
            classes,
            types,
            envs,
            machines,
        },
        stored,
    ))
}

/// Decode the closed type table and the type environment table.
///
/// Every count is checked against a load limit and against the bytes
/// that remain before it sizes an allocation. Every child index must
/// name an earlier entry, so the table stays a directed acyclic graph
/// and a walk over one node terminates.
fn decode_types(
    cur: &mut Cursor<'_, '_>,
    limits: &LoadLimits,
) -> Read<(Vec<ClosedType>, Vec<TypeEnv>)> {
    let count = cur.count(limits.max_closed_types as u64, "closed type")?;
    let mut types: Vec<ClosedType> = cur.vector(count, "closed type table")?;
    for idx in 0..count {
        let node = decode_closed_type(cur, limits, idx as u32)?;
        types.push(node);
    }
    let env_count = cur.count(limits.max_type_envs as u64, "type environment")?;
    let env_total = env_count
        .checked_add(1)
        .ok_or_else(|| ImageError::new(ImageReason::LimitExceeded, "environment count overflow"))?;
    let mut envs: Vec<TypeEnv> = cur.vector(env_total, "type environment table")?;
    envs.push(TypeEnv::default());
    for _ in 0..env_count {
        let arity = cur.count(limits.max_closed_types as u64, "environment argument")?;
        let mut list: Vec<u32> = cur.vector(arity, "environment arguments")?;
        for _ in 0..arity {
            list.push(closed_ref(cur, count as u32)?);
        }
        let rows_len = cur.count(limits.max_closed_types as u64, "environment row")?;
        let mut rows: Vec<ClosedRow> = cur.vector(rows_len, "environment rows")?;
        for _ in 0..rows_len {
            rows.push(decode_row(cur, limits)?);
        }
        envs.push(TypeEnv { types: list, rows });
    }
    Ok((types, envs))
}

/// One closed type ordinal below `count`.
fn closed_ref(cur: &mut Cursor<'_, '_>, count: u32) -> Read<u32> {
    let id = cur.leb()?;
    if id >= count as u64 {
        return err(
            ImageReason::Reference,
            format!("a closed type ordinal names entry {id} of {count}"),
        );
    }
    Ok(id as u32)
}

fn decode_row(cur: &mut Cursor<'_, '_>, limits: &LoadLimits) -> Read<ClosedRow> {
    let len = cur.count(limits.max_code_slots as u64, "effect row element")?;
    let mut row: ClosedRow = cur.vector(len, "effect row")?;
    for _ in 0..len {
        let slot = u32::try_from(cur.leb()?)
            .map_err(|_| ImageError::new(ImageReason::Code, "an effect name slot is too large"))?;
        row.push(slot);
    }
    Ok(row)
}

/// Decode one closed type node. `at` is the ordinal of the node, and
/// every child must name an earlier ordinal.
fn decode_closed_type(cur: &mut Cursor<'_, '_>, limits: &LoadLimits, at: u32) -> Read<ClosedType> {
    let tag = cur.u8()?;
    let list = |cur: &mut Cursor<'_, '_>| -> Read<Vec<u32>> {
        let len = cur.count(limits.max_closed_types as u64, "closed type argument")?;
        let mut out = cur.vector(len, "closed type arguments")?;
        for _ in 0..len {
            out.push(closed_ref(cur, at)?);
        }
        Ok(out)
    };
    Ok(match tag {
        0 => ClosedType::Unit,
        1 => ClosedType::Bool,
        2 => ClosedType::Int,
        3 => ClosedType::Str,
        6 => ClosedType::Fault,
        7 => ClosedType::Request,
        8 => ClosedType::PolicyTable,
        9 => ClosedType::EmptyVm,
        10 => ClosedType::Digest,
        11 => ClosedType::SnapshotImage,
        12 => ClosedType::Class(class_slot(cur)?),
        13 => {
            let class = class_slot(cur)?;
            ClosedType::Inst(class, list(cur)?)
        }
        14 => ClosedType::List(closed_ref(cur, at)?),
        15 => {
            let k = closed_ref(cur, at)?;
            let v = closed_ref(cur, at)?;
            ClosedType::Map(k, v)
        }
        16 => ClosedType::Tuple(list(cur)?),
        17 => {
            let len = cur.count(limits.max_closed_types as u64, "closed parameter")?;
            let mut params = cur.vector(len, "closed parameters")?;
            let mut muts = cur.vector(len, "parameter markers")?;
            for _ in 0..len {
                muts.push(cur.flag()?);
                params.push(closed_ref(cur, at)?);
            }
            let ret = closed_ref(cur, at)?;
            let row = decode_row(cur, limits)?;
            ClosedType::Fn(params, muts, ret, row)
        }
        18 => ClosedType::Vm(closed_ref(cur, at)?),
        19 => {
            let a = closed_ref(cur, at)?;
            let b = closed_ref(cur, at)?;
            ClosedType::PendingCall(a, b)
        }
        20 => {
            let a = closed_ref(cur, at)?;
            let b = closed_ref(cur, at)?;
            ClosedType::Handle(a, b)
        }
        21 => {
            let op = decode_op(cur)?;
            ClosedType::Op(op, closed_ref(cur, at)?)
        }
        22 => ClosedType::Snapshot(closed_ref(cur, at)?),
        23 => ClosedType::Bytes,
        24 => ClosedType::FileHandle,
        25 => ClosedType::ResourceHandle,
        26 => ClosedType::Wait(closed_ref(cur, at)?),
        27 => {
            let len = cur.count(limits.max_closed_types as u64, "closed callback parameter")?;
            let mut params = cur.vector(len, "closed callback parameters")?;
            let mut muts = cur.vector(len, "callback parameter markers")?;
            for _ in 0..len {
                muts.push(cur.flag()?);
                params.push(closed_ref(cur, at)?);
            }
            let ret = closed_ref(cur, at)?;
            let row = decode_row(cur, limits)?;
            ClosedType::Callback(params, muts, ret, row)
        }
        other => {
            return err(
                ImageReason::Layout,
                format!("the closed type tag {other} is not a canonical tag"),
            )
        }
    })
}

fn class_slot(cur: &mut Cursor<'_, '_>) -> Read<u32> {
    u32::try_from(cur.leb()?)
        .map_err(|_| ImageError::new(ImageReason::Code, "a class slot is too large"))
}

fn decode_value(cur: &mut Cursor<'_, '_>, objects: u32, callbacks: u32) -> Read<Value> {
    let tag = cur.u8()?;
    Ok(match tag {
        V_UNIT => Value::Unit,
        V_BOOL => Value::Bool(cur.flag()?),
        V_INT => Value::Int(cur.i64()?),
        V_OP => {
            let id = cur.hash()?;
            let slot = (0..lm_abi::OP_COUNT).find(|slot| lm_abi::op_identity(*slot) == id);
            match slot {
                Some(slot) => Value::Op(slot),
                None => {
                    return err(
                        ImageReason::Code,
                        "an operation value names no manifest operation",
                    )
                }
            }
        }
        V_OBJ => {
            let ordinal = cur.leb()?;
            if ordinal >= objects as u64 {
                return err(
                    ImageReason::Reference,
                    format!("an object reference names ordinal {ordinal} of {objects}"),
                );
            }
            Value::Obj(ObjRef {
                slot: ordinal as u32,
                generation: 0,
            })
        }
        V_CALLBACK => {
            let ordinal = cur.leb()?;
            if ordinal >= callbacks as u64 {
                return err(
                    ImageReason::Reference,
                    format!("a callback reference names ordinal {ordinal} of {callbacks}"),
                );
            }
            Value::Callback(CallbackRef {
                slot: ordinal as u32,
                generation: 0,
            })
        }
        V_UNINIT => Value::Uninit,
        V_CHAR => {
            let value = cur.u32()?;
            let Some(value) = char::from_u32(value) else {
                return err(ImageReason::Layout, "a Char value is not a Unicode scalar");
            };
            Value::Char(value)
        }
        V_EMPTY_CASE => {
            let ty = u32::try_from(cur.leb()?).map_err(|_| {
                ImageError::new(ImageReason::Layout, "an empty-case type is too large")
            })?;
            let arm = u32::try_from(cur.leb()?).map_err(|_| {
                ImageError::new(ImageReason::Layout, "an empty-case arm is too large")
            })?;
            Value::EmptyCase { ty, arm }
        }
        other => {
            return err(
                ImageReason::Layout,
                format!("the value tag {other} is not a canonical tag"),
            )
        }
    })
}

fn decode_values(
    cur: &mut Cursor<'_, '_>,
    objects: u32,
    callbacks: u32,
    cap: u64,
    what: &str,
) -> Read<Vec<Value>> {
    let count = cur.count(cap, what)?;
    let mut out: Vec<Value> = cur.vector(count, what)?;
    for _ in 0..count {
        out.push(decode_value(cur, objects, callbacks)?);
    }
    Ok(out)
}

fn decode_op(cur: &mut Cursor<'_, '_>) -> Read<u32> {
    let id = cur.hash()?;
    match (0..lm_abi::OP_COUNT).find(|slot| lm_abi::op_identity(*slot) == id) {
        Some(slot) => Ok(slot),
        None => err(
            ImageReason::Code,
            "an operation identity names no manifest operation",
        ),
    }
}

fn decode_fault(cur: &mut Cursor<'_, '_>, limits: &LoadLimits) -> Read<crate::FaultRec> {
    let name = cur.str(limits.max_string_bytes)?;
    let Some(code) = FaultCode::from_name(&name) else {
        return err(
            ImageReason::Layout,
            format!("`{name}` is not a stable fault code"),
        );
    };
    let message = cur.str(limits.max_string_bytes)?;
    let op = match cur.u8()? {
        0 => None,
        1 => Some(decode_op(cur)?),
        other => {
            return err(
                ImageReason::Layout,
                format!("the fault operation tag {other} is not 0 or 1"),
            )
        }
    };
    Ok(crate::FaultRec { code, message, op })
}

fn decode_epoch(cur: &mut Cursor<'_, '_>) -> Read<StructuralEpoch> {
    let epoch = cur.u64()?;
    let Ok(epoch) = u32::try_from(epoch) else {
        return err(
            ImageReason::Layout,
            "a collection epoch is outside its supported range",
        );
    };
    Ok(StructuralEpoch(epoch))
}

fn decode_object(cur: &mut Cursor<'_, '_>, ctx: &Ctx, objects: u32) -> Read<Object> {
    let tag = cur.u8()?;
    let limits = &ctx.limits;
    Ok(match tag {
        0 => Object::Str(cur.str(limits.max_string_bytes)?.into()),
        1 => {
            let class = class_slot(cur)?;
            let env = env_ref(cur, ctx)?;
            let fields = decode_values(cur, objects, 0, limits.max_stack_values as u64, "field")?;
            Object::Instance { class, fields, env }
        }
        2 => Object::List {
            epoch: decode_epoch(cur)?,
            items: decode_values(cur, objects, 0, limits.max_stack_values as u64, "list item")?,
        },
        3 => {
            let epoch = decode_epoch(cur)?;
            let count = cur.count(limits.max_stack_values as u64, "map entry")?;
            let mut entries: Vec<(Value, Value)> = cur.vector(count, "map entries")?;
            for _ in 0..count {
                let key = decode_value(cur, objects, 0)?;
                let value = decode_value(cur, objects, 0)?;
                entries.push((key, value));
            }
            let mut index = MapIndex::default();
            index.epoch = epoch;
            Object::Map { entries, index }
        }
        4 => Object::Tuple {
            items: decode_values(
                cur,
                objects,
                0,
                limits.max_stack_values as u64,
                "tuple item",
            )?,
        },
        5 => {
            let func = cur.leb()?;
            let func = u32::try_from(func)
                .map_err(|_| ImageError::new(ImageReason::Code, "a function slot is too large"))?;
            let env = env_ref(cur, ctx)?;
            let captures =
                decode_values(cur, objects, 0, limits.max_stack_values as u64, "capture")?;
            Object::Closure {
                func,
                captures,
                env,
            }
        }
        6 => match cur.u8()? {
            0 => Object::StrBuilder(NativeStringBuilder::finished()),
            1 => Object::StrBuilder(NativeStringBuilder::from_string(
                cur.str(limits.max_string_bytes)?,
            )),
            _ => {
                return Err(ImageError::new(
                    ImageReason::Code,
                    "a string builder state flag is invalid",
                ));
            }
        },
        7 => match cur.u8()? {
            0 => Object::ByteBuf(NativeByteBuffer::finished()),
            1 => {
                let count = cur.count(limits.max_string_bytes as u64, "buffer byte")?;
                let source = cur.take(count)?;
                Object::ByteBuf(NativeByteBuffer::from_vec(
                    cur.copy_bytes(source, "buffer bytes")?,
                ))
            }
            _ => {
                return Err(ImageError::new(
                    ImageReason::Code,
                    "a byte buffer state flag is invalid",
                ));
            }
        },
        8 => Object::NativeVm {
            vm: machine_ref(cur, ctx)?,
        },
        9 => Object::NativeTable {
            vm: machine_ref(cur, ctx)?,
        },
        10 => {
            let vm = machine_ref(cur, ctx)?;
            let ordinal = cur.u64()?;
            Object::NativeRequest { vm, ordinal }
        }
        11 => {
            let vm = machine_ref(cur, ctx)?;
            let ordinal = cur.u64()?;
            let op = decode_op(cur)?;
            Object::NativeCall { vm, ordinal, op }
        }
        12 => {
            let rec = decode_fault(cur, limits)?;
            Object::NativeFault {
                code: rec.code,
                message: rec.message,
                op: rec.op,
            }
        }
        13 => Object::NativeDigest(cur.hash()?),
        14 => {
            let proc = machine_ref(cur, ctx)?;
            let generation = cur.u32()?;
            Object::NativeHandle { proc, generation }
        }
        15 => {
            // A nested image is opaque bytes here. It arrived inside
            // an untrusted container, so it is not verified. The
            // restore path runs the loader over it before it becomes a
            // world, and the restored object records that.
            let count = cur.count(limits.max_bytes as u64, "nested image byte")?;
            let source = cur.take(count)?;
            Object::NativeSnapshot(std::sync::Arc::new(
                cur.copy_bytes(source, "nested image bytes")?,
            ))
        }
        16 => {
            let count = cur.count(limits.max_string_bytes as u64, "byte")?;
            let source = cur.take(count)?;
            Object::Bytes(cur.copy_bytes(source, "bytes")?.into())
        }
        17 => Object::NativeFileHandle {
            resource: cur.u64()?,
        },
        18 => Object::NativeResourceHandle {
            surface: machine_ref(cur, ctx)?,
            resource: cur.u64()?,
        },
        19 => Object::NativeWait {
            owner: machine_ref(cur, ctx)?,
            token: cur.u64()?,
        },
        20 => Object::Substring(cur.str(limits.max_string_bytes)?.into()),
        21 => Object::NativeTcpStream {
            resource: cur.u64()?,
        },
        22 => Object::NativeTcpListener {
            resource: cur.u64()?,
        },
        23 => Object::NativeTlsStream {
            resource: cur.u64()?,
        },
        other => {
            return err(
                ImageReason::Layout,
                format!("the object tag {other} is not a native shape"),
            )
        }
    })
}

/// One type environment ordinal of this image, as a witness.
fn env_ref(cur: &mut Cursor<'_, '_>, ctx: &Ctx) -> Read<Witness> {
    Ok(Witness(TypeEnvId(env_ordinal(cur, ctx)?)))
}

/// One type environment ordinal of this image.
fn env_ordinal(cur: &mut Cursor<'_, '_>, ctx: &Ctx) -> Read<u32> {
    let env = cur.leb()?;
    if env >= ctx.env_count as u64 {
        return err(
            ImageReason::Reference,
            format!(
                "a type environment ordinal names entry {env} of {}",
                ctx.env_count
            ),
        );
    }
    Ok(env as u32)
}

fn machine_ref(cur: &mut Cursor<'_, '_>, ctx: &Ctx) -> Read<u32> {
    let vm = cur.leb()?;
    if vm >= ctx.machine_count as u64 {
        return err(
            ImageReason::Reference,
            format!(
                "a machine reference names ordinal {vm} of {}",
                ctx.machine_count
            ),
        );
    }
    Ok(vm as u32)
}

#[allow(clippy::too_many_lines)]
fn decode_machine(
    cur: &mut Cursor<'_, '_>,
    ctx: &Ctx,
    objects: Vec<ImageObject>,
) -> Read<ImageMachine> {
    let count = objects.len() as u32;
    let limits = &ctx.limits;
    let parent = cur.opt(ctx.machine_count as u64, "parent machine")?;
    let state = ImageState::from_tag(cur.u8()?)
        .ok_or_else(|| ImageError::new(ImageReason::State, "a machine state tag is not legal"))?;
    let flags = cur.u8()?;
    if flags & !0b111 != 0 {
        return err(ImageReason::State, "a machine flag byte holds a spare bit");
    }
    let scheduler_owned = flags & 1 != 0;
    let paused = flags & 2 != 0;
    let is_proc = flags & 4 != 0;
    let body_func = match cur.leb()? {
        0 => None,
        raw => Some(u32::try_from(raw - 1).map_err(|_| {
            ImageError::new(ImageReason::Code, "a body function slot is too large")
        })?),
    };
    let witness = env_ordinal(cur, ctx)?;
    let generation = cur.u32()?;
    let fuel = cur.u64()?;
    let next_ordinal = cur.u64()?;
    let next_wait = cur.u64()?;
    let wait_count = cur.count(crate::machine::MAX_LIVE_WAITS as u64, "wait")?;
    let mut waits = cur.vector(wait_count, "wait table")?;
    for _ in 0..wait_count {
        let token = cur.u64()?;
        let linked = cur.flag()?;
        let source = match cur.u8()? {
            0 => ImageWaitSource::Receive,
            1 => ImageWaitSource::Drive {
                target: machine_ref(cur, ctx)?,
            },
            2 => ImageWaitSource::Choice {
                first: cur.u64()?,
                second: cur.u64()?,
            },
            other => {
                return err(
                    ImageReason::State,
                    format!("the wait source tag {other} is not a source kind"),
                )
            }
        };
        waits.push(ImageWaitEntry {
            token,
            source,
            linked,
        });
    }
    let children = cur.u32()?;
    let machine_limits = decode_limits(cur)?;
    let callback_count = cur.count(limits.max_stack_values as u64, "callback")?;
    let mut callbacks: Vec<ImageCallback> = cur.vector(callback_count, "callback table")?;
    for _ in 0..callback_count {
        let func = u32::try_from(cur.leb()?)
            .map_err(|_| ImageError::new(ImageReason::Code, "a callback function is too large"))?;
        let captures = decode_values(
            cur,
            count,
            callback_count as u32,
            limits.max_stack_values as u64,
            "callback capture",
        )?;
        let env = env_ordinal(cur, ctx)?;
        let owner_depth = u32::try_from(cur.leb()?).map_err(|_| {
            ImageError::new(ImageReason::Layout, "a callback owner depth is too large")
        })?;
        callbacks.push(ImageCallback {
            func,
            captures,
            env,
            owner_depth,
        });
    }
    let frame_count = cur.count(limits.max_frames as u64, "frame")?;
    let mut frames: Vec<ImageFrame> = cur.vector(frame_count, "frame table")?;
    for _ in 0..frame_count {
        let func = cur.leb()?;
        let func = u32::try_from(func).map_err(|_| {
            ImageError::new(ImageReason::Code, "a frame function slot is too large")
        })?;
        // The decoder reads no program, so the block and the program
        // counter are data here. Admission proves that the pair names
        // a reachable instruction boundary.
        let block = u32::try_from(cur.leb()?)
            .map_err(|_| ImageError::new(ImageReason::Layout, "a frame block is too large"))?;
        let ip = u32::try_from(cur.leb()?).map_err(|_| {
            ImageError::new(ImageReason::Layout, "a frame program counter is too large")
        })?;
        let base_local = cur.leb()?;
        let base_operand = cur.leb()?;
        let closure = match cur.u8()? {
            0 => None,
            1 => Some(decode_value(cur, count, callback_count as u32)?),
            other => {
                return err(
                    ImageReason::State,
                    format!("the frame closure tag {other} is not 0 or 1"),
                )
            }
        };
        let env = env_ordinal(cur, ctx)?;
        frames.push(ImageFrame {
            func,
            block,
            ip,
            base_local: u32::try_from(base_local)
                .map_err(|_| ImageError::new(ImageReason::Layout, "a local base is too large"))?,
            base_operand: u32::try_from(base_operand).map_err(|_| {
                ImageError::new(ImageReason::Layout, "an operand base is too large")
            })?,
            closure,
            env,
        });
    }
    let locals = decode_values(
        cur,
        count,
        callback_count as u32,
        limits.max_stack_values as u64,
        "local",
    )?;
    let operands = decode_values(
        cur,
        count,
        callback_count as u32,
        limits.max_stack_values as u64,
        "operand",
    )?;
    let literal_count = cur.count(limits.max_code_slots as u64, "literal")?;
    let mut literals: Vec<Option<u32>> = cur.vector(literal_count, "literal table")?;
    for _ in 0..literal_count {
        literals.push(cur.opt(count as u64, "literal")?);
    }
    let start_body = cur.opt(count as u64, "proc body")?;
    let pending = match cur.u8()? {
        0 => None,
        1 => {
            let op = decode_op(cur)?;
            let args = decode_values(
                cur,
                count,
                callback_count as u32,
                limits.max_stack_values as u64,
                "argument",
            )?;
            let ordinal = cur.u64()?;
            Some(ImagePending { op, args, ordinal })
        }
        other => {
            return err(
                ImageReason::State,
                format!("the pending tag {other} is not 0 or 1"),
            )
        }
    };
    let nested = cur.opt(ctx.machine_count as u64, "nested machine")?;
    let routed = match cur.u8()? {
        0 => None,
        1 => {
            let target = machine_ref(cur, ctx)?;
            let cursor = match cur.u8()? {
                0 => ImagePolicyCursor::Table(machine_ref(cur, ctx)?),
                1 => ImagePolicyCursor::Binding,
                2 => ImagePolicyCursor::Root,
                other => {
                    return err(
                        ImageReason::State,
                        format!("the policy cursor tag {other} is not a cursor kind"),
                    )
                }
            };
            Some(ImageRoutedRequest { target, cursor })
        }
        other => {
            return err(
                ImageReason::State,
                format!("the routed request tag {other} is not 0 or 1"),
            )
        }
    };
    let terminal = match cur.u8()? {
        0 => None,
        1 => Some(ImageTerminal::Done(decode_value(
            cur,
            count,
            callback_count as u32,
        )?)),
        2 => Some(ImageTerminal::Fault(decode_fault(cur, limits)?)),
        other => {
            return err(
                ImageReason::State,
                format!("the terminal tag {other} is not 0, 1, or 2"),
            )
        }
    };
    let mailbox_limit = cur.u32()?;
    if mailbox_limit > limits.max_mailbox {
        return err(
            ImageReason::Mailbox,
            format!("the mailbox limit {mailbox_limit} passes the load limit"),
        );
    }
    let closed = cur.flag()?;
    let accepted = cur.u64()?;
    let delivered = cur.u64()?;
    let queue = decode_values(
        cur,
        count,
        callback_count as u32,
        mailbox_limit as u64,
        "mailbox message",
    )?;
    let block = match cur.u8()? {
        0 => None,
        1 => Some(ImageBlock::Receive),
        2 => Some(ImageBlock::Send {
            target: machine_ref(cur, ctx)?,
        }),
        3 => Some(ImageBlock::Done {
            target: machine_ref(cur, ctx)?,
        }),
        4 => Some(ImageBlock::Wait { token: cur.u64()? }),
        5 => Some(ImageBlock::Snapshot {
            target: machine_ref(cur, ctx)?,
            remaining: cur.u64()?,
            retry: cur.flag()?,
        }),
        other => {
            return err(
                ImageReason::State,
                format!("the block tag {other} is not a block kind"),
            )
        }
    };
    Ok(ImageMachine {
        parent,
        state,
        scheduler_owned,
        paused,
        is_proc,
        body_func,
        witness,
        generation,
        fuel,
        next_ordinal,
        next_wait,
        waits,
        children,
        limits: machine_limits,
        objects,
        callbacks,
        frames,
        locals,
        operands,
        literals,
        start_body,
        pending,
        nested,
        routed,
        terminal,
        mailbox: ImageMailbox {
            limit: mailbox_limit,
            queue,
            closed,
            accepted,
            delivered,
        },
        block,
    })
}

fn decode_limits(cur: &mut Cursor<'_, '_>) -> Read<ImageLimits> {
    Ok(ImageLimits {
        fuel: cur.u64()?,
        max_frames: cur.u32()?,
        max_stack_values: cur.u32()?,
        heap_bytes: cur.u64()?,
        max_objects: cur.u64()?,
        max_edges: cur.u64()?,
        max_graph_bytes: cur.u64()?,
        max_work: cur.u64()?,
        max_children: cur.u32()?,
        max_resources: cur.u32()?,
        mailbox_limit: cur.u32()?,
    })
}
