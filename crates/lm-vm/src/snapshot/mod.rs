//! Machine-world snapshots: the image, the canonical codec, the
//! writer, admission, and restore (specification 17 and
//! `docs/specs/snapshot-image-admission.md`).
//!
//! The module holds two host states, and the type system keeps them
//! apart:
//!
//! - `Image` is editable snapshot data. It supports inspection,
//!   editing, encoding, and diagnostic tools. It promises nothing
//!   about references, machine state, or types;
//! - `SnapshotImage` is the admitted, immutable form. Only admission
//!   or trusted capture builds one, and only a `SnapshotImage`
//!   restores.
//!
//! Three paths reach an admitted image:
//!
//! - trusted capture. `World::capture_snapshot` builds one from
//!   verified live state. It never decodes bytes;
//! - external loading. `load_external` decodes untrusted bytes, admits
//!   the result against the exact verified module, and seals it;
//! - re-admission. An editor turns a `SnapshotImage` back into an
//!   `Image`, and the edited image passes admission again.
//!
//! Nothing in this module touches the filesystem, the clock, or the
//! network. The command-line tool reads and writes `.lms` files.

pub mod admit;
pub mod codec;
pub mod dump;
pub mod restore;
pub mod write;

pub use admit::{admit, AdmissionBudget};
pub use codec::load_external;
pub use write::{CutError, CutReport};

use crate::machine::VmId;
use crate::{FaultCode, FaultRec};
use lm_heap::Object;
use lm_value::Value;

/// The canonical snapshot container magic.
pub const MAGIC: [u8; 8] = *b"LMSNAP\0\x01";

/// The canonical snapshot format version.
///
/// The container is sectioned, and the section table names each
/// section by kind. A later format may add a section kind, for example
/// a delta section that names its base image by container hash,
/// without moving any existing section.
///
/// Version 2 states the initialization of a local slot. A slot that
/// holds no value spells the uninitialized marker, and version 1
/// spelled it as a unit value. Admission reads the marker as the
/// initialization fact, so it cannot admit a version-1 container.
///
/// Version 2 also carries the type environment witnesses of section
/// 5.6: one closed type table, one environment table, and one
/// environment index for each frame, closure, instance, and machine.
pub const FORMAT_VERSION: u32 = 2;

/// The section kinds, in canonical order.
///
/// The order of this list is the order of the section table and of the
/// payloads. A reader rejects any other order, so one image has one
/// byte string.
pub const SECTION_HEADER: u32 = 1;
pub const SECTION_CODE: u32 = 2;
/// The closed type table and the type environment table. Heaps and
/// machine records name an environment by its ordinal here, so the
/// section precedes both.
pub const SECTION_TYPES: u32 = 3;
pub const SECTION_HEAPS: u32 = 4;
pub const SECTION_MACHINES: u32 = 5;

/// The lifecycle state one captured machine may hold.
///
/// `Running` and `Waiting` are absent by rule. A captured machine sits
/// at an instruction boundary, so it is never running, and a machine
/// in `waiting` holds a live host attachment, which blocks the capture
/// with `ResourceActive` (specification 17.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageState {
    Empty,
    Ready,
    Asked,
    Blocked,
    Done,
    Faulted,
}

impl ImageState {
    pub fn tag(self) -> u8 {
        match self {
            ImageState::Empty => 0,
            ImageState::Ready => 1,
            ImageState::Asked => 2,
            ImageState::Blocked => 3,
            ImageState::Done => 4,
            ImageState::Faulted => 5,
        }
    }

    pub fn from_tag(tag: u8) -> Option<ImageState> {
        Some(match tag {
            0 => ImageState::Empty,
            1 => ImageState::Ready,
            2 => ImageState::Asked,
            3 => ImageState::Blocked,
            4 => ImageState::Done,
            5 => ImageState::Faulted,
            _ => return None,
        })
    }

    /// The stable name, for `lm snapshot verify` and the dump.
    pub fn name(self) -> &'static str {
        match self {
            ImageState::Empty => "empty",
            ImageState::Ready => "ready",
            ImageState::Asked => "asked",
            ImageState::Blocked => "blocked",
            ImageState::Done => "done",
            ImageState::Faulted => "faulted",
        }
    }
}

/// One captured frame. Every index names the loaded program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageFrame {
    pub func: u32,
    pub block: u32,
    pub ip: u32,
    pub base_local: u32,
    pub base_operand: u32,
    /// The capture context, as an object ordinal.
    pub closure: Option<u32>,
    /// The type environment of this activation, as an ordinal of the
    /// environment table of the image.
    ///
    /// The frame witness. Admission checks it against the call site
    /// below, and it uses it where no call site exists: the bottom
    /// frame of a machine has none.
    pub env: u32,
}

/// One captured pending request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePending {
    pub op: u32,
    pub args: Vec<Value>,
    pub ordinal: u64,
}

/// One captured terminal result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageTerminal {
    Done(Value),
    Fault(FaultRec),
}

/// One captured mailbox. The barrier freeze flag is barrier state, so
/// it never enters an image: a restored mailbox accepts at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMailbox {
    pub limit: u32,
    pub queue: Vec<Value>,
    pub closed: bool,
    pub accepted: u64,
    pub delivered: u64,
}

/// Why one captured machine is blocked on another captured machine.
///
/// The record names the target by machine ordinal. The generation
/// comes from the target machine on restore, so the two can never
/// disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageBlock {
    Receive,
    Send { target: u32 },
    Done { target: u32 },
}

/// The resource limits of one captured machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLimits {
    pub fuel: u64,
    pub max_frames: u32,
    pub max_stack_values: u32,
    pub heap_bytes: u64,
    pub max_objects: u64,
    pub max_edges: u64,
    pub max_graph_bytes: u64,
    pub max_work: u64,
    pub max_children: u32,
    pub max_resources: u32,
    pub mailbox_limit: u32,
}

/// One captured machine.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageMachine {
    /// The captured parent, as a machine ordinal. `None` means the
    /// parent is outside the world; restore re-roots such a machine
    /// on the restoring machine (specification 17.5).
    pub parent: Option<u32>,
    pub state: ImageState,
    /// True when the scheduler owns the execution of this machine.
    pub scheduler_owned: bool,
    /// True when a holder paused this proc (specification 17.6).
    pub paused: bool,
    /// The body function of this machine, as a function slot.
    ///
    /// The machine witness, with `witness` below. The declared result
    /// type of the machine is the result type of this function, and
    /// the first parameter of this function is the proc instance of a
    /// proc. A terminal machine keeps no frame and no body closure, so
    /// the record is the one lasting evidence of both types.
    pub body_func: Option<u32>,
    /// The type environment of the machine body activation, as an
    /// ordinal of the environment table of the image.
    pub witness: u32,
    /// True when `Proc.Spawn` launched this machine.
    ///
    /// The flag survives the root normalization of specification
    /// 17.5, so a restored proc world still knows which machines take
    /// the birth grant of specification 18.3. Admission proves it: the
    /// witness of a proc must name a proc class.
    pub is_proc: bool,
    pub generation: u32,
    /// The remaining instruction budget.
    pub fuel: u64,
    pub next_ordinal: u64,
    pub children: u32,
    pub limits: ImageLimits,
    /// The captured heap, in canonical traversal order. Every object
    /// reference inside holds the ordinal of the target in this
    /// vector.
    pub objects: Vec<ImageObject>,
    pub frames: Vec<ImageFrame>,
    pub locals: Vec<Value>,
    pub operands: Vec<Value>,
    /// The interned literal of each module string slot.
    pub literals: Vec<Option<u32>>,
    pub start_body: Option<u32>,
    pub pending: Option<ImagePending>,
    pub terminal: Option<ImageTerminal>,
    pub mailbox: ImageMailbox,
    pub block: Option<ImageBlock>,
}

/// One captured object: the frozen bit and the payload.
///
/// Every object reference inside `object` holds `(ordinal, 0)` instead
/// of a live heap slot, so the payload is independent of the heap that
/// produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageObject {
    pub frozen: bool,
    pub object: Object,
}

/// One decoded machine world.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub format: u32,
    pub abi_version: u32,
    pub compiler_abi: u32,
    pub verifier_version: u32,
    /// The semantic hash of the program this world runs.
    pub module_semantic: [u8; 32],
    /// The canonical content digest of the closed root machine result
    /// type. `SnapshotImage.cast_result` compares it.
    ///
    /// The digest names a class by definition hash and an effect by
    /// text, so it never depends on a numeric slot of one linked
    /// program. A machine with no body function records zeros.
    pub result_type: [u8; 32],
    /// Every referenced function slot with its definition hash.
    pub funcs: Vec<(u32, [u8; 32])>,
    /// Every referenced class slot with its definition hash.
    pub classes: Vec<(u32, [u8; 32])>,
    /// The closed type table of this image.
    ///
    /// No entry holds a free type variable, and every child index
    /// names an earlier entry, so a walk over one node terminates.
    pub types: Vec<lm_bytecode::closed::ClosedType>,
    /// The type environment table of this image. Ordinal zero is the
    /// empty environment.
    pub envs: Vec<lm_bytecode::closed::TypeEnv>,
    pub machines: Vec<ImageMachine>,
}

impl Image {
    /// The number of captured machines.
    pub fn machine_count(&self) -> usize {
        self.machines.len()
    }

    /// The number of captured mailboxes: one per captured proc.
    pub fn mailbox_count(&self) -> usize {
        self.machines.iter().filter(|m| m.is_proc).count()
    }

    /// The state of the root machine.
    ///
    /// An editable image may hold no machine at all, so inspection of
    /// an invalid image stays total.
    pub fn root_state(&self) -> Option<ImageState> {
        self.machines.first().map(|m| m.state)
    }
}

/// Where the bytes of one admitted image came from.
///
/// The origin supports diagnostics alone. It grants no trust and it
/// selects no restore path: trusted capture and external loading
/// produce the same guarantees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A consistent cut of a live verified world produced the bytes.
    TrustedCapture,
    /// The external loader decoded and admitted the bytes.
    ExternalContainer,
}

/// What one admission proved the image against.
///
/// The container hash identifies bytes. It does not say which program
/// admitted them, so the admitted state carries this record beside the
/// bytes. Restore compares it with the running program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionIdentity {
    /// The semantic hash of the program the image runs.
    pub module_semantic: [u8; 32],
    /// The verification hash of that exact verified module.
    pub verification: [u8; 32],
    pub format: u32,
    pub abi_version: u32,
    pub compiler_abi: u32,
    pub verifier_version: u32,
}

/// One admitted image plus its canonical bytes.
///
/// The fields stay private, so no host code can edit an admitted
/// image. `into_image` converts back into editable data, and that data
/// needs admission again before it restores.
///
/// The bytes and the admitted world always agree: capture produces
/// both, and the loader admits the bytes it received. A restore reads
/// `world` and repeats no structural check, which is what
/// specification 17.8 asks for.
#[derive(Debug, Clone)]
pub struct SnapshotImage {
    bytes: std::sync::Arc<Vec<u8>>,
    world: std::sync::Arc<Image>,
    /// The container hash of the bytes.
    hash: [u8; 32],
    /// The program and ABI identity this image passed admission
    /// against.
    identity: AdmissionIdentity,
    /// Where the bytes came from. This is provenance, not trust.
    origin: Origin,
}

impl SnapshotImage {
    /// The canonical container bytes.
    pub fn bytes(&self) -> &std::sync::Arc<Vec<u8>> {
        &self.bytes
    }

    /// The admitted machine world.
    pub fn world(&self) -> &Image {
        &self.world
    }

    /// One editable copy of the admitted world.
    ///
    /// An arbitrary edit destroys every admitted property, so the
    /// result is ordinary `Image` data. It needs admission again
    /// before it restores.
    pub fn into_image(&self) -> Image {
        (*self.world).clone()
    }

    /// The container hash.
    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }

    /// The program and ABI identity this image passed admission
    /// against.
    pub fn identity(&self) -> AdmissionIdentity {
        self.identity
    }

    /// Where these bytes came from. The value is provenance alone.
    pub fn origin(&self) -> Origin {
        self.origin
    }

    /// The semantic type digest of the root machine result type.
    pub fn result_type(&self) -> [u8; 32] {
        self.world.result_type
    }

    /// Check the result type of this image against an expected type
    /// digest (specification 17.1).
    ///
    /// The guest form `cast_result[T](expected: Type[T])` needs a
    /// `Type[T]` descriptor, which version 0.2 does not have. The host
    /// form below carries the same rule.
    pub fn cast_result(&self, expected: [u8; 32]) -> Result<&SnapshotImage, SnapshotTypeError> {
        if self.world.result_type == expected {
            Ok(self)
        } else {
            Err(SnapshotTypeError {
                found: self.world.result_type,
                expected,
            })
        }
    }
}

/// The result-type mismatch of one `cast_result`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotTypeError {
    pub found: [u8; 32],
    pub expected: [u8; 32],
}

/// Why one capture produced no image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotFail {
    /// One machine of the world holds a live host attachment
    /// (specification 17.4). The path starts at the root.
    ResourceActive { path: Vec<u32>, kind: String },
    /// The capture passed the configured snapshot byte limit.
    LimitExceeded,
    /// The world is not in a state a cut can copy. This is a machine
    /// fault, not one of the two ordinary errors.
    Fault(FaultCode, String),
}

/// Why one restore built no world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreFail {
    /// The restored world passed a machine, heap, or graph limit.
    LimitExceeded,
    /// The image passed admission against another program than the one
    /// this world runs.
    ///
    /// Every function slot, class slot, and literal slot of an image
    /// belongs to the program that admitted it. A restore into another
    /// program would read those slots against the wrong tables.
    OtherProgram,
}

/// The stage that rejected one container.
///
/// Decoding protects the host from the byte stream. Admission proves
/// resolved structure and accurate live types. The two stages report
/// separately, because an editor can produce an admission failure with
/// no container behind it (specification section 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageStage {
    /// The container bytes are malformed or excessive.
    Decode,
    /// The image structure does not resolve, or a live type is not
    /// accurate.
    Admission,
}

impl ImageStage {
    pub fn name(self) -> &'static str {
        match self {
            ImageStage::Decode => "decode",
            ImageStage::Admission => "admission",
        }
    }
}

/// Why the loader rejected untrusted bytes.
///
/// The reason is stable, so a corruption test names the rule it broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageReason {
    /// The container magic is not the snapshot magic.
    Magic,
    /// The format, ABI, compiler, or verifier version disagrees.
    Version,
    /// The reader needed more bytes than the input holds.
    Truncated,
    /// An integer is not in canonical LEB128 form, or it does not fit.
    NonCanonicalInteger,
    /// The section table is out of order, overlapping, or outside the
    /// container.
    SectionBounds,
    /// The stored container hash does not match the bytes.
    ContainerHash,
    /// A count or a size passed a load limit before any allocation.
    LimitExceeded,
    /// A code or class slot is missing, out of range, or carries the
    /// wrong definition hash.
    Code,
    /// A frame, a program counter, or an arena length is not legal.
    Layout,
    /// An object, machine, or handle reference is out of range or
    /// names the wrong target.
    Reference,
    /// A machine state, a pending request, or a paused flag is not
    /// consistent.
    State,
    /// A mailbox type, limit, queue, or close flag is not legal.
    Mailbox,
    /// The heap is not in canonical traversal order from its roots.
    Order,
    /// Bytes remain after the last section.
    Trailing,
    /// Admission cannot derive a type the image needs, or a derived
    /// type does not match the target it names.
    ///
    /// `Layout` names a value that carries the wrong shape for a type
    /// admission derived. This reason names the other half: the type
    /// itself has no evidence.
    Type,
    /// One aggregate admission budget ran out.
    Budget,
}

impl ImageReason {
    pub fn name(self) -> &'static str {
        match self {
            ImageReason::Magic => "magic",
            ImageReason::Version => "version",
            ImageReason::Truncated => "truncated",
            ImageReason::NonCanonicalInteger => "non-canonical integer",
            ImageReason::SectionBounds => "section bounds",
            ImageReason::ContainerHash => "container hash",
            ImageReason::LimitExceeded => "limit exceeded",
            ImageReason::Code => "code",
            ImageReason::Layout => "layout",
            ImageReason::Reference => "reference",
            ImageReason::State => "state",
            ImageReason::Mailbox => "mailbox",
            ImageReason::Order => "order",
            ImageReason::Trailing => "trailing bytes",
            ImageReason::Type => "type",
            ImageReason::Budget => "budget",
        }
    }
}

/// One load rejection: the stage, the rule, and a readable detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageError {
    pub stage: ImageStage,
    pub reason: ImageReason,
    pub detail: String,
}

impl ImageError {
    /// One decode rejection.
    pub fn new(reason: ImageReason, detail: impl Into<String>) -> ImageError {
        ImageError {
            stage: ImageStage::Decode,
            reason,
            detail: detail.into(),
        }
    }

    /// One admission rejection.
    pub fn admission(reason: ImageReason, detail: impl Into<String>) -> ImageError {
        ImageError {
            stage: ImageStage::Admission,
            reason,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}: {}",
            self.stage.name(),
            self.reason.name(),
            self.detail
        )
    }
}

/// The load limits the external loader applies before any allocation.
///
/// Every count in the container is checked against a limit and against
/// the bytes that remain, so no untrusted length ever sizes an
/// allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadLimits {
    pub max_bytes: usize,
    pub max_machines: u32,
    pub max_objects: u32,
    pub max_frames: u32,
    pub max_stack_values: u32,
    pub max_mailbox: u32,
    pub max_string_bytes: u32,
    /// The largest code manifest or literal table one container may
    /// name. The decoder reads no program, so this limit replaces the
    /// table lengths of the loaded module.
    pub max_code_slots: u32,
    /// The largest closed type table one container may carry.
    pub max_closed_types: u32,
    /// The largest type environment table one container may carry.
    pub max_type_envs: u32,
}

impl Default for LoadLimits {
    fn default() -> LoadLimits {
        LoadLimits {
            max_bytes: 256 << 20,
            max_machines: 4096,
            max_objects: 1 << 22,
            max_frames: 1 << 20,
            max_stack_values: 1 << 24,
            max_mailbox: 1 << 20,
            max_string_bytes: 1 << 26,
            max_code_slots: 1 << 20,
            max_closed_types: lm_bytecode::closed::DEFAULT_MAX_CLOSED_TYPES,
            max_type_envs: lm_bytecode::closed::DEFAULT_MAX_TYPE_ENVS,
        }
    }
}

/// The canonical snapshot roots of one machine, in canonical order.
///
/// The order is the one declaration point of snapshot reachability.
/// The writer reads it from the live machine and the loader reads it
/// from the image, so the two orders cannot drift.
///
/// The list excludes the policy table. Specification 17.2 excludes
/// policy tables from a snapshot, so a machine or an object that only
/// a table-held mock closure names is not snapshot content
/// (`docs/notes/week8.md`, the week-9 carry-forward).
pub const SNAPSHOT_ROOT_ORDER: &str = "frame closures, locals, operands, pending arguments, \
terminal value, mailbox queue, proc body, literals";

/// Build the canonical snapshot root list of one captured machine.
pub fn image_roots(machine: &ImageMachine) -> Vec<u32> {
    let mut roots: Vec<u32> = Vec::new();
    let push = |value: &Value, roots: &mut Vec<u32>| {
        if let Value::Obj(r) = value {
            roots.push(r.slot);
        }
    };
    for frame in &machine.frames {
        if let Some(r) = frame.closure {
            roots.push(r);
        }
    }
    for value in &machine.locals {
        push(value, &mut roots);
    }
    for value in &machine.operands {
        push(value, &mut roots);
    }
    if let Some(pending) = &machine.pending {
        for value in &pending.args {
            push(value, &mut roots);
        }
    }
    if let Some(ImageTerminal::Done(value)) = &machine.terminal {
        push(value, &mut roots);
    }
    for value in &machine.mailbox.queue {
        push(value, &mut roots);
    }
    if let Some(r) = machine.start_body {
        roots.push(r);
    }
    roots.extend(machine.literals.iter().flatten().copied());
    roots
}

/// One machine world path, for `ResourceActive`.
pub type MachinePath = Vec<VmId>;
