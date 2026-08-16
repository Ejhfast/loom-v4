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
use lm_value::{ObjRef, Value};

/// Logical byte cost of one object header.
pub(crate) const HEADER_COST: usize = 32;
/// Logical byte cost of one stored value.
pub(crate) const VALUE_COST: usize = 16;
/// Logical byte cost of one map entry (key and value).
pub(crate) const ENTRY_COST: usize = 2 * VALUE_COST;

/// A derived lookup index for one map: key hash to entry indices.
///
/// The index is a cache over the insertion-ordered entries: `built`
/// counts the indexed prefix, and lookups extend it on demand.
/// Iteration, display, equality, and digest semantics never read it.
/// It holds no object references, so every graph mode skips it by
/// design, and the logical entry cost covers it: the index grows by
/// one bounded bucket entry per map entry.
#[derive(Debug, Clone, Default)]
pub struct MapIndex {
    /// The number of entries the table already indexes.
    pub built: usize,
    /// Key hash to the entry indices with that hash.
    pub table: std::collections::HashMap<u64, Vec<u32>>,
}

impl PartialEq for MapIndex {
    /// The index is derived data, so it never takes part in object
    /// equality.
    fn eq(&self, _: &MapIndex) -> bool {
        true
    }
}

/// The payload of one heap object.
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    /// Immutable UTF-8 text. Born frozen.
    Str(String),
    /// A class instance. Fields follow the class layout. A field holds
    /// `Value::Uninit` before its first assignment.
    Instance { class: u32, fields: Vec<Value> },
    /// A growable list.
    List { items: Vec<Value> },
    /// A map with entries in insertion order plus a derived lookup
    /// index.
    Map {
        entries: Vec<(Value, Value)>,
        index: MapIndex,
    },
    /// A fixed-arity immutable tuple. Born frozen.
    Tuple { items: Vec<Value> },
    /// A closure: code index plus captured values. Born frozen.
    Closure { func: u32, captures: Vec<Value> },
    /// A string builder.
    StrBuilder(String),
    /// A byte buffer.
    ByteBuf(Vec<u8>),
    /// A holder-local handle to one machine in the world registry.
    /// The static type separates `EmptyVm` and `Vm[T]` views.
    NativeVm { vm: u32 },
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
    },
    /// A frozen canonical graph digest. Born frozen.
    NativeDigest([u8; 32]),
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

/// Every shape descriptor, in shape-tag order. The tag is the index,
/// and the canonical digest encoding reads it.
pub const SHAPES: [&ShapeDesc; 14] = [
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
];

impl Object {
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
        }
    }

    /// The shape descriptor for this object.
    pub fn shape(&self) -> &'static ShapeDesc {
        SHAPES[self.tag() as usize]
    }

    /// The logical byte cost charged against the heap cap.
    pub fn cost(&self) -> usize {
        HEADER_COST
            + match self {
                Object::Str(s) => s.len(),
                Object::Instance { fields, .. } => fields.len() * VALUE_COST,
                Object::List { items } => items.len() * VALUE_COST,
                Object::Map { entries, .. } => entries.len() * ENTRY_COST,
                Object::Tuple { items } => items.len() * VALUE_COST,
                Object::Closure { captures, .. } => captures.len() * VALUE_COST,
                Object::StrBuilder(s) => s.len(),
                Object::ByteBuf(b) => b.len(),
                Object::NativeVm { .. }
                | Object::NativeTable { .. }
                | Object::NativeRequest { .. }
                | Object::NativeCall { .. } => VALUE_COST,
                Object::NativeFault { message, .. } => message.len(),
                Object::NativeDigest(bytes) => bytes.len(),
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
            | Object::NativeTable { .. }
            | Object::NativeRequest { .. }
            | Object::NativeCall { .. }
            | Object::NativeFault { .. }
            | Object::NativeDigest(_) => {}
            Object::Instance { fields, .. } => fields.iter().for_each(&mut visit),
            Object::List { items } | Object::Tuple { items } => items.iter().for_each(&mut visit),
            Object::Map { entries, .. } => {
                // The index holds hashes and positions only, never an
                // object reference, so the walk covers the entries.
                for (k, v) in entries {
                    visit(k);
                    visit(v);
                }
            }
            Object::Closure { captures, .. } => captures.iter().for_each(&mut visit),
        }
    }

    /// The empty destination shell of one sendable object.
    ///
    /// Children are patched afterwards. The shell keeps the payload
    /// sizes, so the cost accounting matches the final object.
    pub fn shell(&self) -> Option<Object> {
        let shell = match self {
            Object::Str(s) => Object::Str(s.clone()),
            Object::NativeFault { code, message, op } => Object::NativeFault {
                code: *code,
                message: message.clone(),
                op: *op,
            },
            Object::NativeDigest(bytes) => Object::NativeDigest(*bytes),
            Object::Tuple { items } => Object::Tuple {
                items: vec![Value::Unit; items.len()],
            },
            Object::List { items } => Object::List {
                items: vec![Value::Unit; items.len()],
            },
            Object::Map { entries, .. } => Object::Map {
                entries: vec![(Value::Unit, Value::Unit); entries.len()],
                index: MapIndex::default(),
            },
            Object::Instance { class, fields } => Object::Instance {
                class: *class,
                fields: vec![Value::Unit; fields.len()],
            },
            Object::Closure { func, captures } => Object::Closure {
                func: *func,
                captures: vec![Value::Unit; captures.len()],
            },
            _ => return None,
        };
        Some(shell)
    }

    /// Rebuild this object with every child reference remapped.
    ///
    /// The order of `map` calls follows the canonical child order, so
    /// a copy reads the same field order the walk reported.
    pub fn remap(&self, mut map: impl FnMut(ObjRef) -> ObjRef) -> Option<Object> {
        let mut value = |v: Value| match v {
            Value::Obj(child) => Value::Obj(map(child)),
            other => other,
        };
        let out = match self {
            Object::Str(_) | Object::NativeFault { .. } | Object::NativeDigest(_) => return None,
            Object::Tuple { items } => Object::Tuple {
                items: items.iter().map(|v| value(*v)).collect(),
            },
            Object::List { items } => Object::List {
                items: items.iter().map(|v| value(*v)).collect(),
            },
            Object::Map { entries, .. } => Object::Map {
                entries: entries
                    .iter()
                    .map(|(k, v)| (value(*k), value(*v)))
                    .collect(),
                // The destination index rebuilds on the first lookup
                // over the copied keys.
                index: MapIndex::default(),
            },
            Object::Instance { class, fields } => Object::Instance {
                class: *class,
                fields: fields.iter().map(|v| value(*v)).collect(),
            },
            Object::Closure { func, captures } => Object::Closure {
                func: *func,
                captures: captures.iter().map(|v| value(*v)).collect(),
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
    fn every_shape_tag_resolves_to_its_own_descriptor() {
        let objects = [
            Object::Str(String::new()),
            Object::Instance {
                class: 0,
                fields: vec![],
            },
            Object::List { items: vec![] },
            Object::Map {
                entries: vec![],
                index: MapIndex::default(),
            },
            Object::Tuple { items: vec![] },
            Object::Closure {
                func: 0,
                captures: vec![],
            },
            Object::StrBuilder(String::new()),
            Object::ByteBuf(vec![]),
            Object::NativeVm { vm: 0 },
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
            },
            Object::NativeDigest([0; 32]),
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
            entries: vec![(Value::Obj(key), Value::Obj(value))],
            index: MapIndex::default(),
        };
        let mut out = Vec::new();
        map.children(&mut out);
        assert_eq!(out, vec![key, value]);
    }

    #[test]
    fn the_shape_dump_lists_every_shape() {
        let dump = dump_shapes();
        assert_eq!(dump.lines().count(), SHAPES.len());
        assert!(dump.contains("3 Map refs=true"), "{dump}");
    }
}
