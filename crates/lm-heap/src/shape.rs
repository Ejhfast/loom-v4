//! Native shapes: the payload of one heap object and the immutable
//! shape descriptor that governs it.
//!
//! Each shape declares one descriptor. The descriptor carries the
//! display name, the canonical child order, the frozen-at-birth rule,
//! the boundary policy, the digestibility, and the snapshot
//! classification. Every graph mode reads reachability and order from
//! `Object::children`, so freeze, mark, transfer, copy, digest,
//! verification, inspection, and snapshot traversal share one
//! definition.

use lm_abi::{FaultCode, SnapshotClass};
use lm_value::{ObjRef, Value, Witness};
use std::collections::TryReserveError;

use crate::{NativeByteBuffer, NativeStringBuilder, SharedBytes, SharedText, ValueArray};

/// Logical byte cost of one object header.
pub(crate) const HEADER_COST: usize = 32;
/// The smallest logical byte cost of one heap object.
pub const MIN_OBJECT_COST: usize = HEADER_COST;
/// Logical byte cost of one stored value.
pub(crate) const VALUE_COST: usize = 16;
/// Logical byte cost of one map entry and its semantic hash.
pub(crate) const ENTRY_COST: usize = 2 * VALUE_COST + 8;

/// One collection epoch. Mutation history does not change value equality.
#[derive(Debug, Clone, Copy, Default, Eq)]
pub struct StructuralEpoch(pub u32);

/// One compact execution coordinate retained by a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultSite {
    pub function: u32,
    pub block: u32,
    pub instruction: u32,
}

impl StructuralEpoch {
    /// Start epoch tracking and return the current epoch.
    pub fn observe(&mut self) -> u32 {
        if self.0 == 0 {
            self.0 = 1;
        }
        self.0
    }

    /// Reject a mutation that cannot keep an `Int` epoch.
    pub fn ensure_bumpable(&self) -> Result<(), FaultCode> {
        if self.0 == 0 {
            return Ok(());
        }
        if self.0 == u32::MAX {
            return Err(FaultCode::CollectionEpochExhausted);
        }
        Ok(())
    }

    /// Increment this epoch after one structural mutation.
    pub fn bump(&mut self) -> Result<(), FaultCode> {
        if self.0 == 0 {
            return Ok(());
        }
        self.ensure_bumpable()?;
        self.0 += 1;
        Ok(())
    }
}

impl PartialEq for StructuralEpoch {
    fn eq(&self, _: &StructuralEpoch) -> bool {
        true
    }
}

#[cfg(test)]
mod epoch_tests {
    use super::*;

    #[test]
    fn an_unobserved_epoch_skips_updates() {
        let mut epoch = StructuralEpoch::default();
        epoch.bump().expect("an unobserved update succeeds");
        assert_eq!(epoch.0, 0);
        assert_eq!(epoch.observe(), 1);
        epoch.bump().expect("an observed update succeeds");
        assert_eq!(epoch.0, 2);
    }

    #[test]
    fn a_structural_epoch_never_exceeds_an_int() {
        let mut epoch = StructuralEpoch(u32::MAX - 1);
        epoch.bump().expect("the final valid increment succeeds");
        assert_eq!(epoch.0, u32::MAX);
        assert_eq!(epoch.bump(), Err(FaultCode::CollectionEpochExhausted));
        assert_eq!(epoch.0, u32::MAX);
    }
}

/// A derived open-addressed lookup index for one map.
///
/// The index is a cache over the insertion-ordered entries: `built`
/// counts the indexed prefix, and lookups extend it on demand.
/// Iteration, display, equality, and digest semantics never read it.
/// It holds no object references, so every graph mode skips it by
/// design.
///
/// The table doubles at a load factor of two thirds, so it holds
/// between 1.5 and 3 slots for each entry. One slot is 16 bytes.
/// `ENTRY_COST` charges 40 bytes for each entry.
/// This charge includes each stored semantic hash and most index capacity.
#[derive(Debug, Clone, Default)]
pub struct MapIndex {
    /// The raw entry prefix that the table already indexes.
    pub built: u32,
    /// The number of live entries.
    live: u32,
    /// The structural epoch stored in existing index padding.
    pub epoch: StructuralEpoch,
    slots: Box<[MapSlot]>,
}

const EMPTY_MAP_ENTRY: u32 = u32::MAX;
const MIN_MAP_SLOTS: usize = 8;

#[derive(Debug, Clone, Copy)]
struct MapSlot {
    hash: u64,
    entry: u32,
}

impl MapSlot {
    const EMPTY: MapSlot = MapSlot {
        hash: 0,
        entry: EMPTY_MAP_ENTRY,
    };
}

impl MapIndex {
    /// Create an empty derived index for a known live count.
    pub fn with_live(epoch: StructuralEpoch, live: usize) -> Self {
        Self {
            built: 0,
            live: u32::try_from(live).expect("map entry count fits u32"),
            epoch,
            slots: Box::default(),
        }
    }

    /// Get the number of live entries.
    pub fn live_len(&self) -> usize {
        self.live as usize
    }

    /// Add one live entry and index it.
    pub fn push_live(&mut self, hash: u64, entry: u32) {
        self.live = self.live.saturating_add(1);
        self.insert(hash, entry);
    }

    /// Advance the indexed prefix over one tombstone.
    pub fn skip_tombstone(&mut self, entry: u32) {
        debug_assert_eq!(entry, self.built);
        self.built += 1;
    }

    /// Record one removed entry.
    pub fn record_removal(&mut self) {
        debug_assert!(self.live > 0);
        self.live -= 1;
    }

    /// Test whether the map needs stable compaction.
    pub fn needs_compaction(&self, storage_len: usize) -> bool {
        let tombstones = storage_len.saturating_sub(self.live_len());
        tombstones >= 8 && tombstones.saturating_mul(3) > storage_len
    }

    /// Drop derived data after stable compaction.
    pub fn record_compaction(&mut self) {
        self.built = 0;
        self.slots = Box::default();
    }

    /// Reset one empty map.
    pub fn reset(&mut self) {
        self.built = 0;
        self.live = 0;
        self.slots = Box::default();
    }

    /// Entry indices whose stored key has this hash.
    pub fn candidates(&self, hash: u64) -> MapCandidates<'_> {
        let remaining = self.slots.len();
        let next = map_slot(hash, remaining);
        MapCandidates {
            slots: &self.slots,
            hash,
            next,
            remaining,
        }
    }

    /// Find one candidate slot after an optional prior slot.
    pub fn probe(&self, hash: u64, prior: Option<u32>) -> Option<(u32, u32)> {
        if self.slots.is_empty() {
            return None;
        }
        let mut next = match prior {
            Some(slot) => (slot as usize + 1) & (self.slots.len() - 1),
            None => map_slot(hash, self.slots.len()),
        };
        let mut remaining = self.slots.len();
        while remaining > 0 {
            let slot = self.slots[next];
            if slot.entry == EMPTY_MAP_ENTRY {
                return None;
            }
            if slot.hash == hash {
                return Some((next as u32, slot.entry));
            }
            next = (next + 1) & (self.slots.len() - 1);
            remaining -= 1;
        }
        None
    }

    /// Get the entry stored in one occupied index slot.
    pub fn entry_at(&self, slot: u32) -> Option<u32> {
        let slot = self.slots.get(slot as usize)?;
        (slot.entry != EMPTY_MAP_ENTRY).then_some(slot.entry)
    }

    /// Add one indexed entry.
    pub fn insert(&mut self, hash: u64, entry: u32) {
        debug_assert_eq!(entry, self.built);
        let required = self.live_len().max(self.built as usize + 1);
        if self.slots.is_empty() || required * 3 > self.slots.len() * 2 {
            self.grow();
        }
        insert_map_slot(&mut self.slots, hash, entry);
        self.built += 1;
    }

    /// Clear the derived lookup table and keep the structural epoch.
    pub fn clear(&mut self) {
        self.built = 0;
        self.slots = Box::default();
    }

    fn grow(&mut self) {
        let new_len = (self.slots.len() * 2).max(MIN_MAP_SLOTS);
        let old = std::mem::replace(
            &mut self.slots,
            vec![MapSlot::EMPTY; new_len].into_boxed_slice(),
        );
        for slot in old {
            if slot.entry != EMPTY_MAP_ENTRY {
                insert_map_slot(&mut self.slots, slot.hash, slot.entry);
            }
        }
    }
}

/// Candidate entries from one open-addressed probe.
pub struct MapCandidates<'a> {
    slots: &'a [MapSlot],
    hash: u64,
    next: usize,
    remaining: usize,
}

impl Iterator for MapCandidates<'_> {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        while self.remaining > 0 {
            let slot = self.slots[self.next];
            self.next = (self.next + 1) & (self.slots.len() - 1);
            self.remaining -= 1;
            if slot.entry == EMPTY_MAP_ENTRY {
                self.remaining = 0;
                return None;
            }
            if slot.hash == self.hash {
                return Some(slot.entry);
            }
        }
        None
    }
}

fn insert_map_slot(slots: &mut [MapSlot], hash: u64, entry: u32) {
    let mut at = map_slot(hash, slots.len());
    loop {
        if slots[at].entry == EMPTY_MAP_ENTRY {
            slots[at] = MapSlot { hash, entry };
            return;
        }
        at = (at + 1) & (slots.len() - 1);
    }
}

fn map_slot(hash: u64, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    debug_assert!(len.is_power_of_two());
    let mixed = hash ^ hash.rotate_right(25) ^ hash.rotate_left(17);
    mixed as usize & (len - 1)
}

impl PartialEq for MapIndex {
    /// The index is derived data, so it never takes part in object
    /// equality.
    fn eq(&self, _: &MapIndex) -> bool {
        true
    }
}

/// One insertion-ordered map entry.
#[derive(Debug, Clone, Copy)]
pub struct MapEntry {
    pub key: Value,
    pub value: Value,
    /// The stable semantic hash of `key`.
    pub semantic_hash: i64,
}

impl MapEntry {
    /// Test whether this entry contains a key and value.
    pub fn is_live(&self) -> bool {
        !matches!(self.key, Value::Uninit)
    }

    /// Replace this entry with an internal tombstone.
    pub fn remove(&mut self) -> Value {
        debug_assert!(self.is_live());
        self.key = Value::Uninit;
        self.semantic_hash = 0;
        std::mem::replace(&mut self.value, Value::Uninit)
    }
}

impl PartialEq for MapEntry {
    /// A cached semantic hash does not change entry equality.
    fn eq(&self, other: &MapEntry) -> bool {
        self.key == other.key && self.value == other.value
    }
}

/// One portable verified-code value kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortableCodeKind {
    Artifact,
    VerifiedModule,
    SlotSpec,
    Function,
    Class,
}

/// One holder-local installed-code handle kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeHandleKind {
    Instance,
    Slot,
    Function,
    Class,
    FunctionBinding,
    ClassBinding,
}

/// One pending slot replacement kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotChangeKind {
    Function,
    Class,
    Value,
    Process,
}

/// One immutable portable code payload.
#[derive(Debug, Clone, PartialEq)]
pub struct PortableCode {
    pub kind: PortableCodeKind,
    pub bytes: SharedBytes,
    /// The selected slot for a `SlotSpec` view.
    ///
    /// A function or class artifact selects its definition through
    /// its sole root export. Other views do not use this value.
    pub slot: Option<u32>,
    /// The selected debug definition origin, when source data exists.
    pub origin: Option<[u8; 32]>,
}

/// The payload of one heap object.
#[derive(Debug, Clone, PartialEq)]
#[repr(C, u32)]
pub enum Object {
    /// Immutable UTF-8 text. Born frozen.
    Str(SharedText) = 0,
    /// A class instance. Fields follow the class layout. A field holds
    /// `Value::Uninit` before its first assignment.
    ///
    /// `env` is the witness: the closed type arguments of the class.
    /// The allocation site supplies them, so a reflection query can
    /// read them from the object alone.
    Instance {
        class: u32,
        fields: ValueArray,
        env: Witness,
    } = 1,
    /// A growable list.
    List {
        items: ValueArray,
        epoch: StructuralEpoch,
    } = 2,
    /// A map with entries in insertion order plus a derived lookup
    /// index.
    Map {
        entries: Vec<MapEntry>,
        index: MapIndex,
    } = 3,
    /// A fixed-arity immutable tuple. Born frozen.
    Tuple { items: ValueArray } = 4,
    /// A closure: code index plus captured values. Born frozen.
    ///
    /// `env` is the witness: the type environment of the frame that
    /// created the closure. A closure outlives that frame, and a
    /// capture type can name a type variable the closure signature
    /// does not hold, so the value must retain the environment.
    Closure {
        func: u32,
        captures: ValueArray,
        env: Witness,
    } = 5,
    /// A string builder.
    StrBuilder(NativeStringBuilder),
    /// A byte buffer.
    ByteBuf(NativeByteBuffer),
    /// Immutable binary data. Born frozen.
    Bytes(SharedBytes),
    /// A holder-local handle to one persistent VM image.
    NativeVm { image: u32, generation: u32 },
    /// A holder-local handle to one active or stopped invocation.
    NativeRun { vm: u32 },
    /// A holder-local reference to the dynamic result of one machine.
    ///
    /// The value stays in the machine that produced it. The reference
    /// names that machine and its generation.
    NativeDynRef { vm: u32, generation: u32 },
    /// Portable code bytes and an optional source table index.
    NativeCode(Box<PortableCode>),
    /// A holder-local handle into one installed VM image.
    NativeCodeHandle {
        image: u32,
        generation: u32,
        instance: u32,
        kind: CodeHandleKind,
        index: u32,
    },
    /// One validated replacement plan for an image slot.
    NativeSlotChange {
        image: u32,
        generation: u32,
        slot: u32,
        version: u64,
        kind: SlotChangeKind,
        target: Value,
    },
    /// A holder-local handle to the policy table of one machine.
    NativeTable { vm: u32 },
    /// A holder-local token for one pending perform of one machine.
    NativeRequest { vm: u32, ordinal: u64 },
    /// A typed pending-call token: the machine, the request ordinal,
    /// and the exact operation slot the token was minted for.
    NativeCall { vm: u32, ordinal: u64, op: u32 },
    /// A frozen machine fault value.
    NativeFault {
        code: FaultCode,
        message: String,
        op: Option<u32>,
        trace: Box<[FaultSite]>,
    },
    /// A frozen canonical graph digest. Born frozen.
    NativeDigest([u8; 32]),
    /// A stable proc reference: the machine identifier and the
    /// generation of that slot. Born frozen and sendable, so send
    /// rights travel as data (specification 18.5).
    NativeHandle { proc: u32, generation: u32 },
    /// One canonical snapshot image: the immutable container bytes of
    /// one captured machine world (specification 17.9).
    ///
    /// The payload never changes, so two heaps that hold one image
    /// share the storage. Specification 16.1 permits that, because no
    /// program can observe the difference.
    NativeSnapshot(std::sync::Arc<Vec<u8>>),
    /// A holder-local handle to one admitted image of this world.
    ///
    /// A live heap holds this shape. The image itself lives in the
    /// world, so a capture and a restore of the same image copy no
    /// bytes. A captured world holds `NativeSnapshot` instead,
    /// because a container states its own bytes.
    NativeSnapshotRef { image: u32 },
    /// A file resource designator. Zero marks a closed handle.
    NativeFileHandle { resource: u64 },
    /// A holder-local control for one file resource.
    NativeResourceHandle { surface: u32, resource: u64 },
    /// A holder-local one-shot wait token.
    NativeWait { owner: u32, token: u64 },
    /// An immutable UTF-8 view. Born frozen.
    Substring(SharedText),
    /// A TCP stream resource designator. Zero marks a closed handle.
    NativeTcpStream { resource: u64 },
    /// A TCP listener resource designator. Zero marks a closed handle.
    NativeTcpListener { resource: u64 },
    /// A TLS stream resource designator. Zero marks a closed handle.
    NativeTlsStream { resource: u64 },
    /// A raw terminal mode designator. Zero marks a closed handle.
    NativeRawMode { resource: u64 },
    /// A process signal stream designator. Zero marks a closed handle.
    NativeSignalStream { resource: u64 },
    /// A pipe read-end designator. Zero marks a closed handle.
    NativePipeReader { resource: u64 },
    /// A pipe write-end designator. Zero marks a closed handle.
    NativePipeWriter { resource: u64 },
    /// An operating-system child designator. Zero marks a closed handle.
    NativeChild { resource: u64 },
    /// A UDP socket designator. Zero marks a closed handle.
    NativeUdpSocket { resource: u64 },
    /// An opaque extension resource. Zero marks a closed handle.
    NativeHostResource { kind: [u8; 32], resource: u64 },
    /// One value with its closed static type. Born frozen.
    DynValue { value: Value, ty: u32 },
}

/// How a boundary transfer treats one shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryPolicy {
    /// The shape crosses a transfer as a copy.
    Sendable,
    /// The shape stays in the heap that holds it.
    HolderLocal,
}

/// One immutable native shape descriptor.
///
/// The descriptor is the single declaration point required by
/// specification 25.5. Every field answers one question a graph mode
/// asks, and no mode carries its own shape table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeDesc {
    /// The stable display name of the shape.
    pub name: &'static str,
    /// True when the payload can hold object references.
    pub has_refs: bool,
    /// True when the object is frozen at birth.
    pub born_frozen: bool,
    /// The canonical child order, as readable text. The graph engine
    /// visits children in exactly this order.
    pub child_order: &'static str,
    /// How a boundary transfer treats the shape.
    pub boundary: BoundaryPolicy,
    /// True when the canonical digest encoder accepts the shape.
    pub digestible: bool,
    /// The snapshot classification (specification 16.4).
    pub snapshot: SnapshotClass,
}

/// The shape table. Every entry states one classification per column.
const SHAPE_STR: ShapeDesc = ShapeDesc {
    name: "String",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: true,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_INSTANCE: ShapeDesc = ShapeDesc {
    name: "Instance",
    has_refs: true,
    born_frozen: false,
    child_order: "fields in class layout order",
    boundary: BoundaryPolicy::Sendable,
    digestible: true,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_LIST: ShapeDesc = ShapeDesc {
    name: "List",
    has_refs: true,
    born_frozen: false,
    child_order: "items in index order",
    boundary: BoundaryPolicy::Sendable,
    digestible: true,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_MAP: ShapeDesc = ShapeDesc {
    name: "Map",
    has_refs: true,
    born_frozen: false,
    child_order: "entries in insertion order, key before value",
    boundary: BoundaryPolicy::Sendable,
    digestible: true,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_TUPLE: ShapeDesc = ShapeDesc {
    name: "Tuple",
    has_refs: true,
    born_frozen: true,
    child_order: "elements in index order",
    boundary: BoundaryPolicy::Sendable,
    digestible: true,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_CLOSURE: ShapeDesc = ShapeDesc {
    name: "Closure",
    has_refs: true,
    born_frozen: true,
    child_order: "captures in capture-list order",
    boundary: BoundaryPolicy::Sendable,
    digestible: true,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_SB: ShapeDesc = ShapeDesc {
    name: "StringBuilder",
    has_refs: false,
    born_frozen: false,
    child_order: "none",
    boundary: BoundaryPolicy::HolderLocal,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_BB: ShapeDesc = ShapeDesc {
    name: "ByteBuffer",
    has_refs: false,
    born_frozen: false,
    child_order: "none",
    boundary: BoundaryPolicy::HolderLocal,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_VM: ShapeDesc = ShapeDesc {
    name: "Vm",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::HolderLocal,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_RUN: ShapeDesc = ShapeDesc {
    name: "Run",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::HolderLocal,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_DYN_REF: ShapeDesc = ShapeDesc {
    name: "DynRef",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::HolderLocal,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_TABLE: ShapeDesc = ShapeDesc {
    name: "PolicyTable",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::HolderLocal,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_REQUEST: ShapeDesc = ShapeDesc {
    name: "Request",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::HolderLocal,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_CALL: ShapeDesc = ShapeDesc {
    name: "PendingCall",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::HolderLocal,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_FAULT: ShapeDesc = ShapeDesc {
    name: "Fault",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: true,
    snapshot: SnapshotClass::MachineState,
};
const SHAPE_DIGEST: ShapeDesc = ShapeDesc {
    name: "Digest",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: true,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_HANDLE: ShapeDesc = ShapeDesc {
    name: "Handle",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    // A handle is a typed designator of one machine of this world.
    // It crosses a transfer and keeps its target, so send rights
    // travel as data.
    boundary: BoundaryPolicy::Sendable,
    // The identity of a proc is world-local, not value-local, so no
    // canonical digest can name it.
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_SNAPSHOT: ShapeDesc = ShapeDesc {
    name: "Snapshot",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    // A snapshot is machine state whose bytes the codec can copy
    // (specification 16.2 and 16.4), so it crosses a boundary.
    boundary: BoundaryPolicy::Sendable,
    // The container carries its own canonical hash, and the value
    // digest encodes guest graphs. A snapshot therefore takes no
    // second canonical encoding.
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_SNAPSHOT_REF: ShapeDesc = ShapeDesc {
    name: "SnapshotRef",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    // The slot names one admitted image of the world that holds the
    // heap. Every machine of one world reads the same table, so the
    // value crosses a boundary inside that world.
    boundary: BoundaryPolicy::Sendable,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_BYTES: ShapeDesc = ShapeDesc {
    name: "Bytes",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: true,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_FILE_HANDLE: ShapeDesc = ShapeDesc {
    name: "FileHandle",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_RESOURCE_HANDLE: ShapeDesc = ShapeDesc {
    name: "ResourceHandle",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::HolderLocal,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_WAIT: ShapeDesc = ShapeDesc {
    name: "Wait",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::HolderLocal,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_SUBSTRING: ShapeDesc = ShapeDesc {
    name: "Substring",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: true,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_TCP_STREAM: ShapeDesc = ShapeDesc {
    name: "TcpStream",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_TCP_LISTENER: ShapeDesc = ShapeDesc {
    name: "TcpListener",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_TLS_STREAM: ShapeDesc = ShapeDesc {
    name: "TlsStream",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_RAW_MODE: ShapeDesc = ShapeDesc {
    name: "RawMode",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_SIGNAL_STREAM: ShapeDesc = ShapeDesc {
    name: "SignalStream",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_PIPE_READER: ShapeDesc = ShapeDesc {
    name: "PipeReader",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_PIPE_WRITER: ShapeDesc = ShapeDesc {
    name: "PipeWriter",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_CHILD: ShapeDesc = ShapeDesc {
    name: "Child",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_UDP_SOCKET: ShapeDesc = ShapeDesc {
    name: "UdpSocket",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_HOST_RESOURCE: ShapeDesc = ShapeDesc {
    name: "HostResource",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::HolderLocal,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_CODE: ShapeDesc = ShapeDesc {
    name: "PortableCode",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::Sendable,
    digestible: true,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_CODE_HANDLE: ShapeDesc = ShapeDesc {
    name: "CodeHandle",
    has_refs: false,
    born_frozen: true,
    child_order: "none",
    boundary: BoundaryPolicy::HolderLocal,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_DYN_VALUE: ShapeDesc = ShapeDesc {
    name: "DynValue",
    has_refs: true,
    born_frozen: true,
    child_order: "the packaged value",
    boundary: BoundaryPolicy::Sendable,
    digestible: true,
    snapshot: SnapshotClass::MachineState,
};

const SHAPE_SLOT_CHANGE: ShapeDesc = ShapeDesc {
    name: "SlotChange",
    has_refs: true,
    born_frozen: true,
    child_order: "the replacement target",
    boundary: BoundaryPolicy::HolderLocal,
    digestible: false,
    snapshot: SnapshotClass::MachineState,
};

/// Every shape descriptor, in shape-tag order. The tag is the index,
/// and the canonical digest encoding reads it.
pub const SHAPES: [&ShapeDesc; 38] = [
    &SHAPE_STR,
    &SHAPE_INSTANCE,
    &SHAPE_LIST,
    &SHAPE_MAP,
    &SHAPE_TUPLE,
    &SHAPE_CLOSURE,
    &SHAPE_SB,
    &SHAPE_BB,
    &SHAPE_VM,
    &SHAPE_TABLE,
    &SHAPE_REQUEST,
    &SHAPE_CALL,
    &SHAPE_FAULT,
    &SHAPE_DIGEST,
    &SHAPE_HANDLE,
    &SHAPE_SNAPSHOT,
    &SHAPE_BYTES,
    &SHAPE_FILE_HANDLE,
    &SHAPE_RESOURCE_HANDLE,
    &SHAPE_WAIT,
    &SHAPE_SUBSTRING,
    &SHAPE_TCP_STREAM,
    &SHAPE_TCP_LISTENER,
    &SHAPE_TLS_STREAM,
    &SHAPE_SNAPSHOT_REF,
    &SHAPE_RUN,
    &SHAPE_CODE,
    &SHAPE_CODE_HANDLE,
    &SHAPE_DYN_VALUE,
    &SHAPE_SLOT_CHANGE,
    &SHAPE_HOST_RESOURCE,
    &SHAPE_RAW_MODE,
    &SHAPE_SIGNAL_STREAM,
    &SHAPE_PIPE_READER,
    &SHAPE_PIPE_WRITER,
    &SHAPE_CHILD,
    &SHAPE_UDP_SOCKET,
    &SHAPE_DYN_REF,
];

impl Object {
    /// Clone this object and remap its object references.
    ///
    /// Every mutable buffer uses a fallible exact reservation.
    /// Immutable String storage remains shared.
    ///
    /// The map index is derived data, so the clone starts empty.
    pub fn try_clone_remapped(
        &self,
        mut map: impl FnMut(ObjRef) -> ObjRef,
    ) -> Result<Object, TryReserveError> {
        fn copy_string(source: &str) -> Result<String, TryReserveError> {
            let mut target = String::new();
            target.try_reserve_exact(source.len())?;
            target.push_str(source);
            Ok(target)
        }
        fn copy_values(
            source: &[Value],
            capacity: usize,
            map: &mut impl FnMut(ObjRef) -> ObjRef,
        ) -> Result<Vec<Value>, TryReserveError> {
            let mut target = Vec::new();
            target.try_reserve_exact(capacity)?;
            for value in source {
                target.push(match value {
                    Value::Obj(reference) => Value::Obj(map(*reference)),
                    other => *other,
                });
            }
            Ok(target)
        }

        Ok(match self {
            Object::Str(text) => Object::Str(text.clone()),
            Object::Instance { class, fields, env } => Object::Instance {
                class: *class,
                fields: copy_values(fields, fields.len(), &mut map)?.into(),
                env: *env,
            },
            Object::List { items, epoch } => Object::List {
                items: copy_values(items, items.capacity(), &mut map)?.into(),
                epoch: *epoch,
            },
            Object::Map { entries, index } => {
                let mut copied = Vec::new();
                copied.try_reserve_exact(entries.capacity())?;
                for entry in entries {
                    if !entry.is_live() {
                        continue;
                    }
                    let key = match entry.key {
                        Value::Obj(reference) => Value::Obj(map(reference)),
                        other => other,
                    };
                    let value = match entry.value {
                        Value::Obj(reference) => Value::Obj(map(reference)),
                        other => other,
                    };
                    copied.push(MapEntry {
                        key,
                        value,
                        semantic_hash: entry.semantic_hash,
                    });
                }
                let copied_index = MapIndex::with_live(index.epoch, copied.len());
                Object::Map {
                    entries: copied,
                    index: copied_index,
                }
            }
            Object::Tuple { items } => Object::Tuple {
                items: copy_values(items, items.len(), &mut map)?.into(),
            },
            Object::Closure {
                func,
                captures,
                env,
            } => Object::Closure {
                func: *func,
                captures: copy_values(captures, captures.len(), &mut map)?.into(),
                env: *env,
            },
            Object::StrBuilder(text) => Object::StrBuilder(text.try_clone_buffer()?),
            Object::ByteBuf(bytes) => Object::ByteBuf(bytes.try_clone_buffer()?),
            Object::Bytes(bytes) => Object::Bytes(bytes.clone()),
            Object::NativeVm { image, generation } => Object::NativeVm {
                image: *image,
                generation: *generation,
            },
            Object::NativeRun { vm } => Object::NativeRun { vm: *vm },
            Object::NativeDynRef { vm, generation } => Object::NativeDynRef {
                vm: *vm,
                generation: *generation,
            },
            Object::NativeCode(code) => Object::NativeCode(Box::new((**code).clone())),
            Object::NativeCodeHandle {
                image,
                generation,
                instance,
                kind,
                index,
            } => Object::NativeCodeHandle {
                image: *image,
                generation: *generation,
                instance: *instance,
                kind: *kind,
                index: *index,
            },
            Object::NativeSlotChange {
                image,
                generation,
                slot,
                version,
                kind,
                target,
            } => Object::NativeSlotChange {
                image: *image,
                generation: *generation,
                slot: *slot,
                version: *version,
                kind: *kind,
                target: match target {
                    Value::Obj(reference) => Value::Obj(map(*reference)),
                    other => *other,
                },
            },
            Object::NativeTable { vm } => Object::NativeTable { vm: *vm },
            Object::NativeRequest { vm, ordinal } => Object::NativeRequest {
                vm: *vm,
                ordinal: *ordinal,
            },
            Object::NativeCall { vm, ordinal, op } => Object::NativeCall {
                vm: *vm,
                ordinal: *ordinal,
                op: *op,
            },
            Object::NativeFault {
                code,
                message,
                op,
                trace,
            } => Object::NativeFault {
                code: *code,
                message: copy_string(message)?,
                op: *op,
                trace: trace.clone(),
            },
            Object::NativeDigest(bytes) => Object::NativeDigest(*bytes),
            Object::NativeHandle { proc, generation } => Object::NativeHandle {
                proc: *proc,
                generation: *generation,
            },
            Object::NativeSnapshot(image) => Object::NativeSnapshot(image.clone()),
            Object::NativeSnapshotRef { image } => Object::NativeSnapshotRef { image: *image },
            Object::NativeFileHandle { resource } => Object::NativeFileHandle {
                resource: *resource,
            },
            Object::NativeResourceHandle { surface, resource } => Object::NativeResourceHandle {
                surface: *surface,
                resource: *resource,
            },
            Object::NativeWait { owner, token } => Object::NativeWait {
                owner: *owner,
                token: *token,
            },
            Object::Substring(text) => Object::Substring(text.clone()),
            Object::NativeTcpStream { resource } => Object::NativeTcpStream {
                resource: *resource,
            },
            Object::NativeTcpListener { resource } => Object::NativeTcpListener {
                resource: *resource,
            },
            Object::NativeTlsStream { resource } => Object::NativeTlsStream {
                resource: *resource,
            },
            Object::NativeRawMode { resource } => Object::NativeRawMode {
                resource: *resource,
            },
            Object::NativeSignalStream { resource } => Object::NativeSignalStream {
                resource: *resource,
            },
            Object::NativePipeReader { resource } => Object::NativePipeReader {
                resource: *resource,
            },
            Object::NativePipeWriter { resource } => Object::NativePipeWriter {
                resource: *resource,
            },
            Object::NativeChild { resource } => Object::NativeChild {
                resource: *resource,
            },
            Object::NativeUdpSocket { resource } => Object::NativeUdpSocket {
                resource: *resource,
            },
            Object::NativeHostResource { kind, resource } => Object::NativeHostResource {
                kind: *kind,
                resource: *resource,
            },
            Object::DynValue { value, ty } => Object::DynValue {
                value: match value {
                    Value::Obj(reference) => Value::Obj(map(*reference)),
                    other => *other,
                },
                ty: *ty,
            },
        })
    }

    /// The shape tag of this object: its index in `SHAPES`. The
    /// canonical digest encoding uses the tag, so the order is part
    /// of the digest contract.
    pub fn tag(&self) -> u8 {
        match self {
            Object::Str(_) => 0,
            Object::Instance { .. } => 1,
            Object::List { .. } => 2,
            Object::Map { .. } => 3,
            Object::Tuple { .. } => 4,
            Object::Closure { .. } => 5,
            Object::StrBuilder(_) => 6,
            Object::ByteBuf(_) => 7,
            Object::NativeVm { .. } => 8,
            Object::NativeTable { .. } => 9,
            Object::NativeRequest { .. } => 10,
            Object::NativeCall { .. } => 11,
            Object::NativeFault { .. } => 12,
            Object::NativeDigest(_) => 13,
            Object::NativeHandle { .. } => 14,
            Object::NativeSnapshot(_) => 15,
            Object::Bytes(_) => 16,
            Object::NativeFileHandle { .. } => 17,
            Object::NativeResourceHandle { .. } => 18,
            Object::NativeWait { .. } => 19,
            Object::Substring(_) => 20,
            Object::NativeTcpStream { .. } => 21,
            Object::NativeTcpListener { .. } => 22,
            Object::NativeTlsStream { .. } => 23,
            Object::NativeSnapshotRef { .. } => 24,
            Object::NativeRun { .. } => 25,
            Object::NativeCode(_) => 26,
            Object::NativeCodeHandle { .. } => 27,
            Object::DynValue { .. } => 28,
            Object::NativeSlotChange { .. } => 29,
            Object::NativeHostResource { .. } => 30,
            Object::NativeRawMode { .. } => 31,
            Object::NativeSignalStream { .. } => 32,
            Object::NativePipeReader { .. } => 33,
            Object::NativePipeWriter { .. } => 34,
            Object::NativeChild { .. } => 35,
            Object::NativeUdpSocket { .. } => 36,
            Object::NativeDynRef { .. } => 37,
        }
    }

    /// The shape descriptor for this object.
    pub fn shape(&self) -> &'static ShapeDesc {
        SHAPES[self.tag() as usize]
    }

    /// The logical byte cost charged against the heap cap.
    pub fn cost(&self) -> usize {
        self.heap_base_cost()
            + self
                .shared_allocation()
                .map(|(_, capacity)| capacity)
                .unwrap_or(0)
    }

    /// The object cost without one shared immutable byte allocation.
    pub(crate) fn heap_base_cost(&self) -> usize {
        HEADER_COST
            + match self {
                Object::Str(_) | Object::Bytes(_) | Object::Substring(_) => 0,
                Object::Instance { fields, .. } => fields.len() * VALUE_COST,
                Object::List { items, .. } => items.len() * VALUE_COST,
                Object::Map { entries, .. } => entries.len() * ENTRY_COST,
                Object::Tuple { items } => items.len() * VALUE_COST,
                Object::Closure { captures, .. } => captures.len() * VALUE_COST,
                Object::StrBuilder(s) => s.retained_capacity(),
                Object::ByteBuf(b) => b.retained_capacity(),
                Object::NativeVm { .. }
                | Object::NativeRun { .. }
                | Object::NativeDynRef { .. }
                | Object::NativeCodeHandle { .. }
                | Object::NativeSlotChange { .. }
                | Object::NativeTable { .. }
                | Object::NativeRequest { .. }
                | Object::NativeCall { .. }
                | Object::NativeHandle { .. } => VALUE_COST,
                Object::NativeCode(_) => VALUE_COST,
                Object::NativeFileHandle { .. }
                | Object::NativeResourceHandle { .. }
                | Object::NativeWait { .. }
                | Object::NativeTcpStream { .. }
                | Object::NativeTcpListener { .. } => VALUE_COST,
                Object::NativeTlsStream { .. }
                | Object::NativeRawMode { .. }
                | Object::NativeSignalStream { .. }
                | Object::NativePipeReader { .. }
                | Object::NativePipeWriter { .. }
                | Object::NativeChild { .. }
                | Object::NativeUdpSocket { .. }
                | Object::NativeHostResource { .. } => VALUE_COST,
                Object::NativeFault { message, trace, .. } => message
                    .len()
                    .saturating_add(trace.len().saturating_mul(std::mem::size_of::<FaultSite>())),
                Object::NativeDigest(bytes) => bytes.len(),
                Object::NativeSnapshot(image) => image.len(),
                Object::NativeSnapshotRef { .. } => VALUE_COST,
                Object::DynValue { .. } => VALUE_COST,
            }
    }

    /// Get the identity and capacity of shared immutable byte storage.
    pub(crate) fn shared_allocation(&self) -> Option<(usize, usize)> {
        match self {
            Object::Str(text) | Object::Substring(text) => {
                Some((text.allocation_key(), text.retained_capacity()))
            }
            Object::Bytes(bytes) => Some((bytes.allocation_key(), bytes.retained_capacity())),
            Object::NativeCode(code) => {
                Some((code.bytes.allocation_key(), code.bytes.retained_capacity()))
            }
            _ => None,
        }
    }

    /// Test whether no other value holds the shared allocation.
    pub(crate) fn shared_allocation_is_unique(&self) -> bool {
        match self {
            Object::Str(text) | Object::Substring(text) => text.allocation_is_unique(),
            Object::Bytes(bytes) => bytes.allocation_is_unique(),
            Object::NativeCode(code) => code.bytes.allocation_is_unique(),
            _ => false,
        }
    }

    /// Push every object reference inside this object onto `out`, in
    /// the canonical child order of its shape.
    ///
    /// This is the one shape walker. Every graph mode reads
    /// reachability and order from it, so a new shape changes
    /// tracing, freezing, transfer, digest, and snapshot traversal in
    /// one place.
    pub fn children(&self, out: &mut Vec<ObjRef>) {
        let mut visit = |v: &Value| {
            if let Value::Obj(r) = v {
                out.push(*r);
            }
        };
        match self {
            Object::Str(_)
            | Object::StrBuilder(_)
            | Object::ByteBuf(_)
            | Object::NativeVm { .. }
            | Object::NativeRun { .. }
            | Object::NativeDynRef { .. }
            | Object::NativeCode(_)
            | Object::NativeCodeHandle { .. }
            | Object::NativeTable { .. }
            | Object::NativeRequest { .. }
            | Object::NativeCall { .. }
            | Object::NativeFault { .. }
            | Object::NativeDigest(_)
            | Object::NativeHandle { .. }
            | Object::NativeSnapshot(_)
            | Object::NativeSnapshotRef { .. }
            | Object::Bytes(_)
            | Object::NativeFileHandle { .. }
            | Object::NativeResourceHandle { .. }
            | Object::NativeWait { .. }
            | Object::NativeTcpStream { .. }
            | Object::NativeTcpListener { .. } => {}
            Object::NativeTlsStream { .. }
            | Object::NativeRawMode { .. }
            | Object::NativeSignalStream { .. }
            | Object::NativePipeReader { .. }
            | Object::NativePipeWriter { .. }
            | Object::NativeChild { .. }
            | Object::NativeUdpSocket { .. }
            | Object::NativeHostResource { .. } => {}
            Object::Substring(_) => {}
            Object::DynValue { value, .. } => visit(value),
            Object::NativeSlotChange { target, .. } => visit(target),
            Object::Instance { fields, .. } => fields.iter().for_each(&mut visit),
            Object::List { items, .. } | Object::Tuple { items } => {
                items.iter().for_each(&mut visit)
            }
            Object::Map { entries, .. } => {
                // The index holds hashes and positions only, never an
                // object reference, so the walk covers the entries.
                for entry in entries {
                    if !entry.is_live() {
                        continue;
                    }
                    visit(&entry.key);
                    visit(&entry.value);
                }
            }
            Object::Closure { captures, .. } => captures.iter().for_each(&mut visit),
        }
    }

    /// The empty destination shell of one sendable object.
    ///
    /// Children are patched afterwards. The shell keeps the payload
    /// sizes, so the cost accounting matches the final object.
    ///
    /// A copy preserves the witness: a closure keeps its creator
    /// environment across a boundary, and an instance keeps its class
    /// arguments.
    pub fn shell(&self) -> Option<Object> {
        let shell = match self {
            Object::Str(s) => Object::Str(s.clone()),
            Object::Substring(text) => Object::Substring(text.clone()),
            Object::NativeFault {
                code,
                message,
                op,
                trace,
            } => Object::NativeFault {
                code: *code,
                message: message.clone(),
                op: *op,
                trace: trace.clone(),
            },
            Object::NativeDigest(bytes) => Object::NativeDigest(*bytes),
            Object::NativeHandle { proc, generation } => Object::NativeHandle {
                proc: *proc,
                generation: *generation,
            },
            // The bytes never change, so the copy shares the storage.
            Object::NativeSnapshot(image) => Object::NativeSnapshot(image.clone()),
            Object::NativeSnapshotRef { image } => Object::NativeSnapshotRef { image: *image },
            Object::Bytes(bytes) => Object::Bytes(bytes.clone()),
            Object::NativeCode(code) => Object::NativeCode(Box::new((**code).clone())),
            Object::NativeFileHandle { resource } => Object::NativeFileHandle {
                resource: *resource,
            },
            Object::NativeTcpStream { resource } => Object::NativeTcpStream {
                resource: *resource,
            },
            Object::NativeTcpListener { resource } => Object::NativeTcpListener {
                resource: *resource,
            },
            Object::NativeTlsStream { resource } => Object::NativeTlsStream {
                resource: *resource,
            },
            Object::NativeRawMode { resource } => Object::NativeRawMode {
                resource: *resource,
            },
            Object::NativeSignalStream { resource } => Object::NativeSignalStream {
                resource: *resource,
            },
            Object::NativePipeReader { resource } => Object::NativePipeReader {
                resource: *resource,
            },
            Object::NativePipeWriter { resource } => Object::NativePipeWriter {
                resource: *resource,
            },
            Object::NativeChild { resource } => Object::NativeChild {
                resource: *resource,
            },
            Object::NativeUdpSocket { resource } => Object::NativeUdpSocket {
                resource: *resource,
            },
            Object::Tuple { items } => Object::Tuple {
                items: vec![Value::Unit; items.len()].into(),
            },
            Object::List { items, epoch } => Object::List {
                items: vec![Value::Unit; items.len()].into(),
                epoch: *epoch,
            },
            Object::Map { entries, index } => {
                let copied_index = MapIndex::with_live(index.epoch, index.live_len());
                let mut copied = Vec::with_capacity(entries.capacity());
                copied.extend(entries.iter().filter(|entry| entry.is_live()).map(|entry| {
                    MapEntry {
                        key: Value::Unit,
                        value: Value::Unit,
                        semantic_hash: entry.semantic_hash,
                    }
                }));
                Object::Map {
                    entries: copied,
                    index: copied_index,
                }
            }
            Object::Instance { class, fields, env } => Object::Instance {
                class: *class,
                fields: vec![Value::Unit; fields.len()].into(),
                env: *env,
            },
            Object::Closure {
                func,
                captures,
                env,
            } => Object::Closure {
                func: *func,
                captures: vec![Value::Unit; captures.len()].into(),
                env: *env,
            },
            Object::DynValue { ty, .. } => Object::DynValue {
                value: Value::Unit,
                ty: *ty,
            },
            _ => return None,
        };
        Some(shell)
    }

    /// Rebuild this object with every child reference remapped.
    ///
    /// The order of `map` calls follows the canonical child order, so
    /// a copy reads the same field order the walk reported.
    ///
    /// The rebuilt object keeps its witness, so a closure that crosses
    /// a boundary keeps the environment of the frame that built it.
    pub fn remap(&self, mut map: impl FnMut(ObjRef) -> ObjRef) -> Option<Object> {
        let mut value = |v: Value| match v {
            Value::Obj(child) => Value::Obj(map(child)),
            other => other,
        };
        let out = match self {
            Object::Str(_)
            | Object::Substring(_)
            | Object::NativeFault { .. }
            | Object::NativeDigest(_)
            | Object::NativeHandle { .. }
            | Object::NativeSnapshot(_)
            | Object::NativeSnapshotRef { .. } => return None,
            Object::NativeCode(_) => return None,
            Object::Bytes(_)
            | Object::NativeFileHandle { .. }
            | Object::NativeTcpStream { .. }
            | Object::NativeTcpListener { .. } => return None,
            Object::NativeTlsStream { .. } => return None,
            Object::NativeRawMode { .. }
            | Object::NativeSignalStream { .. }
            | Object::NativePipeReader { .. }
            | Object::NativePipeWriter { .. }
            | Object::NativeChild { .. } => return None,
            Object::NativeUdpSocket { .. } => return None,
            Object::Tuple { items } => Object::Tuple {
                items: items.iter().map(|v| value(*v)).collect(),
            },
            Object::List { items, epoch } => Object::List {
                items: items.iter().map(|v| value(*v)).collect(),
                epoch: *epoch,
            },
            Object::Map { entries, index } => {
                let copied_index = MapIndex::with_live(index.epoch, index.live_len());
                let mut copied = Vec::with_capacity(entries.capacity());
                copied.extend(entries.iter().filter(|entry| entry.is_live()).map(|entry| {
                    MapEntry {
                        key: value(entry.key),
                        value: value(entry.value),
                        semantic_hash: entry.semantic_hash,
                    }
                }));
                Object::Map {
                    entries: copied,
                    // The destination index rebuilds on the first lookup.
                    index: copied_index,
                }
            }
            Object::Instance { class, fields, env } => Object::Instance {
                class: *class,
                fields: fields.iter().map(|v| value(*v)).collect(),
                env: *env,
            },
            Object::Closure {
                func,
                captures,
                env,
            } => Object::Closure {
                func: *func,
                captures: captures.iter().map(|v| value(*v)).collect(),
                env: *env,
            },
            Object::DynValue { value: held, ty } => Object::DynValue {
                value: value(*held),
                ty: *ty,
            },
            Object::NativeSlotChange {
                image,
                generation,
                slot,
                version,
                kind,
                target,
            } => Object::NativeSlotChange {
                image: *image,
                generation: *generation,
                slot: *slot,
                version: *version,
                kind: *kind,
                target: value(*target),
            },
            _ => return None,
        };
        Some(out)
    }
}

/// A readable dump of the shape table, one line per shape.
pub fn dump_shapes() -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (tag, shape) in SHAPES.iter().enumerate() {
        let _ = writeln!(
            out,
            "{tag} {} refs={} born_frozen={} boundary={} digestible={} snapshot={} children={}",
            shape.name,
            shape.has_refs,
            shape.born_frozen,
            match shape.boundary {
                BoundaryPolicy::Sendable => "sendable",
                BoundaryPolicy::HolderLocal => "holder-local",
            },
            shape.digestible,
            shape.snapshot,
            shape.child_order
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_object_payload_stays_compact() {
        let bytes = std::mem::size_of::<Object>();
        assert!(bytes <= 64, "the object payload uses {bytes} bytes");
    }

    #[test]
    fn the_map_index_keeps_all_equal_hash_candidates() {
        let mut index = MapIndex::default();
        index.insert(7, 0);
        index.insert(7, 1);
        index.insert(7, 2);
        assert_eq!(index.candidates(7).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(index.candidates(8).next(), None);
    }

    #[test]
    fn the_map_index_keeps_entries_across_growth() {
        let mut index = MapIndex::default();
        for entry in 0..100u32 {
            index.insert(u64::from(entry) * 17, entry);
        }
        assert_eq!(index.built, 100);
        for entry in 0..100u32 {
            assert_eq!(
                index.candidates(u64::from(entry) * 17).collect::<Vec<_>>(),
                vec![entry]
            );
        }
    }

    /// One sample object per shape, each container holding two
    /// distinct references so an order swap is visible.
    fn sample_objects() -> Vec<Object> {
        let a = ObjRef {
            slot: 11,
            generation: 0,
        };
        let b = ObjRef {
            slot: 22,
            generation: 0,
        };
        vec![
            Object::Str("text".into()),
            Object::Instance {
                class: 3,
                fields: vec![Value::Obj(a), Value::Int(1), Value::Obj(b)].into(),
                env: Witness(lm_value::TypeEnvId(5)),
            },
            Object::List {
                items: vec![Value::Obj(a), Value::Obj(b)].into(),
                epoch: Default::default(),
            },
            Object::Map {
                entries: vec![MapEntry {
                    key: Value::Obj(a),
                    value: Value::Obj(b),
                    semantic_hash: 1,
                }],
                index: MapIndex::with_live(StructuralEpoch::default(), 1),
            },
            Object::Tuple {
                items: vec![Value::Obj(b), Value::Obj(a)].into(),
            },
            Object::Closure {
                func: 4,
                captures: vec![Value::Obj(b), Value::Unit, Value::Obj(a)].into(),
                env: Witness(lm_value::TypeEnvId(7)),
            },
            Object::StrBuilder(NativeStringBuilder::from_string("buffer".to_string())),
            Object::ByteBuf(NativeByteBuffer::from_vec(vec![1, 2, 3])),
            Object::NativeVm {
                image: 1,
                generation: 0,
            },
            Object::NativeTable { vm: 1 },
            Object::NativeRequest { vm: 1, ordinal: 2 },
            Object::NativeCall {
                vm: 1,
                ordinal: 2,
                op: 3,
            },
            Object::NativeFault {
                code: FaultCode::HostFault,
                message: "message".to_string(),
                op: Some(1),
                trace: Box::default(),
            },
            Object::NativeDigest([9; 32]),
            Object::NativeHandle {
                proc: 1,
                generation: 2,
            },
            Object::NativeSnapshot(std::sync::Arc::new(vec![7, 8, 9])),
            Object::Bytes(vec![1, 2, 3].into()),
            Object::NativeFileHandle { resource: 4 },
            Object::NativeResourceHandle {
                surface: 1,
                resource: 4,
            },
            Object::NativeWait { owner: 1, token: 2 },
            Object::Substring("view".into()),
            Object::NativeTcpStream { resource: 5 },
            Object::NativeTcpListener { resource: 6 },
            Object::NativeTlsStream { resource: 7 },
            Object::NativeSnapshotRef { image: 3 },
            Object::NativeRun { vm: 2 },
            Object::NativeCode(Box::new(PortableCode {
                kind: PortableCodeKind::Artifact,
                bytes: vec![1, 2].into(),
                slot: None,
                origin: None,
            })),
            Object::NativeCodeHandle {
                image: 1,
                generation: 0,
                instance: 2,
                kind: CodeHandleKind::Function,
                index: 3,
            },
            Object::DynValue {
                value: Value::Obj(a),
                ty: 4,
            },
            Object::NativeSlotChange {
                image: 1,
                generation: 2,
                slot: 3,
                version: 4,
                kind: SlotChangeKind::Function,
                target: Value::Obj(b),
            },
            Object::NativeHostResource {
                kind: [8; 32],
                resource: 9,
            },
            Object::NativeRawMode { resource: 10 },
            Object::NativeSignalStream { resource: 11 },
            Object::NativePipeReader { resource: 12 },
            Object::NativePipeWriter { resource: 13 },
            Object::NativeChild { resource: 14 },
            Object::NativeUdpSocket { resource: 15 },
            Object::NativeDynRef {
                vm: 2,
                generation: 1,
            },
        ]
    }

    /// The four shape walks must stay in step.
    ///
    /// `children`, `remap`, and the fallible clone use one order.
    /// `shell` keeps each payload size. A new shape or reordered field
    /// breaks this test before a graph mode can attach a wrong child.
    #[test]
    fn the_three_shape_walks_agree() {
        for object in sample_objects() {
            let name = object.shape().name;
            let mut listed = Vec::new();
            object.children(&mut listed);
            let mut mapped = Vec::new();
            match object.remap(|r| {
                mapped.push(r);
                r
            }) {
                Some(rebuilt) => {
                    assert_eq!(listed, mapped, "{name}: remap order");
                    assert_eq!(rebuilt, object, "{name}: an identity remap rebuilds it");
                }
                // A shape that rebuilds nothing must hold no
                // reference, or a copy would drop one.
                None => assert!(listed.is_empty(), "{name}: unmapped children"),
            }
            let mut cloned = Vec::new();
            let rebuilt = object
                .try_clone_remapped(|reference| {
                    cloned.push(reference);
                    reference
                })
                .expect("the sample clone fits");
            assert_eq!(listed, cloned, "{name}: fallible clone order");
            assert_eq!(rebuilt, object, "{name}: a fallible identity clone");
            match object.shell() {
                Some(shell) => {
                    assert_eq!(shell.cost(), object.cost(), "{name}: shell cost");
                    assert_eq!(shell.tag(), object.tag(), "{name}: shell tag");
                    let mut shell_children = Vec::new();
                    shell.children(&mut shell_children);
                    assert!(shell_children.is_empty(), "{name}: shell holds a reference");
                }
                // Only a holder-local shape refuses a shell.
                None => assert_eq!(
                    object.shape().boundary,
                    BoundaryPolicy::HolderLocal,
                    "{name}: sendable shape without a shell"
                ),
            }
        }
    }

    /// The witness of one object, or `None` for a shape that holds
    /// none.
    fn witness_of(object: &Object) -> Option<lm_value::TypeEnvId> {
        match object {
            Object::Instance { env, .. } | Object::Closure { env, .. } => Some(env.env()),
            _ => None,
        }
    }

    /// A copy preserves the witness.
    ///
    /// The two copy paths reconstruct an object: `shell` builds the
    /// destination and `remap` patches the children. A closure keeps
    /// the environment of the frame that created it across a machine
    /// boundary, so both paths carry the witness through.
    #[test]
    fn the_two_copy_paths_preserve_the_witness() {
        for object in sample_objects() {
            let Some(want) = witness_of(&object) else {
                continue;
            };
            assert_ne!(want, lm_value::TypeEnvId::EMPTY, "the sample is generic");
            let shell = object.shell().expect("a sendable shape has a shell");
            assert_eq!(witness_of(&shell), Some(want), "shell");
            let mapped = object
                .remap(|r| r)
                .expect("a shape with references rebuilds");
            assert_eq!(witness_of(&mapped), Some(want), "remap");
        }
    }

    /// A witness is provenance, so it never decides value equality.
    #[test]
    fn two_objects_with_other_witnesses_stay_equal() {
        let one = Object::Closure {
            func: 1,
            captures: vec![Value::Int(3)].into(),
            env: Witness(lm_value::TypeEnvId(1)),
        };
        let other = Object::Closure {
            func: 1,
            captures: vec![Value::Int(3)].into(),
            env: Witness(lm_value::TypeEnvId(2)),
        };
        assert_eq!(one, other);
    }

    /// Every shape has one sample, so the walk agreement covers the
    /// whole table.
    #[test]
    fn the_samples_cover_every_shape() {
        let tags: Vec<u8> = sample_objects().iter().map(Object::tag).collect();
        assert_eq!(tags, (0..SHAPES.len() as u8).collect::<Vec<u8>>());
    }

    #[test]
    fn every_shape_tag_resolves_to_its_own_descriptor() {
        let objects = [
            Object::Str(SharedText::new()),
            Object::Instance {
                class: 0,
                fields: vec![].into(),
                env: Witness::EMPTY,
            },
            Object::List {
                items: vec![].into(),
                epoch: Default::default(),
            },
            Object::Map {
                entries: vec![],
                index: MapIndex::default(),
            },
            Object::Tuple {
                items: vec![].into(),
            },
            Object::Closure {
                func: 0,
                captures: vec![].into(),
                env: Witness::EMPTY,
            },
            Object::StrBuilder(NativeStringBuilder::new()),
            Object::ByteBuf(NativeByteBuffer::new()),
            Object::NativeVm {
                image: 0,
                generation: 0,
            },
            Object::NativeTable { vm: 0 },
            Object::NativeRequest { vm: 0, ordinal: 0 },
            Object::NativeCall {
                vm: 0,
                ordinal: 0,
                op: 0,
            },
            Object::NativeFault {
                code: FaultCode::HostFault,
                message: String::new(),
                op: None,
                trace: Box::default(),
            },
            Object::NativeDigest([0; 32]),
            Object::NativeHandle {
                proc: 0,
                generation: 0,
            },
            Object::NativeSnapshot(std::sync::Arc::new(Vec::new())),
            Object::Bytes(SharedBytes::new()),
            Object::NativeFileHandle { resource: 0 },
            Object::NativeResourceHandle {
                surface: 0,
                resource: 0,
            },
            Object::NativeWait { owner: 0, token: 0 },
            Object::Substring(SharedText::new()),
            Object::NativeTcpStream { resource: 0 },
            Object::NativeTcpListener { resource: 0 },
            Object::NativeTlsStream { resource: 0 },
            Object::NativeSnapshotRef { image: 0 },
            Object::NativeRun { vm: 0 },
            Object::NativeCode(Box::new(PortableCode {
                kind: PortableCodeKind::Artifact,
                bytes: SharedBytes::new(),
                slot: None,
                origin: None,
            })),
            Object::NativeCodeHandle {
                image: 0,
                generation: 0,
                instance: 0,
                kind: CodeHandleKind::Instance,
                index: 0,
            },
            Object::DynValue {
                value: Value::Unit,
                ty: 0,
            },
            Object::NativeSlotChange {
                image: 0,
                generation: 0,
                slot: 0,
                version: 0,
                kind: SlotChangeKind::Value,
                target: Value::Unit,
            },
            Object::NativeHostResource {
                kind: [0; 32],
                resource: 0,
            },
            Object::NativeRawMode { resource: 0 },
            Object::NativeSignalStream { resource: 0 },
            Object::NativePipeReader { resource: 0 },
            Object::NativePipeWriter { resource: 0 },
            Object::NativeChild { resource: 0 },
            Object::NativeUdpSocket { resource: 0 },
            Object::NativeDynRef {
                vm: 0,
                generation: 0,
            },
        ];
        assert_eq!(objects.len(), SHAPES.len());
        for (tag, object) in objects.iter().enumerate() {
            assert_eq!(object.tag() as usize, tag);
            assert_eq!(object.shape(), SHAPES[tag]);
        }
    }

    /// A shape without references declares no child order, and a
    /// shape with references names one.
    #[test]
    fn child_order_agrees_with_the_reference_flag() {
        for shape in SHAPES {
            if shape.has_refs {
                assert_ne!(shape.child_order, "none", "{}", shape.name);
            } else {
                assert_eq!(shape.child_order, "none", "{}", shape.name);
            }
        }
    }

    /// Exactly the sendable shapes have a shell.
    ///
    /// The copy runs its shape check first and builds a shell after
    /// it, so the two sets must agree. A sendable shape without a
    /// shell would pass the check and then fail the copy.
    #[test]
    fn a_shape_has_a_shell_exactly_when_it_is_sendable() {
        for object in sample_objects() {
            let sendable = object.shape().boundary == BoundaryPolicy::Sendable;
            assert_eq!(
                object.shell().is_some(),
                sendable,
                "{}",
                object.shape().name
            );
        }
    }

    /// A holder-local shape is never digestible: a digest that named
    /// it could not be reproduced in another heap.
    #[test]
    fn holder_local_shapes_are_not_digestible() {
        for shape in SHAPES {
            if shape.boundary == BoundaryPolicy::HolderLocal {
                assert!(!shape.digestible, "{}", shape.name);
            }
        }
    }

    /// Native shapes contain no host state.
    ///
    /// A live file entry stays outside the heap. Snapshot preflight
    /// checks the entry before it writes a closed handle marker.
    #[test]
    fn native_shapes_contain_only_machine_state() {
        for shape in SHAPES {
            assert_eq!(
                shape.snapshot,
                SnapshotClass::MachineState,
                "{}",
                shape.name
            );
        }
    }

    #[test]
    fn map_children_visit_key_before_value() {
        let key = ObjRef {
            slot: 1,
            generation: 0,
        };
        let value = ObjRef {
            slot: 2,
            generation: 0,
        };
        let map = Object::Map {
            entries: vec![MapEntry {
                key: Value::Obj(key),
                value: Value::Obj(value),
                semantic_hash: 1,
            }],
            index: MapIndex::with_live(StructuralEpoch::default(), 1),
        };
        let mut out = Vec::new();
        map.children(&mut out);
        assert_eq!(out, vec![key, value]);
    }

    /// The boundary column of every shape, by name.
    ///
    /// A holder-local shape never crosses a machine boundary, so the
    /// second list is the whole set of values a message may not hold.
    /// A boundary crossing copies every other shape.
    ///
    /// `Snapshot` joins the sendable column. Specification 16.2 lists
    /// snapshots among the sendable values, and specification 16.4
    /// calls a snapshot machine state with bytes the codec can copy.
    ///
    /// Immutable bytes and resource handles are sendable. A resource
    /// control stays with its holder.
    #[test]
    fn every_shape_declares_its_boundary_column() {
        let mut sendable: Vec<&str> = Vec::new();
        let mut holder_local: Vec<&str> = Vec::new();
        for shape in SHAPES {
            match shape.boundary {
                BoundaryPolicy::Sendable => sendable.push(shape.name),
                BoundaryPolicy::HolderLocal => holder_local.push(shape.name),
            }
        }
        assert_eq!(
            sendable,
            vec![
                "String",
                "Instance",
                "List",
                "Map",
                "Tuple",
                "Closure",
                "Fault",
                "Digest",
                "Handle",
                "Snapshot",
                "Bytes",
                "FileHandle",
                "Substring",
                "TcpStream",
                "TcpListener",
                "TlsStream",
                "SnapshotRef",
                "PortableCode",
                "DynValue",
                "RawMode",
                "SignalStream",
                "PipeReader",
                "PipeWriter",
                "Child",
                "UdpSocket",
            ]
        );
        // A builder holds a private mutable buffer.
        // Control designators stay with their holder.
        assert_eq!(
            holder_local,
            vec![
                "StringBuilder",
                "ByteBuffer",
                "Vm",
                "PolicyTable",
                "Request",
                "PendingCall",
                "ResourceHandle",
                "Wait",
                "Run",
                "CodeHandle",
                "SlotChange",
                "HostResource",
                "DynRef",
            ]
        );
    }

    #[test]
    fn the_shape_dump_lists_every_shape() {
        let dump = dump_shapes();
        assert_eq!(dump.lines().count(), SHAPES.len());
        assert!(dump.contains("3 Map refs=true"), "{dump}");
        // A proc handle is a sendable designator of one machine, so
        // send rights travel as data and no digest can name it.
        assert!(
            dump.contains(
                "14 Handle refs=false born_frozen=true boundary=sendable digestible=false"
            ),
            "{dump}"
        );
    }
}
