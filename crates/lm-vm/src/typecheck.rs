//! Boundary type checks.
//!
//! A value crosses a VM boundary when it enters typed guest code. The
//! receiving instruction supplies the expected module type. Its frame
//! supplies the closed type environment.
//!
//! The checker closes that type once. All later work uses canonical
//! closed types. This rule resolves a root type variable and avoids a
//! second argument-list interner inside the checker.
//!
//! The walk is iterative and bounded. It descends collection elements
//! and instance fields. It also checks a closure signature before the
//! closure can execute in the receiving machine.

use crate::NamespaceRuntime;
use lm_abi::FaultCode;
use lm_bytecode::closed::{ClosedRow, ClosedType, ClosedTypeId, TypeEnvFull, TypeEnvs};
use lm_bytecode::BcType;
#[cfg(test)]
use lm_bytecode::Module;
use lm_heap::{Heap, Object};
use lm_value::{TypeEnvId, Value};
use std::collections::HashSet;

/// The largest number of positions one boundary check may visit.
const MAX_STEPS: u32 = 1 << 20;

/// The number of visited positions the linear table holds.
const SEEN_LINEAR: usize = 32;

/// Reusable state for one boundary check.
#[derive(Debug, Default)]
pub(crate) struct BoundaryScratch {
    work: Vec<(Value, ClosedTypeId)>,
    seen: Vec<(u32, ClosedTypeId)>,
    seen_set: HashSet<(u32, ClosedTypeId)>,
    subtype_work: Vec<(ClosedTypeId, ClosedTypeId)>,
    subtype_seen: HashSet<(ClosedTypeId, ClosedTypeId)>,
}

/// The immutable inputs for one boundary check.
#[derive(Clone, Copy)]
pub(crate) struct BoundaryContext<'a> {
    module: &'a NamespaceRuntime,
    heap: &'a Heap,
}

impl<'a> BoundaryContext<'a> {
    /// Create one boundary-check context.
    pub(crate) fn new(module: &'a NamespaceRuntime, heap: &'a Heap) -> BoundaryContext<'a> {
        BoundaryContext { module, heap }
    }
}

impl BoundaryScratch {
    fn reset(&mut self) {
        self.work.clear();
        self.seen.clear();
        self.seen_set.clear();
        self.subtype_work.clear();
        self.subtype_seen.clear();
    }

    /// Record one visited object and expected type.
    fn mark(&mut self, key: (u32, ClosedTypeId)) -> bool {
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
}

fn env_fault(_: TypeEnvFull) -> FaultCode {
    FaultCode::BoundaryLimit
}

/// Check one value against the closed type of its receiving position.
pub(crate) fn check_boundary_value(
    context: BoundaryContext<'_>,
    envs: &mut TypeEnvs,
    scratch: &mut BoundaryScratch,
    value: Value,
    reply_ty: u32,
    env: TypeEnvId,
) -> Result<(), FaultCode> {
    let BoundaryContext { module, heap } = context;
    if module.types.get(reply_ty as usize).is_none() || envs.env(env).is_none() {
        return Err(FaultCode::MalformedState);
    }

    // These common monomorphic replies need no closed-type node.
    match (value, &module.types[reply_ty as usize]) {
        (Value::Unit, BcType::Unit)
        | (Value::Int(_), BcType::Int)
        | (Value::Float(_), BcType::Float)
        | (Value::Bool(_), BcType::Bool) => return Ok(()),
        _ => {}
    }

    let root = envs.close(module, reply_ty, env).map_err(env_fault)?;
    scratch.reset();
    scratch.work.push((value, root));
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

/// The scalar kinds of an expected type.
#[derive(Clone, Copy)]
enum Scalar {
    Unit,
    Bool,
    Int,
    Float,
    Char,
    Op(u32),
}

/// The heap shapes of an expected type.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Str,
    Text,
    Substring,
    StringBuilder,
    ByteBuffer,
    Bytes,
    FileHandle,
    ResourceHandle,
    HostResource,
    Wait,
    Fault,
    Request,
    PolicyTable,
    Digest,
    Snapshot,
    Vm,
    Run,
    PendingCall,
    Handle,
    Closure,
    List,
    Map,
    Tuple,
    Instance,
    TcpResource,
    TcpStream,
    TcpListener,
    TlsStream,
    RawMode,
    SignalStream,
    PipeEnd,
    PipeReader,
    PipeWriter,
    Child,
    UdpSocket,
    Artifact,
    VerifiedModule,
    FunctionCode,
    ClassCode,
    SlotSpec,
    CodeInstance,
    Slot,
    FunctionDef,
    ClassDef,
    FunctionBinding,
    ClassBinding,
    SlotChange,
    DynValue,
}

enum Node {
    Scalar(Scalar),
    Heap(Kind),
    Callback,
    Option {
        payload: ClosedTypeId,
        case: Option<bool>,
    },
}

/// Check one value and expected type pair.
fn check_one(
    module: &NamespaceRuntime,
    heap: &Heap,
    envs: &mut TypeEnvs,
    scratch: &mut BoundaryScratch,
    value: Value,
    expect: ClosedTypeId,
) -> Result<(), FaultCode> {
    match resolve(module, envs, expect)? {
        Node::Scalar(tag) => {
            let matches = match (value, tag) {
                (Value::Unit, Scalar::Unit) => true,
                (Value::Bool(_), Scalar::Bool) => true,
                (Value::Int(_), Scalar::Int) => true,
                (Value::Float(_), Scalar::Float) => true,
                (Value::Char(_), Scalar::Char) => true,
                (Value::Op(slot), Scalar::Op(op)) => slot == op,
                _ => false,
            };
            matches.then_some(()).ok_or(FaultCode::TypeMismatch)
        }
        Node::Heap(kind) => {
            let Value::Obj(reference) = value else {
                return Err(FaultCode::TypeMismatch);
            };
            let object = heap.get(reference);
            let found = kind_of(object);
            let text_match = kind == Kind::Text && matches!(found, Kind::Str | Kind::Substring);
            let tcp_match =
                kind == Kind::TcpResource && matches!(found, Kind::TcpStream | Kind::TcpListener);
            let pipe_match =
                kind == Kind::PipeEnd && matches!(found, Kind::PipeReader | Kind::PipeWriter);
            if !(found == kind || text_match || tcp_match || pipe_match) {
                return Err(FaultCode::TypeMismatch);
            }

            let children = match object {
                Object::List { items, .. } | Object::Tuple { items } => items.len(),
                Object::Map { index, .. } => index.live_len(),
                Object::Instance { fields, .. } => fields.len(),
                Object::DynValue { .. } => 1,
                _ => 0,
            };
            if children > 0 && !scratch.mark((reference.slot, expect)) {
                return Ok(());
            }
            check_object(module, envs, scratch, object, kind, expect)
        }
        Node::Callback => Err(FaultCode::BoundaryViolation),
        Node::Option { payload, case } => {
            let family = match envs.ty(expect).cloned() {
                Some(ClosedType::Inst(_, args)) => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_OPTION];
                    envs.intern(ClosedType::Inst(class, args))
                        .map_err(env_fault)?
                }
                _ => return Err(FaultCode::MalformedState),
            };
            let is_none = matches!(
                value,
                Value::EmptyCase { ty, arm: 1 } if ty == family
            );
            match case {
                Some(false) if !is_none => Err(FaultCode::TypeMismatch),
                Some(true) if is_none => Err(FaultCode::TypeMismatch),
                None | Some(false) if is_none => Ok(()),
                None | Some(true) => {
                    scratch.work.push((value, payload));
                    Ok(())
                }
                Some(false) => unreachable!(),
            }
        }
    }
}

fn resolve(
    module: &NamespaceRuntime,
    envs: &TypeEnvs,
    expect: ClosedTypeId,
) -> Result<Node, FaultCode> {
    let node = envs.ty(expect).ok_or(FaultCode::MalformedState)?;
    Ok(match node {
        ClosedType::Unit => Node::Scalar(Scalar::Unit),
        ClosedType::Bool => Node::Scalar(Scalar::Bool),
        ClosedType::Int => Node::Scalar(Scalar::Int),
        ClosedType::Float => Node::Scalar(Scalar::Float),
        ClosedType::Op(op, _) => Node::Scalar(Scalar::Op(*op)),
        ClosedType::Str => Node::Heap(Kind::Str),
        ClosedType::Bytes => Node::Heap(Kind::Bytes),
        ClosedType::FileHandle => Node::Heap(Kind::FileHandle),
        ClosedType::ResourceHandle => Node::Heap(Kind::ResourceHandle),
        ClosedType::HostResource => Node::Heap(Kind::HostResource),
        ClosedType::Fault => Node::Heap(Kind::Fault),
        ClosedType::Request => Node::Heap(Kind::Request),
        ClosedType::PolicyTable => Node::Heap(Kind::PolicyTable),
        ClosedType::Vm => Node::Heap(Kind::Vm),
        ClosedType::Digest => Node::Heap(Kind::Digest),
        ClosedType::VmSnapshot | ClosedType::RunSnapshot(_) => Node::Heap(Kind::Snapshot),
        ClosedType::Run(_) => Node::Heap(Kind::Run),
        ClosedType::Wait(_) => Node::Heap(Kind::Wait),
        ClosedType::PendingCall(_, _) => Node::Heap(Kind::PendingCall),
        ClosedType::Handle(_, _) => Node::Heap(Kind::Handle),
        ClosedType::Fn(_, _, _, _) => Node::Heap(Kind::Closure),
        ClosedType::Callback(_, _, _, _) => Node::Callback,
        ClosedType::List(_) => Node::Heap(Kind::List),
        ClosedType::Map(_, _) => Node::Heap(Kind::Map),
        ClosedType::Tuple(_) => Node::Heap(Kind::Tuple),
        ClosedType::Class(class) => {
            if module.core_roles[lm_bytecode::corepin::ROLE_STRING_BUILDER] == *class {
                Node::Heap(Kind::StringBuilder)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_BYTE_BUFFER] == *class {
                Node::Heap(Kind::ByteBuffer)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_CHAR] == *class {
                Node::Scalar(Scalar::Char)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_TEXT] == *class {
                Node::Heap(Kind::Text)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_SUBSTRING] == *class {
                Node::Heap(Kind::Substring)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_TCP_RESOURCE] == *class {
                Node::Heap(Kind::TcpResource)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_TCP_STREAM] == *class {
                Node::Heap(Kind::TcpStream)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_TCP_LISTENER] == *class {
                Node::Heap(Kind::TcpListener)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_TLS_STREAM] == *class {
                Node::Heap(Kind::TlsStream)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_RAW_MODE] == *class {
                Node::Heap(Kind::RawMode)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_SIGNAL_STREAM] == *class {
                Node::Heap(Kind::SignalStream)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_PIPE_END] == *class {
                Node::Heap(Kind::PipeEnd)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_PIPE_READER] == *class {
                Node::Heap(Kind::PipeReader)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_PIPE_WRITER] == *class {
                Node::Heap(Kind::PipeWriter)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_CHILD] == *class {
                Node::Heap(Kind::Child)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_UDP_SOCKET] == *class {
                Node::Heap(Kind::UdpSocket)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_ARTIFACT] == *class {
                Node::Heap(Kind::Artifact)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_VERIFIED_MODULE] == *class {
                Node::Heap(Kind::VerifiedModule)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_CLASS_CODE] == *class {
                Node::Heap(Kind::ClassCode)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_SLOT_SPEC] == *class {
                Node::Heap(Kind::SlotSpec)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_INSTANCE] == *class {
                Node::Heap(Kind::CodeInstance)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_SLOT] == *class {
                Node::Heap(Kind::Slot)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_CLASS_DEF] == *class {
                Node::Heap(Kind::ClassDef)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_CLASS_BINDING] == *class {
                Node::Heap(Kind::ClassBinding)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_SLOT_CHANGE] == *class {
                Node::Heap(Kind::SlotChange)
            } else if module.core_roles[lm_bytecode::corepin::ROLE_DYN_VALUE] == *class {
                Node::Heap(Kind::DynValue)
            } else {
                Node::Heap(Kind::Instance)
            }
        }
        ClosedType::Inst(class, args)
            if args.len() == 1
                && (*class == module.core_roles[lm_bytecode::corepin::ROLE_OPTION]
                    || *class == module.core_roles[lm_bytecode::corepin::ROLE_OPTION_SOME]
                    || *class == module.core_roles[lm_bytecode::corepin::ROLE_OPTION_NONE]) =>
        {
            let case = if *class == module.core_roles[lm_bytecode::corepin::ROLE_OPTION_SOME] {
                Some(true)
            } else if *class == module.core_roles[lm_bytecode::corepin::ROLE_OPTION_NONE] {
                Some(false)
            } else {
                None
            };
            Node::Option {
                payload: args[0],
                case,
            }
        }
        ClosedType::Inst(class, args)
            if args.len() == 1 && *class == module.core_roles[lm_bytecode::corepin::ROLE_LIST] =>
        {
            Node::Heap(Kind::List)
        }
        ClosedType::Inst(class, args)
            if args.len() == 2 && *class == module.core_roles[lm_bytecode::corepin::ROLE_MAP] =>
        {
            Node::Heap(Kind::Map)
        }
        ClosedType::Inst(class, args)
            if args.len() == 2
                && *class == module.core_roles[lm_bytecode::corepin::ROLE_FUNCTION_CODE] =>
        {
            Node::Heap(Kind::FunctionCode)
        }
        ClosedType::Inst(class, args)
            if args.len() == 2
                && *class == module.core_roles[lm_bytecode::corepin::ROLE_FUNCTION_DEF] =>
        {
            Node::Heap(Kind::FunctionDef)
        }
        ClosedType::Inst(class, args)
            if args.len() == 2
                && *class == module.core_roles[lm_bytecode::corepin::ROLE_FUNCTION_BINDING] =>
        {
            Node::Heap(Kind::FunctionBinding)
        }
        ClosedType::Inst(_, _) => Node::Heap(Kind::Instance),
    })
}

fn kind_of(object: &Object) -> Kind {
    match object {
        Object::Str(_) => Kind::Str,
        Object::Substring(_) => Kind::Substring,
        Object::StrBuilder(_) => Kind::StringBuilder,
        Object::ByteBuf(_) => Kind::ByteBuffer,
        Object::Bytes(_) => Kind::Bytes,
        Object::NativeFileHandle { .. } => Kind::FileHandle,
        Object::NativeResourceHandle { .. } => Kind::ResourceHandle,
        Object::NativeHostResource { .. } => Kind::HostResource,
        Object::NativeWait { .. } => Kind::Wait,
        Object::NativeFault { .. } => Kind::Fault,
        Object::NativeRequest { .. } => Kind::Request,
        Object::NativeTable { .. } => Kind::PolicyTable,
        Object::NativeVm { .. } => Kind::Vm,
        Object::NativeRun { .. } => Kind::Run,
        Object::NativeDynRef { .. } => Kind::DynValue,
        Object::NativeDigest(_) => Kind::Digest,
        Object::NativeSnapshot(_) | Object::NativeSnapshotRef { .. } => Kind::Snapshot,
        Object::NativeCall { .. } => Kind::PendingCall,
        Object::NativeHandle { .. } => Kind::Handle,
        Object::Closure { .. } => Kind::Closure,
        Object::List { .. } => Kind::List,
        Object::Map { .. } => Kind::Map,
        Object::Tuple { .. } => Kind::Tuple,
        Object::Instance { .. } => Kind::Instance,
        Object::NativeTcpStream { .. } => Kind::TcpStream,
        Object::NativeTcpListener { .. } => Kind::TcpListener,
        Object::NativeTlsStream { .. } => Kind::TlsStream,
        Object::NativeRawMode { .. } => Kind::RawMode,
        Object::NativeSignalStream { .. } => Kind::SignalStream,
        Object::NativePipeReader { .. } => Kind::PipeReader,
        Object::NativePipeWriter { .. } => Kind::PipeWriter,
        Object::NativeChild { .. } => Kind::Child,
        Object::NativeUdpSocket { .. } => Kind::UdpSocket,
        Object::NativeCode(code) => match code.kind {
            lm_heap::PortableCodeKind::Artifact => Kind::Artifact,
            lm_heap::PortableCodeKind::VerifiedModule => Kind::VerifiedModule,
            lm_heap::PortableCodeKind::SlotSpec => Kind::SlotSpec,
            lm_heap::PortableCodeKind::Function => Kind::FunctionCode,
            lm_heap::PortableCodeKind::Class => Kind::ClassCode,
        },
        Object::NativeCodeHandle { kind, .. } => match kind {
            lm_heap::CodeHandleKind::Instance => Kind::CodeInstance,
            lm_heap::CodeHandleKind::Slot => Kind::Slot,
            lm_heap::CodeHandleKind::Function => Kind::FunctionDef,
            lm_heap::CodeHandleKind::Class => Kind::ClassDef,
            lm_heap::CodeHandleKind::FunctionBinding => Kind::FunctionBinding,
            lm_heap::CodeHandleKind::ClassBinding => Kind::ClassBinding,
        },
        Object::NativeSlotChange { .. } => Kind::SlotChange,
        Object::DynValue { .. } => Kind::DynValue,
    }
}

/// Check one object and add its typed children.
fn check_object(
    module: &NamespaceRuntime,
    envs: &mut TypeEnvs,
    scratch: &mut BoundaryScratch,
    object: &Object,
    kind: Kind,
    expect: ClosedTypeId,
) -> Result<(), FaultCode> {
    match (object, kind) {
        (Object::List { items, .. }, Kind::List) => {
            let elem = child(module, envs, expect, 0)?;
            for item in items {
                scratch.work.push((*item, elem));
            }
            Ok(())
        }
        (Object::Map { entries, .. }, Kind::Map) => {
            let key = child(module, envs, expect, 0)?;
            let value = child(module, envs, expect, 1)?;
            for entry in entries {
                if !entry.is_live() {
                    continue;
                }
                scratch.work.push((entry.key, key));
                scratch.work.push((entry.value, value));
            }
            Ok(())
        }
        (Object::Tuple { items }, Kind::Tuple) => {
            let elems = match envs.ty(expect) {
                Some(ClosedType::Tuple(elems)) => elems.clone(),
                _ => return Err(FaultCode::MalformedState),
            };
            if items.len() != elems.len() {
                return Err(FaultCode::TypeMismatch);
            }
            for (item, elem) in items.iter().zip(elems) {
                scratch.work.push((*item, elem));
            }
            Ok(())
        }
        (Object::Instance { class, fields, .. }, Kind::Instance) => {
            check_instance(module, envs, scratch, *class, fields, expect)
        }
        (Object::Closure { func, env, .. }, Kind::Closure) => {
            check_closure(module, envs, scratch, *func, env.env(), expect)
        }
        // A dynamic result reference names a value in another machine.
        // That machine's own code checked the value.
        (Object::NativeDynRef { .. }, Kind::DynValue) => Ok(()),
        (Object::DynValue { value, ty }, Kind::DynValue) => {
            if envs.ty(*ty).is_none() {
                return Err(FaultCode::MalformedState);
            }
            scratch.work.push((*value, *ty));
            Ok(())
        }
        _ => Ok(()),
    }
}

fn child(
    module: &NamespaceRuntime,
    envs: &TypeEnvs,
    expect: ClosedTypeId,
    at: usize,
) -> Result<ClosedTypeId, FaultCode> {
    match (envs.ty(expect), at) {
        (Some(ClosedType::List(elem)), 0) => Ok(*elem),
        (Some(ClosedType::Map(key, _)), 0) => Ok(*key),
        (Some(ClosedType::Map(_, value)), 1) => Ok(*value),
        (Some(ClosedType::Inst(class, args)), _)
            if *class == module.core_roles[lm_bytecode::corepin::ROLE_LIST]
                || *class == module.core_roles[lm_bytecode::corepin::ROLE_MAP] =>
        {
            args.get(at).copied().ok_or(FaultCode::TypeMismatch)
        }
        (Some(ClosedType::Tuple(elems)), _) => {
            elems.get(at).copied().ok_or(FaultCode::TypeMismatch)
        }
        _ => Err(FaultCode::MalformedState),
    }
}

/// Check a closure's callable type before it can execute.
fn check_closure(
    module: &NamespaceRuntime,
    envs: &mut TypeEnvs,
    scratch: &mut BoundaryScratch,
    func: u32,
    env: TypeEnvId,
    expect: ClosedTypeId,
) -> Result<(), FaultCode> {
    let code = module
        .funcs
        .get(func as usize)
        .ok_or(FaultCode::TypeMismatch)?;
    let held = envs.env(env).ok_or(FaultCode::MalformedState)?;
    if held.types.len() != code.type_params as usize
        || held.rows.len() != code.effect_params as usize
    {
        return Err(FaultCode::TypeMismatch);
    }

    let mut params = Vec::with_capacity(code.params.len());
    for param in &code.params {
        params.push(envs.close(module, *param, env).map_err(env_fault)?);
    }
    let result = envs.close(module, code.ret, env).map_err(env_fault)?;
    let row = envs.close_row(module, &code.row, env);
    let actual = envs
        .intern(ClosedType::Fn(params, code.param_muts.clone(), result, row))
        .map_err(env_fault)?;
    if closed_is_subtype(module, envs, scratch, actual, expect)? {
        Ok(())
    } else {
        Err(FaultCode::TypeMismatch)
    }
}

/// Check the closed subtype relation used by callable values.
fn closed_is_subtype(
    module: &NamespaceRuntime,
    envs: &mut TypeEnvs,
    scratch: &mut BoundaryScratch,
    found: ClosedTypeId,
    expected: ClosedTypeId,
) -> Result<bool, FaultCode> {
    scratch.subtype_work.clear();
    scratch.subtype_seen.clear();
    scratch.subtype_work.push((found, expected));
    while let Some((found, expected)) = scratch.subtype_work.pop() {
        if found == expected || !scratch.subtype_seen.insert((found, expected)) {
            continue;
        }
        let found_node = envs.ty(found).cloned().ok_or(FaultCode::MalformedState)?;
        let expected_node = envs
            .ty(expected)
            .cloned()
            .ok_or(FaultCode::MalformedState)?;
        match (found_node, expected_node) {
            (ClosedType::Class(class), ClosedType::Class(parent)) => {
                if envs.ancestor_args(module, class, &[], parent) != Some(Vec::new()) {
                    return Ok(false);
                }
            }
            (ClosedType::Class(class), ClosedType::Inst(parent, expected_args)) => {
                if envs.ancestor_args(module, class, &[], parent) != Some(expected_args) {
                    return Ok(false);
                }
            }
            (ClosedType::Inst(class, args), ClosedType::Class(parent)) => {
                if envs.ancestor_args(module, class, &args, parent) != Some(Vec::new()) {
                    return Ok(false);
                }
            }
            (ClosedType::Inst(class, args), ClosedType::Inst(parent, expected_args)) => {
                if envs.ancestor_args(module, class, &args, parent) != Some(expected_args) {
                    return Ok(false);
                }
            }
            (ClosedType::Tuple(found), ClosedType::Tuple(expected)) => {
                if found.len() != expected.len() {
                    return Ok(false);
                }
                scratch.subtype_work.extend(found.into_iter().zip(expected));
            }
            (
                ClosedType::Fn(found_params, found_muts, found_result, found_row),
                ClosedType::Fn(expected_params, expected_muts, expected_result, expected_row),
            ) => {
                if found_params.len() != expected_params.len()
                    || !found_muts
                        .iter()
                        .zip(expected_muts.iter())
                        .all(|(found, expected)| !*found || *expected)
                    || !row_included(&found_row, &expected_row)
                {
                    return Ok(false);
                }
                scratch
                    .subtype_work
                    .extend(expected_params.into_iter().zip(found_params));
                scratch.subtype_work.push((found_result, expected_result));
            }
            (
                ClosedType::Callback(found_params, found_muts, found_result, found_row),
                ClosedType::Callback(expected_params, expected_muts, expected_result, expected_row),
            ) => {
                if found_params.len() != expected_params.len()
                    || !found_muts
                        .iter()
                        .zip(expected_muts.iter())
                        .all(|(found, expected)| !*found || *expected)
                    || !row_included(&found_row, &expected_row)
                {
                    return Ok(false);
                }
                scratch
                    .subtype_work
                    .extend(expected_params.into_iter().zip(found_params));
                scratch.subtype_work.push((found_result, expected_result));
            }
            _ => return Ok(false),
        }
    }
    Ok(true)
}

fn row_included(sub: &ClosedRow, sup: &ClosedRow) -> bool {
    sub.iter().all(|slot| sup.binary_search(slot).is_ok())
}

/// Check one instance against one closed class position.
fn check_instance(
    module: &NamespaceRuntime,
    envs: &mut TypeEnvs,
    scratch: &mut BoundaryScratch,
    class: u32,
    fields: &[Value],
    expect: ClosedTypeId,
) -> Result<(), FaultCode> {
    let layout = module
        .classes
        .get(class as usize)
        .ok_or(FaultCode::TypeMismatch)?;
    if fields.len() != layout.fields.len() {
        return Err(FaultCode::TypeMismatch);
    }
    let (want_class, want_args) = envs.as_instance(expect).ok_or(FaultCode::MalformedState)?;

    let actual_args = if class == want_class {
        want_args.clone()
    } else if layout.type_params == 0 {
        Vec::new()
    } else if layout.type_params as usize == want_args.len() {
        // An enum case passes its family arguments through unchanged.
        want_args.clone()
    } else {
        return Err(FaultCode::TypeMismatch);
    };
    if actual_args.len() != layout.type_params as usize {
        return Err(FaultCode::TypeMismatch);
    }
    if envs.ancestor_args(module, class, &actual_args, want_class) != Some(want_args) {
        return Err(FaultCode::TypeMismatch);
    }

    let field_env = envs.env_of(actual_args, Vec::new()).map_err(env_fault)?;
    for (value, (_, field_ty)) in fields.iter().zip(layout.fields.iter()) {
        if *value == Value::Uninit {
            continue;
        }
        let field = envs
            .close(module, *field_ty, field_env)
            .map_err(env_fault)?;
        scratch.work.push((*value, field));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lm_bytecode::{BcClass, BcClassKind, BcRow, Func, NO_PARENT};
    use lm_value::Witness;
    use std::sync::Arc;

    const TY_INT: u32 = 2;
    const TY_STR: u32 = 3;

    fn module(
        types: Vec<BcType>,
        classes: Vec<BcClass>,
        mut funcs: Vec<Func>,
    ) -> Arc<NamespaceRuntime> {
        if funcs.is_empty() {
            funcs.push(Func {
                name: "main".to_string(),
                param_names: Vec::new(),
                type_params: 0,
                effect_params: 0,
                params: Vec::new(),
                param_muts: Vec::new(),
                ret: 0,
                row: Vec::new(),
                captures: Vec::new(),
                local_types: Vec::new(),
                blocks: vec![vec![
                    lm_bytecode::Instr::ConstUnit,
                    lm_bytecode::Instr::Return,
                ]],
            });
        }
        for function in &mut funcs {
            if function.blocks.is_empty() {
                function.blocks = vec![vec![
                    lm_bytecode::Instr::LoadLocal(0),
                    lm_bytecode::Instr::Return,
                ]];
            }
        }
        let entry = if funcs[0].params.is_empty() && funcs[0].captures.is_empty() {
            0
        } else {
            let entry = funcs.len() as u32;
            funcs.push(Func {
                name: "main".to_string(),
                param_names: Vec::new(),
                type_params: 0,
                effect_params: 0,
                params: Vec::new(),
                param_muts: Vec::new(),
                ret: 0,
                row: Vec::new(),
                captures: Vec::new(),
                local_types: Vec::new(),
                blocks: vec![vec![
                    lm_bytecode::Instr::ConstUnit,
                    lm_bytecode::Instr::Return,
                ]],
            });
            entry
        };
        let class_bounds = classes
            .iter()
            .map(|class| vec![Vec::new(); class.type_params as usize])
            .collect();
        let func_bounds = funcs
            .iter()
            .map(|function| vec![Vec::new(); function.type_params as usize])
            .collect();
        crate::unit_from_module_for_test(Module {
            strings: vec!["Io.Write".to_string()],
            bytes: vec![],
            types,
            selectors: Vec::new(),
            apps: Vec::new(),
            interfaces: Vec::new(),
            conformances: Vec::new(),
            class_bounds,
            func_bounds,
            imports: Vec::new(),
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            classes,
            funcs,
            entry,
            exports: Vec::new(),
            bindings: Vec::new(),
            debug: Vec::new(),
        })
        .expect("the boundary test unit verifies")
    }

    fn base_types() -> Vec<BcType> {
        vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str]
    }

    fn function(row: Vec<BcRow>) -> Func {
        Func {
            name: "body".to_string(),
            param_names: vec!["value".to_string()],
            type_params: 0,
            effect_params: 0,
            params: vec![TY_INT],
            param_muts: vec![false],
            ret: TY_INT,
            row,
            captures: Vec::new(),
            local_types: vec![TY_INT],
            blocks: Vec::new(),
        }
    }

    #[test]
    fn a_root_type_variable_uses_the_frame_environment() {
        let mut types = base_types();
        types.push(BcType::Var(0));
        let module = module(types, Vec::new(), Vec::new());
        let heap = Heap::new(1 << 20);
        let mut envs = TypeEnvs::default();
        let int = envs.intern(ClosedType::Int).expect("the type interns");
        let env = envs
            .env_of(vec![int], Vec::new())
            .expect("the environment interns");
        let mut scratch = BoundaryScratch::default();

        assert_eq!(
            check_boundary_value(
                BoundaryContext::new(&module, &heap),
                &mut envs,
                &mut scratch,
                Value::Int(41),
                4,
                env,
            ),
            Ok(())
        );
    }

    #[test]
    fn a_closure_with_another_parameter_type_does_not_fit() {
        let mut types = base_types();
        types.push(BcType::Fn(vec![TY_STR], vec![false], TY_INT, Vec::new()));
        let module = module(types, Vec::new(), vec![function(Vec::new())]);
        let mut heap = Heap::new(1 << 20);
        let closure = heap.alloc(Object::Closure {
            func: 0,
            captures: Vec::new().into(),
            env: Witness::EMPTY,
        });
        let mut envs = TypeEnvs::default();
        let mut scratch = BoundaryScratch::default();

        assert_eq!(
            check_boundary_value(
                BoundaryContext::new(&module, &heap),
                &mut envs,
                &mut scratch,
                Value::Obj(closure),
                4,
                TypeEnvId::EMPTY,
            ),
            Err(FaultCode::TypeMismatch)
        );
    }

    #[test]
    fn an_effectful_closure_does_not_fit_a_pure_function_type() {
        let mut types = base_types();
        types.push(BcType::Fn(vec![TY_INT], vec![false], TY_INT, Vec::new()));
        let module = module(types, Vec::new(), vec![function(vec![BcRow::Op(0)])]);
        let mut heap = Heap::new(1 << 20);
        let closure = heap.alloc(Object::Closure {
            func: 0,
            captures: Vec::new().into(),
            env: Witness::EMPTY,
        });
        let mut envs = TypeEnvs::default();
        let mut scratch = BoundaryScratch::default();

        assert_eq!(
            check_boundary_value(
                BoundaryContext::new(&module, &heap),
                &mut envs,
                &mut scratch,
                Value::Obj(closure),
                4,
                TypeEnvId::EMPTY,
            ),
            Err(FaultCode::TypeMismatch)
        );
    }

    #[test]
    fn a_child_of_another_generic_application_does_not_fit() {
        let mut types = base_types();
        types.push(BcType::Inst(0, vec![TY_INT]));
        let class = |name: &str, parent: u32, parent_args: Vec<u32>, type_params: u32| BcClass {
            name: name.to_string(),
            key: name.to_string(),
            is_final: false,
            is_frozen: false,
            parent,
            parent_args,
            type_params,
            kind: BcClassKind::Normal,
            fields: Vec::new(),
            field_defaults: Vec::new(),
            own_start: 0,
            has_init: false,
            methods: Vec::new(),
        };
        let classes = vec![
            class("Parent", NO_PARENT, Vec::new(), 1),
            class("StringChild", 0, vec![TY_STR], 0),
        ];
        let module = module(types, classes, Vec::new());
        let mut heap = Heap::new(1 << 20);
        let instance = heap.alloc(Object::Instance {
            class: 1,
            fields: Vec::new().into(),
            env: Witness::EMPTY,
        });
        let mut envs = TypeEnvs::default();
        let mut scratch = BoundaryScratch::default();

        assert_eq!(
            check_boundary_value(
                BoundaryContext::new(&module, &heap),
                &mut envs,
                &mut scratch,
                Value::Obj(instance),
                4,
                TypeEnvId::EMPTY,
            ),
            Err(FaultCode::TypeMismatch)
        );
    }
}
