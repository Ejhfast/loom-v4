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
//! The decoder is the whole load checklist of specification 17.8. It
//! runs once, on untrusted bytes.
//!
//! **The decoder rule.** No count in the container ever sizes an
//! allocation before the reader checks it against the load limits and
//! against the bytes that remain. Every element of every counted list
//! costs at least one byte, so `count > remaining` rejects at once.

use super::{
    Image, ImageBlock, ImageError, ImageFrame, ImageLimits, ImageMachine, ImageMailbox,
    ImageObject, ImagePending, ImageReason, ImageState, ImageTerminal, LoadLimits, SnapshotFail,
    SnapshotImage, FORMAT_VERSION, MAGIC, SECTION_CODE, SECTION_HEADER, SECTION_HEAPS,
    SECTION_MACHINES,
};
use crate::LoadedModule;
use lm_abi::FaultCode;
use lm_bytecode::identity::COMPILER_ABI_VERSION;
use lm_heap::{MapIndex, Object};
use lm_value::{ObjRef, Value};

/// The domain separator of the container hash.
const HASH_DOMAIN: &[u8] = b"lm-snapshot-container-v1\0";

/// The value tags of the canonical encoding.
const V_UNIT: u8 = 0;
const V_BOOL: u8 = 1;
const V_INT: u8 = 2;
const V_OP: u8 = 3;
const V_OBJ: u8 = 4;
const V_UNINIT: u8 = 5;

/// The container hash of one byte prefix.
pub fn container_hash(prefix: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(HASH_DOMAIN.len() + prefix.len());
    input.extend_from_slice(HASH_DOMAIN);
    input.extend_from_slice(prefix);
    lm_graph::digest::hash(&input)
}

// ---------------------------------------------------------------
// The writer.
// ---------------------------------------------------------------

struct Out {
    bytes: Vec<u8>,
    limit: usize,
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
            Value::Op(op) => {
                self.u8(V_OP);
                let id = lm_abi::op_identity(op);
                self.hash(&id);
            }
            Value::Obj(r) => {
                self.u8(V_OBJ);
                self.leb(r.slot as u64);
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
    let heaps = section_heaps(image, limit)?;
    let machines = section_machines(image, limit)?;
    let payloads = [
        (SECTION_HEADER, header),
        (SECTION_CODE, code),
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
    Ok(out.bytes)
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

fn section_heaps(image: &Image, limit: usize) -> Result<Vec<u8>, SnapshotFail> {
    let mut out = Out {
        bytes: Vec::new(),
        limit,
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
    Ok(out.bytes)
}

fn encode_object(out: &mut Out, object: &Object) {
    out.u8(object.tag());
    match object {
        Object::Str(text) => out.str(text),
        Object::Instance { class, fields } => {
            out.leb(*class as u64);
            out.values(fields);
        }
        Object::List { items } | Object::Tuple { items } => out.values(items),
        Object::Map { entries, .. } => {
            out.leb(entries.len() as u64);
            for (key, value) in entries {
                out.value(*key);
                out.value(*value);
            }
        }
        Object::Closure { func, captures } => {
            out.leb(*func as u64);
            out.values(captures);
        }
        Object::StrBuilder(text) => out.str(text),
        Object::ByteBuf(bytes) => {
            out.leb(bytes.len() as u64);
            out.bytes.extend_from_slice(bytes);
        }
        Object::NativeVm { vm } | Object::NativeTable { vm } => out.leb(*vm as u64),
        Object::NativeRequest { vm, ordinal } => {
            out.leb(*vm as u64);
            out.u64(*ordinal);
        }
        Object::NativeCall { vm, ordinal, op } => {
            out.leb(*vm as u64);
            out.u64(*ordinal);
            let id = lm_abi::op_identity(*op);
            out.hash(&id);
        }
        Object::NativeFault { code, message, op } => {
            out.str(&code.to_string());
            out.str(message);
            match op {
                None => out.u8(0),
                Some(slot) => {
                    out.u8(1);
                    let id = lm_abi::op_identity(*slot);
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
    }
}

fn section_machines(image: &Image, limit: usize) -> Result<Vec<u8>, SnapshotFail> {
    let mut out = Out {
        bytes: Vec::new(),
        limit,
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
        out.u8(flags);
        out.u32(machine.generation);
        out.u64(machine.fuel);
        out.u64(machine.next_ordinal);
        out.u32(machine.children);
        encode_limits(&mut out, &machine.limits);
        out.leb(machine.frames.len() as u64);
        for frame in &machine.frames {
            out.leb(frame.func as u64);
            out.leb(frame.block as u64);
            out.leb(frame.ip as u64);
            out.leb(frame.base_local as u64);
            out.leb(frame.base_operand as u64);
            out.opt(frame.closure);
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
                let id = lm_abi::op_identity(pending.op);
                out.hash(&id);
                out.values(&pending.args);
                out.u64(pending.ordinal);
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
                        let id = lm_abi::op_identity(slot);
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
        }
        if out.over_limit() {
            return Err(SnapshotFail::LimitExceeded);
        }
    }
    Ok(out.bytes)
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

struct Cursor<'b> {
    bytes: &'b [u8],
    at: usize,
    /// The end of the section the reader is inside.
    end: usize,
}

type Read<T> = Result<T, ImageError>;

fn err<T>(reason: ImageReason, detail: impl Into<String>) -> Read<T> {
    Err(ImageError::new(reason, detail))
}

impl<'b> Cursor<'b> {
    fn new(bytes: &'b [u8]) -> Cursor<'b> {
        Cursor {
            bytes,
            at: 0,
            end: bytes.len(),
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
        String::from_utf8(bytes.to_vec())
            .map_err(|_| ImageError::new(ImageReason::Layout, "a string is not valid UTF-8"))
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

/// The context every reference check needs.
struct Ctx<'m> {
    module: &'m lm_bytecode::Module,
    limits: LoadLimits,
    funcs: Vec<u32>,
    classes: Vec<u32>,
    machine_count: u32,
}

impl Ctx<'_> {
    fn func_ok(&self, slot: u32) -> bool {
        self.funcs.binary_search(&slot).is_ok()
    }

    fn class_ok(&self, slot: u32) -> bool {
        self.classes.binary_search(&slot).is_ok()
    }
}

/// Load one external snapshot container.
///
/// This is the one place that runs the whole checklist of
/// specification 17.8. It runs once per byte string; a later restore
/// reads the checked image and repeats nothing.
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
    let image = decode(bytes, loaded, limits)?;
    let hash = container_hash(&bytes[..bytes.len() - 32]);
    Ok(SnapshotImage {
        bytes: std::sync::Arc::new(bytes.to_vec()),
        world: std::sync::Arc::new(image),
        hash,
        externally_checked: true,
    })
}

/// Seal one image the writer produced.
///
/// The writer builds the image from trusted verified state, so this
/// path runs no structural check. It encodes the canonical bytes and
/// keeps the image beside them.
pub fn seal(image: Image, limit: usize) -> Result<SnapshotImage, SnapshotFail> {
    let bytes = encode(&image, limit)?;
    let hash = container_hash(&bytes[..bytes.len() - 32]);
    Ok(SnapshotImage {
        bytes: std::sync::Arc::new(bytes),
        world: std::sync::Arc::new(image),
        hash,
        externally_checked: false,
    })
}

/// Decode and check one container.
pub fn decode(
    bytes: &[u8],
    loaded: &LoadedModule,
    limits: LoadLimits,
) -> Result<Image, ImageError> {
    let identity = loaded
        .identity()
        .map_err(|_| ImageError::new(ImageReason::Code, "the program has no verified identity"))?;
    let module = loaded.module();
    if bytes.len() < prefix_len() + 32 {
        return err(
            ImageReason::Truncated,
            "the container is shorter than its frame",
        );
    }
    let mut cur = Cursor::new(bytes);
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
    let mut table: Vec<(u32, u64, u64)> = Vec::with_capacity(count);
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
    let section = |idx: usize| -> Cursor<'_> {
        let (_, offset, length) = table[idx];
        Cursor {
            bytes,
            at: offset as usize,
            end: (offset + length) as usize,
        }
    };
    // Section 1: the header. The machine count is checked against the
    // load limit before any machine vector exists.
    let mut header = section(0);
    let machine_count = header.count(limits.max_machines as u64, "machine")?;
    if machine_count == 0 {
        return err(ImageReason::State, "a snapshot world holds no machine");
    }
    let root = header.leb()?;
    if root != 0 {
        return err(
            ImageReason::State,
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
    if module_semantic != identity.semantic_hash {
        return err(
            ImageReason::Code,
            "the image names another program than the loaded one",
        );
    }
    // Section 2: the code manifest. Every referenced slot exists in
    // the loaded verified program and carries its definition hash.
    let mut code = section(1);
    let func_count = code.count(module.funcs.len() as u64, "function")?;
    let mut funcs: Vec<(u32, [u8; 32])> = Vec::new();
    let mut last: Option<u32> = None;
    for _ in 0..func_count {
        let slot = code.leb()?;
        let hash = code.hash()?;
        let slot = u32::try_from(slot)
            .map_err(|_| ImageError::new(ImageReason::Code, "a function slot is too large"))?;
        if slot as usize >= module.funcs.len() {
            return err(
                ImageReason::Code,
                format!("the image names function slot {slot}, which the program has not"),
            );
        }
        if last.is_some_and(|l| slot <= l) {
            return err(ImageReason::Code, "the function manifest is not ascending");
        }
        last = Some(slot);
        if identity.func_hashes[slot as usize] != hash {
            return err(
                ImageReason::Code,
                format!("function slot {slot} carries another definition hash"),
            );
        }
        funcs.push((slot, hash));
    }
    let class_count = code.count(module.classes.len() as u64, "class")?;
    let mut classes: Vec<(u32, [u8; 32])> = Vec::new();
    let mut last: Option<u32> = None;
    for _ in 0..class_count {
        let slot = code.leb()?;
        let hash = code.hash()?;
        let slot = u32::try_from(slot)
            .map_err(|_| ImageError::new(ImageReason::Code, "a class slot is too large"))?;
        if slot as usize >= module.classes.len() {
            return err(
                ImageReason::Code,
                format!("the image names class slot {slot}, which the program has not"),
            );
        }
        if last.is_some_and(|l| slot <= l) {
            return err(ImageReason::Code, "the class manifest is not ascending");
        }
        last = Some(slot);
        if identity.class_hashes[slot as usize] != hash {
            return err(
                ImageReason::Code,
                format!("class slot {slot} carries another definition hash"),
            );
        }
        classes.push((slot, hash));
    }
    if code.remaining() != 0 {
        return err(ImageReason::Trailing, "the code section holds extra bytes");
    }
    let ctx = Ctx {
        module,
        limits,
        funcs: funcs.iter().map(|(s, _)| *s).collect(),
        classes: classes.iter().map(|(s, _)| *s).collect(),
        machine_count: machine_count as u32,
    };
    // Section 3: the heaps, one per machine, in ordinal order.
    let mut heaps = section(2);
    let mut all_objects: Vec<Vec<ImageObject>> = Vec::new();
    for vm in 0..machine_count {
        let count = heaps.count(ctx.limits.max_objects as u64, "object")?;
        let mut objects: Vec<ImageObject> = Vec::new();
        for ordinal in 0..count {
            let frozen = heaps.flag()?;
            let object = decode_object(&mut heaps, &ctx, count as u32)?;
            if object.shape().born_frozen && !frozen {
                return err(
                    ImageReason::State,
                    format!(
                        "machine {vm} object {ordinal} is a {} without the frozen bit",
                        object.shape().name
                    ),
                );
            }
            objects.push(ImageObject { frozen, object });
        }
        all_objects.push(objects);
    }
    if heaps.remaining() != 0 {
        return err(ImageReason::Trailing, "the heap section holds extra bytes");
    }
    // Section 4: the machine records.
    let mut records = section(3);
    let mut machines: Vec<ImageMachine> = Vec::new();
    for (vm, objects) in all_objects.into_iter().enumerate() {
        let machine = decode_machine(&mut records, &ctx, vm as u32, objects)?;
        machines.push(machine);
    }
    if records.remaining() != 0 {
        return err(
            ImageReason::Trailing,
            "the machine section holds extra bytes",
        );
    }
    let image = Image {
        format,
        abi_version,
        compiler_abi,
        verifier_version,
        module_semantic,
        result_type,
        funcs,
        classes,
        machines,
    };
    check_world(&image)?;
    Ok(image)
}

fn decode_value(cur: &mut Cursor<'_>, objects: u32) -> Read<Value> {
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
        V_UNINIT => Value::Uninit,
        other => {
            return err(
                ImageReason::Layout,
                format!("the value tag {other} is not a canonical tag"),
            )
        }
    })
}

fn decode_values(cur: &mut Cursor<'_>, objects: u32, cap: u64, what: &str) -> Read<Vec<Value>> {
    let count = cur.count(cap, what)?;
    let mut out: Vec<Value> = Vec::new();
    for _ in 0..count {
        out.push(decode_value(cur, objects)?);
    }
    Ok(out)
}

fn decode_op(cur: &mut Cursor<'_>) -> Read<u32> {
    let id = cur.hash()?;
    match (0..lm_abi::OP_COUNT).find(|slot| lm_abi::op_identity(*slot) == id) {
        Some(slot) => Ok(slot),
        None => err(
            ImageReason::Code,
            "an operation identity names no manifest operation",
        ),
    }
}

fn decode_fault(cur: &mut Cursor<'_>, limits: &LoadLimits) -> Read<crate::FaultRec> {
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

fn decode_object(cur: &mut Cursor<'_>, ctx: &Ctx<'_>, objects: u32) -> Read<Object> {
    let tag = cur.u8()?;
    let limits = &ctx.limits;
    Ok(match tag {
        0 => Object::Str(cur.str(limits.max_string_bytes)?),
        1 => {
            let class = cur.leb()?;
            let class = u32::try_from(class)
                .map_err(|_| ImageError::new(ImageReason::Code, "a class slot is too large"))?;
            if !ctx.class_ok(class) {
                return err(
                    ImageReason::Code,
                    format!("an instance names class slot {class}, which the manifest omits"),
                );
            }
            let fields = decode_values(cur, objects, limits.max_stack_values as u64, "field")?;
            let want = ctx.module.classes[class as usize].fields.len();
            if fields.len() != want {
                return err(
                    ImageReason::Layout,
                    format!(
                        "an instance of class {class} holds {} fields and the layout has {want}",
                        fields.len()
                    ),
                );
            }
            Object::Instance { class, fields }
        }
        2 => Object::List {
            items: decode_values(cur, objects, limits.max_stack_values as u64, "list item")?,
        },
        3 => {
            let count = cur.count(limits.max_stack_values as u64, "map entry")?;
            let mut entries: Vec<(Value, Value)> = Vec::new();
            for _ in 0..count {
                let key = decode_value(cur, objects)?;
                let value = decode_value(cur, objects)?;
                entries.push((key, value));
            }
            Object::Map {
                entries,
                index: MapIndex::default(),
            }
        }
        4 => Object::Tuple {
            items: decode_values(cur, objects, limits.max_stack_values as u64, "tuple item")?,
        },
        5 => {
            let func = cur.leb()?;
            let func = u32::try_from(func)
                .map_err(|_| ImageError::new(ImageReason::Code, "a function slot is too large"))?;
            if !ctx.func_ok(func) {
                return err(
                    ImageReason::Code,
                    format!("a closure names function slot {func}, which the manifest omits"),
                );
            }
            let captures = decode_values(cur, objects, limits.max_stack_values as u64, "capture")?;
            let want = ctx.module.funcs[func as usize].captures.len();
            if captures.len() != want {
                return err(
                    ImageReason::Layout,
                    format!(
                        "a closure of function {func} holds {} captures and the code declares {want}",
                        captures.len()
                    ),
                );
            }
            Object::Closure { func, captures }
        }
        6 => Object::StrBuilder(cur.str(limits.max_string_bytes)?),
        7 => {
            let count = cur.count(limits.max_string_bytes as u64, "buffer byte")?;
            Object::ByteBuf(cur.take(count)?.to_vec())
        }
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
            Object::NativeSnapshot(std::sync::Arc::new(cur.take(count)?.to_vec()))
        }
        other => {
            return err(
                ImageReason::Layout,
                format!("the object tag {other} is not a native shape"),
            )
        }
    })
}

fn machine_ref(cur: &mut Cursor<'_>, ctx: &Ctx<'_>) -> Read<u32> {
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
    cur: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    vm: u32,
    objects: Vec<ImageObject>,
) -> Read<ImageMachine> {
    let count = objects.len() as u32;
    let limits = &ctx.limits;
    let parent = cur.opt(ctx.machine_count as u64, "parent machine")?;
    let state = ImageState::from_tag(cur.u8()?)
        .ok_or_else(|| ImageError::new(ImageReason::State, "a machine state tag is not legal"))?;
    let flags = cur.u8()?;
    if flags & !0b11 != 0 {
        return err(ImageReason::State, "a machine flag byte holds a spare bit");
    }
    let scheduler_owned = flags & 1 != 0;
    let paused = flags & 2 != 0;
    let generation = cur.u32()?;
    let fuel = cur.u64()?;
    let next_ordinal = cur.u64()?;
    let children = cur.u32()?;
    let machine_limits = decode_limits(cur)?;
    let frame_count = cur.count(limits.max_frames as u64, "frame")?;
    let mut frames: Vec<ImageFrame> = Vec::new();
    for _ in 0..frame_count {
        let func = cur.leb()?;
        let func = u32::try_from(func).map_err(|_| {
            ImageError::new(ImageReason::Code, "a frame function slot is too large")
        })?;
        if !ctx.func_ok(func) {
            return err(
                ImageReason::Code,
                format!("a frame names function slot {func}, which the manifest omits"),
            );
        }
        let code = &ctx.module.funcs[func as usize];
        let block = cur.leb()?;
        let ip = cur.leb()?;
        if block >= code.blocks.len() as u64 {
            return err(
                ImageReason::Layout,
                format!("a frame of function {func} names block {block}, which does not exist"),
            );
        }
        // A machine stops between instructions, so the program counter
        // names the next instruction of the block. Every block ends
        // with a terminator, so the counter never reaches the end.
        if ip >= code.blocks[block as usize].len() as u64 {
            return err(
                ImageReason::Layout,
                format!("a frame of function {func} holds a program counter past its block"),
            );
        }
        let base_local = cur.leb()?;
        let base_operand = cur.leb()?;
        let closure = cur.opt(count as u64, "frame closure")?;
        frames.push(ImageFrame {
            func,
            block: block as u32,
            ip: ip as u32,
            base_local: u32::try_from(base_local)
                .map_err(|_| ImageError::new(ImageReason::Layout, "a local base is too large"))?,
            base_operand: u32::try_from(base_operand).map_err(|_| {
                ImageError::new(ImageReason::Layout, "an operand base is too large")
            })?,
            closure,
        });
    }
    let locals = decode_values(cur, count, limits.max_stack_values as u64, "local")?;
    let operands = decode_values(cur, count, limits.max_stack_values as u64, "operand")?;
    let literal_count = cur.count(ctx.module.strings.len() as u64, "literal")?;
    let mut literals: Vec<Option<u32>> = Vec::new();
    for _ in 0..literal_count {
        literals.push(cur.opt(count as u64, "literal")?);
    }
    let start_body = cur.opt(count as u64, "proc body")?;
    let pending = match cur.u8()? {
        0 => None,
        1 => {
            let op = decode_op(cur)?;
            let args = decode_values(cur, count, limits.max_stack_values as u64, "argument")?;
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
    let terminal = match cur.u8()? {
        0 => None,
        1 => Some(ImageTerminal::Done(decode_value(cur, count)?)),
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
    let queue = decode_values(cur, count, mailbox_limit as u64, "mailbox message")?;
    let block = match cur.u8()? {
        0 => None,
        1 => Some(ImageBlock::Receive),
        2 => Some(ImageBlock::Send {
            target: machine_ref(cur, ctx)?,
        }),
        3 => Some(ImageBlock::Done {
            target: machine_ref(cur, ctx)?,
        }),
        other => {
            return err(
                ImageReason::State,
                format!("the block tag {other} is not a block kind"),
            )
        }
    };
    let machine = ImageMachine {
        parent,
        state,
        scheduler_owned,
        paused,
        generation,
        fuel,
        next_ordinal,
        children,
        limits: machine_limits,
        objects,
        frames,
        locals,
        operands,
        literals,
        start_body,
        pending,
        terminal,
        mailbox: ImageMailbox {
            limit: mailbox_limit,
            queue,
            closed,
            accepted,
            delivered,
        },
        block,
    };
    check_machine(&machine, ctx, vm)?;
    Ok(machine)
}

fn decode_limits(cur: &mut Cursor<'_>) -> Read<ImageLimits> {
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

/// Prove the state rules of one decoded machine.
fn check_machine(m: &ImageMachine, ctx: &Ctx<'_>, vm: u32) -> Read<()> {
    let at = |what: &str| format!("machine {vm}: {what}");
    // The frame chain. Local bases follow the declared local counts
    // exactly, and the arenas end where the last frame ends.
    let mut want_local = 0u64;
    let mut last_operand = 0u64;
    for (idx, frame) in m.frames.iter().enumerate() {
        if frame.base_local as u64 != want_local {
            return err(
                ImageReason::Layout,
                at(&format!("frame {idx} does not start at its local base")),
            );
        }
        if (frame.base_operand as u64) < last_operand {
            return err(
                ImageReason::Layout,
                at(&format!("frame {idx} lowers the operand base")),
            );
        }
        last_operand = frame.base_operand as u64;
        want_local += ctx.module.funcs[frame.func as usize].local_count() as u64;
    }
    if m.locals.len() as u64 != want_local {
        return err(
            ImageReason::Layout,
            at("the local arena does not match the frame chain"),
        );
    }
    if (m.operands.len() as u64) < last_operand {
        return err(
            ImageReason::Layout,
            at("the operand arena ends below the last frame base"),
        );
    }
    if m.locals.len() + m.operands.len() > m.limits.max_stack_values as usize {
        return err(
            ImageReason::Layout,
            at("the arenas together pass the declared stack limit"),
        );
    }
    if m.frames.len() > m.limits.max_frames as usize {
        return err(ImageReason::Layout, at("the frame count passes its limit"));
    }
    // The literal table names the module string pool, and a stored
    // literal holds exactly the pooled text.
    for (idx, literal) in m.literals.iter().enumerate() {
        let Some(ordinal) = literal else { continue };
        match &m.objects[*ordinal as usize].object {
            Object::Str(text) if *text == ctx.module.strings[idx] => {}
            _ => {
                return err(
                    ImageReason::Reference,
                    at(&format!("literal {idx} does not hold its pooled string")),
                )
            }
        }
    }
    if let Some(body) = m.start_body {
        if !matches!(m.objects[body as usize].object, Object::Closure { .. }) {
            return err(ImageReason::Reference, at("the proc body is not a closure"));
        }
    }
    for (idx, frame) in m.frames.iter().enumerate() {
        if let Some(closure) = frame.closure {
            if !matches!(m.objects[closure as usize].object, Object::Closure { .. }) {
                return err(
                    ImageReason::Reference,
                    at(&format!(
                        "frame {idx} names a capture context that is not a closure"
                    )),
                );
            }
        }
    }
    // The state rules of specification 14.3 and 17.6.
    match m.state {
        ImageState::Empty => {
            if !m.frames.is_empty() || m.pending.is_some() || m.terminal.is_some() {
                return err(
                    ImageReason::State,
                    at("an empty machine holds execution state"),
                );
            }
        }
        ImageState::Ready => {
            if m.frames.is_empty() {
                return err(ImageReason::State, at("a ready machine holds no frame"));
            }
            if m.pending.is_some() {
                return err(
                    ImageReason::State,
                    at("a ready machine holds a pending request"),
                );
            }
            if m.terminal.is_some() {
                return err(
                    ImageReason::State,
                    at("a ready machine holds a terminal result"),
                );
            }
        }
        ImageState::Asked | ImageState::Blocked => {
            if m.frames.is_empty() {
                return err(ImageReason::State, at("a stopped machine holds no frame"));
            }
            if m.pending.is_none() {
                return err(
                    ImageReason::State,
                    at("an asked or blocked machine holds no pending request"),
                );
            }
            if m.terminal.is_some() {
                return err(
                    ImageReason::State,
                    at("an asked or blocked machine holds a terminal result"),
                );
            }
        }
        ImageState::Done | ImageState::Faulted => {
            if m.pending.is_some() {
                return err(
                    ImageReason::State,
                    at("a terminal machine holds a pending request"),
                );
            }
        }
    }
    match (&m.state, &m.terminal) {
        (ImageState::Done, Some(ImageTerminal::Done(_))) => {}
        (ImageState::Faulted, Some(ImageTerminal::Fault(_))) => {}
        (ImageState::Done | ImageState::Faulted, _) => {
            return err(
                ImageReason::State,
                at("a terminal machine does not store its result"),
            )
        }
        (_, Some(_)) => {
            return err(
                ImageReason::State,
                at("a live machine stores a terminal result"),
            )
        }
        _ => {}
    }
    // A block record exists exactly when the machine is blocked, and
    // its kind matches the pending proc operation.
    match (m.state, m.block) {
        (ImageState::Blocked, Some(block)) => {
            let op = m
                .pending
                .as_ref()
                .expect("a blocked machine has a request")
                .op;
            let ok = match block {
                ImageBlock::Receive => op == lm_abi::OP_PROC_RECV,
                ImageBlock::Send { .. } => op == lm_abi::OP_PROC_SEND,
                ImageBlock::Done { .. } => op == lm_abi::OP_PROC_DONE,
            };
            if !ok {
                return err(
                    ImageReason::State,
                    at("the block record does not match the pending operation"),
                );
            }
        }
        (ImageState::Blocked, None) => {
            return err(
                ImageReason::State,
                at("a blocked machine holds no block record"),
            )
        }
        (_, Some(_)) => {
            return err(
                ImageReason::State,
                at("a machine that is not blocked holds a block record"),
            )
        }
        _ => {}
    }
    // The pending request names a legal operation for this state.
    if let Some(pending) = &m.pending {
        if lm_abi::op(pending.op).suspends() {
            return err(
                ImageReason::State,
                at("a pending host attachment has no bytes to copy"),
            );
        }
        if pending.ordinal >= m.next_ordinal {
            return err(
                ImageReason::State,
                at("the pending request ordinal is not below the next ordinal"),
            );
        }
    }
    // The mailbox rules of specification 18.5.
    if m.mailbox.queue.len() > m.mailbox.limit as usize {
        return err(
            ImageReason::Mailbox,
            at("the accepted queue is longer than the mailbox limit"),
        );
    }
    if m.mailbox.delivered > m.mailbox.accepted {
        return err(
            ImageReason::Mailbox,
            at("the mailbox delivered more messages than it accepted"),
        );
    }
    // The world gate and the paused state.
    if m.paused && m.scheduler_owned {
        return err(
            ImageReason::State,
            at("a paused proc is not scheduler-owned"),
        );
    }
    if vm == 0 && (m.scheduler_owned || m.paused) {
        return err(
            ImageReason::State,
            at("the restored root is holder-controlled"),
        );
    }
    Ok(())
}

/// Prove the rules that need the whole world.
fn check_world(image: &Image) -> Read<()> {
    for (vm, machine) in image.machines.iter().enumerate() {
        // Every handle names a captured machine at its generation.
        for (ordinal, entry) in machine.objects.iter().enumerate() {
            if let Object::NativeHandle { proc, generation } = entry.object {
                let target = &image.machines[proc as usize];
                if target.generation != generation {
                    return err(
                        ImageReason::Reference,
                        format!(
                            "machine {vm} object {ordinal} names machine {proc} at generation \
                             {generation}, and that machine holds {}",
                            target.generation
                        ),
                    );
                }
            }
        }
        // A request or call token names a machine that holds exactly
        // that pending request.
        for (ordinal, entry) in machine.objects.iter().enumerate() {
            let (target, request, op) = match entry.object {
                Object::NativeRequest { vm, ordinal } => (vm, Some(ordinal), None),
                Object::NativeCall { vm, ordinal, op } => (vm, Some(ordinal), Some(op)),
                _ => continue,
            };
            let target = &image.machines[target as usize];
            let Some(request) = request else { continue };
            // A stale token is legal: the machine answered the request
            // already. The rule is that a live token agrees.
            if target.state == ImageState::Asked {
                let pending = target
                    .pending
                    .as_ref()
                    .expect("an asked machine holds its request");
                if pending.ordinal == request && op.is_some_and(|op| op != pending.op) {
                    return err(
                        ImageReason::Reference,
                        format!(
                            "machine {vm} object {ordinal} names another operation than the \
                             pending request it points at"
                        ),
                    );
                }
            }
        }
        // A block names a machine that can release it.
        if let Some(ImageBlock::Send { target } | ImageBlock::Done { target }) = machine.block {
            if target as usize >= image.machines.len() {
                return err(ImageReason::Reference, "a block names no captured machine");
            }
        }
        // The heap is exactly the canonical traversal of its roots.
        check_order(machine, vm as u32)?;
    }
    Ok(())
}

/// Prove that the stored heap is the canonical traversal of its roots.
///
/// The walk is iterative, so a deep image never grows the Rust stack.
/// The check also proves that every stored object is reachable and
/// that no object is missing.
fn check_order(machine: &ImageMachine, vm: u32) -> Read<()> {
    let count = machine.objects.len();
    let mut seen = vec![false; count];
    let mut next = 0usize;
    let roots = super::image_roots(machine);
    let mut stack: Vec<u32> = roots.iter().rev().copied().collect();
    let mut children: Vec<ObjRef> = Vec::new();
    while let Some(r) = stack.pop() {
        let idx = r as usize;
        if seen[idx] {
            continue;
        }
        if idx != next {
            return err(
                ImageReason::Order,
                format!(
                    "machine {vm}: the traversal reaches object {idx} where the canonical \
                     order needs {next}"
                ),
            );
        }
        seen[idx] = true;
        next += 1;
        children.clear();
        machine.objects[idx].object.children(&mut children);
        stack.extend(children.iter().rev().map(|c| c.slot));
    }
    if next != count {
        return err(
            ImageReason::Order,
            format!(
                "machine {vm}: {} stored objects are unreachable",
                count - next
            ),
        );
    }
    Ok(())
}
