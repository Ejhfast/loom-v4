//! The boundary type check.
//!
//! A value crosses a VM boundary when it enters typed guest code: a
//! terminal result read, a mailbox receive, a pending call reply, a
//! spawn argument, a mock reply, and a restore that answers a handle.
//! The receiving code expects one type there, and this module proves
//! that the value carries it.
//!
//! Two inputs decide the expected type, and neither comes from a
//! snapshot container:
//!
//! - the instruction states a module type index. The verifier proves
//!   that the index equals the type its dataflow pushes, so the index
//!   comes from verified code;
//! - the frame states a `TypeEnvId`. `call_generic` builds it from the
//!   caller environment and the application of the call site, so a
//!   live generic frame binds its variables to closed types that
//!   execution built.
//!
//! A restored frame states its own environment, so the check there
//! compares against a type the container chose. That is expected. The
//! check has teeth where the performing frame is live, which is the
//! machine that called restore and holds its own static type.
//!
//! The walk is iterative and bounded. It descends every element and
//! every field, so a wrong type inside a container faults at the
//! boundary instead of later.
//!
//! The walk reads no witness. An instance takes the arguments of the
//! position that reached it, and the field types follow from the
//! layout of its concrete class under those arguments. A wrong
//! argument therefore shows as a wrong field value. An object that
//! carries no value under the argument, for example an empty list,
//! stays a question the interpreter answers: every read tests its own
//! tag.
//!
//! The walk allocates nothing after its first call, and it performs no
//! hashing in the common shapes.

use lm_abi::FaultCode;
use lm_bytecode::closed::{ClosedType, ClosedTypeId, TypeEnvs};
use lm_bytecode::{BcType, Module};
use lm_heap::{Heap, Object};
use lm_value::{TypeEnvId, Value};
use std::collections::HashSet;

/// The largest number of positions one boundary check may visit.
///
/// A graph past the bound takes `BoundaryLimit`, exactly as a copy
/// past its graph limit does.
const MAX_STEPS: u32 = 1 << 20;

/// The number of visited positions the linear table holds.
///
/// A boundary value is usually one scalar or one small enum, so the
/// linear scan answers without a hash. A wide graph spills into the
/// set.
const SEEN_LINEAR: usize = 32;

/// One expected type of the walk.
///
/// `Open` names a module type under one argument list of the arena.
/// `Closed` names an entry of the closed type table of the world. The
/// walk starts open, and it turns closed where a type variable needs
/// the substitution an environment holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Expect {
    Open(u32, u32),
    Closed(ClosedTypeId),
}

/// The reusable state of one boundary check.
///
/// The world owns one of these. Each check clears it, so a repeated
/// check reuses the same buffers.
#[derive(Debug, Default)]
pub(crate) struct BoundaryScratch {
    /// The argument lists the walk built. A list holds the expected
    /// type of each parameter one position binds.
    args: Vec<Vec<Expect>>,
    /// The number of argument lists this check filled. The vectors
    /// past that mark keep their storage for the next check.
    args_len: usize,
    /// One buffer for the argument list under construction.
    tmp: Vec<Expect>,
    work: Vec<(Value, Expect)>,
    seen: Vec<(u32, Expect)>,
    seen_set: HashSet<(u32, Expect)>,
}

impl BoundaryScratch {
    fn reset(&mut self) {
        self.args_len = 0;
        self.work.clear();
        self.seen.clear();
        self.seen_set.clear();
    }

    /// Record one visited position. The answer is true at the first
    /// visit.
    fn mark(&mut self, key: (u32, Expect)) -> bool {
        if self.seen_set.is_empty() {
            if self.seen.len() < SEEN_LINEAR {
                if self.seen.contains(&key) {
                    return false;
                }
                self.seen.push(key);
                return true;
            }
            self.seen_set.extend(self.seen.iter().copied());
        }
        self.seen_set.insert(key)
    }

    /// Record one argument list and answer its arena index.
    ///
    /// The arena holds no duplicate, so one argument list has one
    /// index inside one check and the visited key stays canonical.
    fn args_of(&mut self, list: &[Expect]) -> u32 {
        for (at, held) in self.args[..self.args_len].iter().enumerate() {
            if held.as_slice() == list {
                return at as u32;
            }
        }
        if self.args_len == self.args.len() {
            self.args.push(Vec::new());
        }
        let slot = &mut self.args[self.args_len];
        slot.clear();
        slot.extend_from_slice(list);
        self.args_len += 1;
        (self.args_len - 1) as u32
    }

    /// Record the closed argument list of one environment.
    fn args_of_closed(&mut self, list: &[ClosedTypeId]) -> u32 {
        let mut tmp = std::mem::take(&mut self.tmp);
        tmp.clear();
        tmp.extend(list.iter().map(|id| Expect::Closed(*id)));
        let at = self.args_of(&tmp);
        self.tmp = tmp;
        at
    }

    fn args_at(&self, id: u32) -> &[Expect] {
        match self.args.get(id as usize) {
            Some(list) => list.as_slice(),
            None => &[],
        }
    }
}

/// Prove that one value carries the type the receiving code expects.
///
/// `reply_ty` is the module type index the instruction states, and
/// `env` is the type environment of the frame that runs it.
pub(crate) fn check_boundary_value(
    module: &Module,
    heap: &Heap,
    envs: &TypeEnvs,
    scratch: &mut BoundaryScratch,
    value: Value,
    reply_ty: u32,
    env: TypeEnvId,
) -> Result<(), FaultCode> {
    // A scalar value at a scalar position needs no walk, and no
    // substitution can turn a primitive into another type. The two
    // most common replies, the unit of an `Io` operation and the
    // integer of a clock read, take this answer.
    match (value, module.types.get(reply_ty as usize)) {
        (Value::Unit, Some(BcType::Unit))
        | (Value::Int(_), Some(BcType::Int))
        | (Value::Bool(_), Some(BcType::Bool)) => return Ok(()),
        _ => {}
    }
    scratch.reset();
    // A monomorphic frame binds no variable, so the root argument list
    // is empty.
    let root = match envs.env(env) {
        Some(entry) => scratch.args_of_closed(&entry.types),
        None => scratch.args_of(&[]),
    };
    scratch.work.push((value, Expect::Open(reply_ty, root)));
    let mut steps = 0u32;
    while let Some((value, expect)) = scratch.work.pop() {
        steps += 1;
        if steps > MAX_STEPS {
            return Err(FaultCode::BoundaryLimit);
        }
        check_one(module, heap, envs, scratch, value, expect)?;
    }
    Ok(())
}

/// The scalar kinds of the expected type.
#[derive(Clone, Copy)]
enum Scalar {
    Unit,
    Bool,
    Int,
    Op(u32),
}

/// The heap shapes an expected type names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Str,
    StringBuilder,
    ByteBuffer,
    Fault,
    Request,
    PolicyTable,
    EmptyVm,
    Digest,
    Snapshot,
    Vm,
    PendingCall,
    Handle,
    Closure,
    List,
    Map,
    Tuple,
    Instance,
}

/// The resolved shape of one expected type.
enum Node {
    Scalar(Scalar),
    Heap(Kind),
}

/// Check one `(value, expected type)` pair and push its children.
fn check_one(
    module: &Module,
    heap: &Heap,
    envs: &TypeEnvs,
    scratch: &mut BoundaryScratch,
    value: Value,
    expect: Expect,
) -> Result<(), FaultCode> {
    match resolve(module, envs, expect)? {
        Node::Scalar(tag) => {
            let ok = match (value, tag) {
                (Value::Unit, Scalar::Unit) => true,
                (Value::Bool(_), Scalar::Bool) => true,
                (Value::Int(_), Scalar::Int) => true,
                (Value::Op(slot), Scalar::Op(op)) => slot == op,
                _ => false,
            };
            if ok {
                Ok(())
            } else {
                Err(FaultCode::TypeMismatch)
            }
        }
        Node::Heap(kind) => {
            let Value::Obj(r) = value else {
                return Err(FaultCode::TypeMismatch);
            };
            let object = heap.get(r);
            let found = kind_of(object);
            // An `EmptyVm` position and a `Vm[T]` position both name a
            // machine handle. The lifecycle rule of `EmptyVm` lives in
            // the kernel.
            if found != kind && !(found == Kind::Vm && kind == Kind::EmptyVm) {
                return Err(FaultCode::TypeMismatch);
            }
            // Four shapes carry a rule past their tag. Every other
            // shape ends the check here.
            let children = match object {
                Object::List { items } | Object::Tuple { items } => items.len(),
                Object::Map { entries, .. } => entries.len(),
                Object::Instance { fields, .. } => fields.len(),
                _ => return Ok(()),
            };
            // A graph may hold a cycle and may share one object at
            // several positions. The visited key carries the type, so
            // one object is proved under every type that reaches it,
            // and the walk still terminates. An object with no child
            // sits on no cycle, so it needs no entry.
            if children > 0 && !scratch.mark((r.slot, expect)) {
                return Ok(());
            }
            check_object(module, envs, scratch, object, kind, expect)
        }
    }
}

/// Resolve one expected type to its shape.
fn resolve(module: &Module, envs: &TypeEnvs, expect: Expect) -> Result<Node, FaultCode> {
    match expect {
        Expect::Open(ty, _) => {
            let node = module
                .types
                .get(ty as usize)
                .ok_or(FaultCode::MalformedState)?;
            Ok(match node {
                BcType::Unit => Node::Scalar(Scalar::Unit),
                BcType::Bool => Node::Scalar(Scalar::Bool),
                BcType::Int => Node::Scalar(Scalar::Int),
                BcType::Op(op, _) => Node::Scalar(Scalar::Op(*op)),
                BcType::Str => Node::Heap(Kind::Str),
                BcType::StringBuilder => Node::Heap(Kind::StringBuilder),
                BcType::ByteBuffer => Node::Heap(Kind::ByteBuffer),
                BcType::Fault => Node::Heap(Kind::Fault),
                BcType::Request => Node::Heap(Kind::Request),
                BcType::PolicyTable => Node::Heap(Kind::PolicyTable),
                BcType::EmptyVm => Node::Heap(Kind::EmptyVm),
                BcType::Digest => Node::Heap(Kind::Digest),
                BcType::SnapshotImage | BcType::Snapshot(_) => Node::Heap(Kind::Snapshot),
                BcType::Vm(_) => Node::Heap(Kind::Vm),
                BcType::PendingCall(_, _) => Node::Heap(Kind::PendingCall),
                BcType::Handle(_, _) => Node::Heap(Kind::Handle),
                BcType::Fn(_, _, _, _) => Node::Heap(Kind::Closure),
                BcType::List(_) => Node::Heap(Kind::List),
                BcType::Map(_, _) => Node::Heap(Kind::Map),
                BcType::Tuple(_) => Node::Heap(Kind::Tuple),
                BcType::Class(_) | BcType::Inst(_, _) => Node::Heap(Kind::Instance),
                // `bind` resolves a variable before it reaches the
                // work list, so a variable here names a position the
                // argument list does not answer.
                BcType::Var(_) => return Err(FaultCode::TypeMismatch),
            })
        }
        Expect::Closed(id) => {
            let node = envs.ty(id).ok_or(FaultCode::MalformedState)?;
            Ok(match node {
                ClosedType::Unit => Node::Scalar(Scalar::Unit),
                ClosedType::Bool => Node::Scalar(Scalar::Bool),
                ClosedType::Int => Node::Scalar(Scalar::Int),
                ClosedType::Op(op, _) => Node::Scalar(Scalar::Op(*op)),
                ClosedType::Str => Node::Heap(Kind::Str),
                ClosedType::StringBuilder => Node::Heap(Kind::StringBuilder),
                ClosedType::ByteBuffer => Node::Heap(Kind::ByteBuffer),
                ClosedType::Fault => Node::Heap(Kind::Fault),
                ClosedType::Request => Node::Heap(Kind::Request),
                ClosedType::PolicyTable => Node::Heap(Kind::PolicyTable),
                ClosedType::EmptyVm => Node::Heap(Kind::EmptyVm),
                ClosedType::Digest => Node::Heap(Kind::Digest),
                ClosedType::SnapshotImage | ClosedType::Snapshot(_) => Node::Heap(Kind::Snapshot),
                ClosedType::Vm(_) => Node::Heap(Kind::Vm),
                ClosedType::PendingCall(_, _) => Node::Heap(Kind::PendingCall),
                ClosedType::Handle(_, _) => Node::Heap(Kind::Handle),
                ClosedType::Fn(_, _, _, _) => Node::Heap(Kind::Closure),
                ClosedType::List(_) => Node::Heap(Kind::List),
                ClosedType::Map(_, _) => Node::Heap(Kind::Map),
                ClosedType::Tuple(_) => Node::Heap(Kind::Tuple),
                ClosedType::Class(_) | ClosedType::Inst(_, _) => Node::Heap(Kind::Instance),
            })
        }
    }
}

/// The shape of one heap object.
fn kind_of(object: &Object) -> Kind {
    match object {
        Object::Str(_) => Kind::Str,
        Object::StrBuilder(_) => Kind::StringBuilder,
        Object::ByteBuf(_) => Kind::ByteBuffer,
        Object::NativeFault { .. } => Kind::Fault,
        Object::NativeRequest { .. } => Kind::Request,
        Object::NativeTable { .. } => Kind::PolicyTable,
        Object::NativeVm { .. } => Kind::Vm,
        Object::NativeDigest(_) => Kind::Digest,
        Object::NativeSnapshot(_) => Kind::Snapshot,
        Object::NativeCall { .. } => Kind::PendingCall,
        Object::NativeHandle { .. } => Kind::Handle,
        Object::Closure { .. } => Kind::Closure,
        Object::List { .. } => Kind::List,
        Object::Map { .. } => Kind::Map,
        Object::Tuple { .. } => Kind::Tuple,
        Object::Instance { .. } => Kind::Instance,
    }
}

/// Prove one object against one expected type and push its children.
///
/// A machine handle, a proc handle, a call token, a snapshot, and a
/// closure meet a shape test alone. Their type arguments name another
/// machine, another operation, or a function body, and a container
/// states all three. The value each one produces meets its own
/// boundary check at its own read, and that check is the one with
/// teeth.
fn check_object(
    module: &Module,
    envs: &TypeEnvs,
    scratch: &mut BoundaryScratch,
    object: &Object,
    kind: Kind,
    expect: Expect,
) -> Result<(), FaultCode> {
    match (object, kind) {
        (Object::List { items }, Kind::List) => {
            let elem = child(module, envs, scratch, expect, 0)?;
            for item in items {
                scratch.work.push((*item, elem));
            }
            Ok(())
        }
        (Object::Map { entries, .. }, Kind::Map) => {
            let key = child(module, envs, scratch, expect, 0)?;
            let val = child(module, envs, scratch, expect, 1)?;
            for (k, v) in entries {
                scratch.work.push((*k, key));
                scratch.work.push((*v, val));
            }
            Ok(())
        }
        (Object::Tuple { items }, Kind::Tuple) => {
            let arity = arity_of(module, envs, expect)?;
            if items.len() != arity {
                return Err(FaultCode::TypeMismatch);
            }
            for (at, item) in items.iter().enumerate() {
                let elem = child(module, envs, scratch, expect, at)?;
                scratch.work.push((*item, elem));
            }
            Ok(())
        }
        (Object::Instance { class, fields, .. }, Kind::Instance) => {
            check_instance(module, envs, scratch, *class, fields, expect)
        }
        _ => Ok(()),
    }
}

/// The child expected type at one position of a container type.
fn child(
    module: &Module,
    envs: &TypeEnvs,
    scratch: &mut BoundaryScratch,
    expect: Expect,
    at: usize,
) -> Result<Expect, FaultCode> {
    match expect {
        Expect::Open(ty, args) => {
            let node = module
                .types
                .get(ty as usize)
                .ok_or(FaultCode::MalformedState)?;
            let child = match (node, at) {
                (BcType::List(e), 0) => *e,
                (BcType::Map(k, _), 0) => *k,
                (BcType::Map(_, v), 1) => *v,
                (BcType::Tuple(elems), _) => *elems.get(at).ok_or(FaultCode::TypeMismatch)?,
                _ => return Err(FaultCode::MalformedState),
            };
            bind(module, scratch.args_at(args), child, args)
        }
        Expect::Closed(id) => {
            let node = envs.ty(id).ok_or(FaultCode::MalformedState)?;
            let child = match (node, at) {
                (ClosedType::List(e), 0) => *e,
                (ClosedType::Map(k, _), 0) => *k,
                (ClosedType::Map(_, v), 1) => *v,
                (ClosedType::Tuple(elems), _) => *elems.get(at).ok_or(FaultCode::TypeMismatch)?,
                _ => return Err(FaultCode::MalformedState),
            };
            Ok(Expect::Closed(child))
        }
    }
}

/// One module type under one argument list.
///
/// A type variable resolves to the expected type the list binds, so no
/// free variable ever reaches the work list. A variable the list does
/// not answer keeps its open form, and `resolve` faults on it.
fn bind(module: &Module, args: &[Expect], ty: u32, args_id: u32) -> Result<Expect, FaultCode> {
    let node = module
        .types
        .get(ty as usize)
        .ok_or(FaultCode::MalformedState)?;
    match node {
        BcType::Var(index) => args
            .get(*index as usize)
            .copied()
            .ok_or(FaultCode::TypeMismatch),
        _ => Ok(Expect::Open(ty, args_id)),
    }
}

/// The element count of one tuple type.
fn arity_of(module: &Module, envs: &TypeEnvs, expect: Expect) -> Result<usize, FaultCode> {
    match expect {
        Expect::Open(ty, _) => match module.types.get(ty as usize) {
            Some(BcType::Tuple(elems)) => Ok(elems.len()),
            _ => Err(FaultCode::MalformedState),
        },
        Expect::Closed(id) => match envs.ty(id) {
            Some(ClosedType::Tuple(elems)) => Ok(elems.len()),
            _ => Err(FaultCode::MalformedState),
        },
    }
}

/// True when `child` equals `ancestor` or inherits it.
///
/// The walk carries a step bound, so a hand-built class table cannot
/// make it spin.
fn class_extends(module: &Module, mut child: u32, ancestor: u32) -> bool {
    for _ in 0..=module.classes.len() {
        if child == ancestor {
            return true;
        }
        match module.classes.get(child as usize).and_then(|c| c.parent()) {
            Some(parent) => child = parent,
            None => return false,
        }
    }
    false
}

/// Prove one instance against one class position.
///
/// The concrete class of the object must equal or extend the class of
/// the position. The field types then follow from the layout of that
/// concrete class under the arguments of the position.
///
/// A class with type parameters declares no parent of its own, and an
/// enum case passes the arguments of its family through, so the
/// arguments of the position bind the parameters of the concrete class
/// wherever the two arities agree. A layout that binds more parameters
/// than the position answers keeps its fields unchecked, and every
/// read of one meets the interpreter tag test.
fn check_instance(
    module: &Module,
    envs: &TypeEnvs,
    scratch: &mut BoundaryScratch,
    class: u32,
    fields: &[Value],
    expect: Expect,
) -> Result<(), FaultCode> {
    let layout = module
        .classes
        .get(class as usize)
        .ok_or(FaultCode::TypeMismatch)?;
    if fields.len() != layout.fields.len() {
        return Err(FaultCode::TypeMismatch);
    }
    let mut want = std::mem::take(&mut scratch.tmp);
    want.clear();
    let found = position_class(module, envs, scratch, expect, &mut want);
    let (want_class, arity_ok) = match found {
        Ok(class_of_position) => {
            let arity_ok = want.len() == layout.type_params as usize;
            (class_of_position, arity_ok)
        }
        Err(code) => {
            scratch.tmp = want;
            return Err(code);
        }
    };
    if !class_extends(module, class, want_class) {
        scratch.tmp = want;
        return Err(FaultCode::TypeMismatch);
    }
    if !arity_ok {
        scratch.tmp = want;
        return Ok(());
    }
    let args = scratch.args_of(&want);
    scratch.tmp = want;
    for (at, value) in fields.iter().enumerate() {
        // A field before its first assignment holds the uninitialized
        // marker. The instruction that reads one faults, so the
        // boundary leaves it alone.
        if *value == Value::Uninit {
            continue;
        }
        let expect = bind(module, scratch.args_at(args), layout.fields[at].1, args)?;
        scratch.work.push((*value, expect));
    }
    Ok(())
}

/// The class of one instance position, with its argument list written
/// into `out`.
fn position_class(
    module: &Module,
    envs: &TypeEnvs,
    scratch: &BoundaryScratch,
    expect: Expect,
    out: &mut Vec<Expect>,
) -> Result<u32, FaultCode> {
    match expect {
        Expect::Open(ty, args) => match module.types.get(ty as usize) {
            Some(BcType::Class(class)) => Ok(*class),
            Some(BcType::Inst(class, list)) => {
                let bound = scratch.args_at(args);
                for item in list {
                    out.push(bind(module, bound, *item, args)?);
                }
                Ok(*class)
            }
            _ => Err(FaultCode::MalformedState),
        },
        Expect::Closed(id) => match envs.ty(id) {
            Some(ClosedType::Class(class)) => Ok(*class),
            Some(ClosedType::Inst(class, list)) => {
                out.extend(list.iter().map(|arg| Expect::Closed(*arg)));
                Ok(*class)
            }
            _ => Err(FaultCode::MalformedState),
        },
    }
}
