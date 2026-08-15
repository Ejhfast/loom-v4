//! Definition and module identity (specification 3.7).
//!
//! A definition hash covers a dedicated canonical encoding, never the
//! raw semantic-section bytes. Every module-global index is replaced:
//!
//! - function and class references become the referenced definition
//!   hash, or the member ordinal for a same-component reference;
//! - string-pool indices become the inline string content;
//! - type-table indices become a structural type digest, with class
//!   references by definition identity;
//! - application indices become an application digest over structural
//!   types and canonical rows;
//! - selector indices become the selector name;
//! - lifted closure bodies are embedded through a body digest and are
//!   identified by their parent identity and occurrence index;
//! - local slots, block indices, argument counts, manifest operation
//!   slots, and table-edit operands stay as-is: they are
//!   function-local or manifest-stable, never module-positional.
//!
//! Mutually recursive definitions form strongly connected components
//! found with an iterative Tarjan walk (pinned traversal: roots in
//! ascending definition index, successors in ascending reference
//! order). A component is canonically ordered by name and receives
//! one component hash; member hashes are domain-separated from the
//! component hash and the ordinal. Tarjan emits components
//! callees-first, and that emission order is the hash schedule.
//!
//! The domain tags:
//!
//! - `lm-type-v1`: structural type digest;
//! - `lm-app-v1`: type-application digest;
//! - `lm-closure-body-v1`: closure body digest;
//! - `lm-def-component-v1`: component hash;
//! - `lm-def-member-v1`: member (definition) hash;
//! - `lm-def-closure-v1`: closure definition hash;
//! - `lm-def-closure-cyclic-v1`: closure hash inside a hand-built
//!   `MakeClosure` cycle;
//! - `lm-module-sem-v1`: module semantic hash.
//!
//! The computation runs on decoded but unverified modules, so it
//! validates every index first, allocates only linearly in the input,
//! and never recurses on the Rust stack over untrusted shapes.

use crate::hash::sha256;
use crate::{BcClassKind, BcRow, BcType, Instr, Module, NO_PARENT, VERSION};
use std::collections::HashMap;

/// The compiler ABI version. It covers the canonical bytecode
/// semantics, the identity encoding, and the lowering conventions.
/// Bump rules: increment on any change to the instruction set
/// semantics, the canonical identity encoding, the hash domains, or
/// the lowering ABI. The operation manifest is covered separately by
/// `lm_abi::manifest_digest()`, which every definition hash includes.
///
/// Version 2 makes class identity nominal and adds the import set.
pub const COMPILER_ABI_VERSION: u32 = 2;

/// A failure to compute identity: the module structure is not
/// hashable. The verifier rejects every such module too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityError(pub String);

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "identity: {}", self.0)
    }
}

fn fail(message: impl Into<String>) -> IdentityError {
    IdentityError(message.into())
}

/// The identity of one module: a definition hash per class and per
/// function, plus the module semantic hash.
#[derive(Debug, Clone)]
pub struct ModuleIdentity {
    pub class_hashes: Vec<[u8; 32]>,
    pub func_hashes: Vec<[u8; 32]>,
    pub semantic_hash: [u8; 32],
}

impl ModuleIdentity {
    /// The definition hash of one class.
    pub fn class_hash(&self, class: u32) -> [u8; 32] {
        self.class_hashes[class as usize]
    }
}

/// The container hash: SHA-256 over the exact container bytes.
pub fn container_hash(bytes: &[u8]) -> [u8; 32] {
    sha256(bytes)
}

/// The verification hash: an index-preserving digest over every
/// verifier input.
///
/// This answers a different question from the semantic hash. The
/// semantic hash answers "do these bytes mean the same program?": it
/// replaces every module-global index with content, so two modules
/// that differ only in an index can share it. The verification hash
/// answers "did the verifier approve this exact representation?": it
/// keeps every index, because the verifier reads indices.
///
/// A verified-code cache must key on this hash. A key built on the
/// semantic hash lets a future semantic equivalence certify a module
/// the verifier rejects. Keying here also lets semantic identity
/// evolve without touching cache soundness.
///
/// The digest covers the semantic region, the operation manifest, and
/// the class names. The manifest is a verifier input, because the row
/// and signature rules read it, and it is not stored in the container.
///
/// The class names are a verifier input since class identity became
/// nominal: the hash-linked core layout resolves a slot by class name
/// and definition hash, and the verifier reads that layout. The names
/// therefore must fix the key. Function names stay out, because no
/// verifier rule and no identity rule reads one, so a function rename
/// must not cost a cache hit.
///
/// The key fixes every input of `module_identity` too, so a cache
/// entry may carry the resolved identity and core layout.
pub fn verification_hash(module: &Module) -> [u8; 32] {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(TAG_VERIFICATION);
    bytes.extend_from_slice(&lm_abi::manifest_digest());
    bytes.extend_from_slice(&crate::semantic_section(module));
    bytes.extend_from_slice(&(module.classes.len() as u32).to_le_bytes());
    for class in &module.classes {
        write_str(&mut bytes, &class.name);
    }
    sha256(&bytes)
}

// ----------------------------------------------------------------
// Node space and preflight validation.
// ----------------------------------------------------------------

/// The node space of the reference graph: classes, then functions,
/// then types, then applications.
#[derive(Clone, Copy)]
struct Space {
    classes: usize,
    funcs: usize,
    types: usize,
    apps: usize,
}

impl Space {
    fn of(module: &Module) -> Space {
        Space {
            classes: module.classes.len(),
            funcs: module.funcs.len(),
            types: module.types.len(),
            apps: module.apps.len(),
        }
    }

    fn total(&self) -> usize {
        self.classes + self.funcs + self.types + self.apps
    }

    fn class_node(&self, c: u32) -> u32 {
        c
    }

    fn func_node(&self, f: u32) -> u32 {
        (self.classes + f as usize) as u32
    }

    fn type_node(&self, t: u32) -> u32 {
        (self.classes + self.funcs + t as usize) as u32
    }

    fn app_node(&self, a: u32) -> u32 {
        (self.classes + self.funcs + self.types + a as usize) as u32
    }
}

/// Validate every index the identity encoding follows. The rules are
/// a subset of the verifier rules, so a rejection here is always a
/// rejection there.
fn preflight(module: &Module) -> Result<(), IdentityError> {
    let s = Space::of(module);
    let strings = module.strings.len();
    let check_row = |what: &str, row: &[BcRow]| -> Result<(), IdentityError> {
        for elem in row {
            if let BcRow::Op(idx) = elem {
                if *idx as usize >= strings {
                    return Err(fail(format!("{what}: row string index out of range")));
                }
            }
        }
        Ok(())
    };
    for (idx, ty) in module.types.iter().enumerate() {
        let earlier = |child: u32| -> Result<(), IdentityError> {
            if child as usize >= idx {
                Err(fail(format!(
                    "type {idx} references type {child}, which is not an earlier entry"
                )))
            } else {
                Ok(())
            }
        };
        let class_ok = |c: u32| -> Result<(), IdentityError> {
            if c as usize >= s.classes {
                Err(fail(format!("type {idx} names class {c} out of range")))
            } else {
                Ok(())
            }
        };
        match ty {
            BcType::Unit
            | BcType::Bool
            | BcType::Int
            | BcType::Str
            | BcType::StringBuilder
            | BcType::ByteBuffer
            | BcType::Fault
            | BcType::Request
            | BcType::PolicyTable
            | BcType::EmptyVm
            | BcType::Var(_) => {}
            BcType::Class(c) => class_ok(*c)?,
            BcType::Inst(c, args) => {
                class_ok(*c)?;
                for a in args {
                    earlier(*a)?;
                }
            }
            BcType::List(e) => earlier(*e)?,
            BcType::Map(k, v) => {
                earlier(*k)?;
                earlier(*v)?;
            }
            BcType::Tuple(elems) => {
                for e in elems {
                    earlier(*e)?;
                }
            }
            BcType::Fn(params, muts, ret, row) => {
                if muts.len() != params.len() {
                    return Err(fail(format!("type {idx}: mut markers do not align")));
                }
                for p in params {
                    earlier(*p)?;
                }
                earlier(*ret)?;
                check_row(&format!("type {idx}"), row)?;
            }
            BcType::Vm(t) => earlier(*t)?,
            BcType::PendingCall(a, r) => {
                earlier(*a)?;
                earlier(*r)?;
            }
            BcType::Op(_, f) => earlier(*f)?,
        }
    }
    for (aidx, app) in module.apps.iter().enumerate() {
        for t in &app.types {
            if *t as usize >= s.types {
                return Err(fail(format!("application {aidx}: type index out of range")));
            }
        }
        for row in &app.rows {
            check_row(&format!("application {aidx}"), row)?;
        }
    }
    for (cidx, class) in module.classes.iter().enumerate() {
        if class.parent != NO_PARENT && class.parent as usize >= s.classes {
            return Err(fail(format!("class {cidx}: parent out of range")));
        }
        for (fname, fty) in &class.fields {
            if *fty as usize >= s.types {
                return Err(fail(format!(
                    "class {cidx}: field `{fname}` type out of range"
                )));
            }
        }
        for (sel, func) in &class.methods {
            if *sel as usize >= module.selectors.len() {
                return Err(fail(format!("class {cidx}: selector out of range")));
            }
            if *func as usize >= s.funcs {
                return Err(fail(format!("class {cidx}: method function out of range")));
            }
        }
    }
    for (fidx, func) in module.funcs.iter().enumerate() {
        if func.param_muts.len() != func.params.len() {
            return Err(fail(format!("function {fidx}: mut markers do not align")));
        }
        for t in func
            .params
            .iter()
            .chain(func.captures.iter())
            .chain(func.local_types.iter())
            .chain([&func.ret])
        {
            if *t as usize >= s.types {
                return Err(fail(format!(
                    "function {fidx}: signature type index out of range"
                )));
            }
        }
        check_row(&format!("function {fidx}"), &func.row)?;
        for block in &func.blocks {
            for instr in block {
                preflight_instr(module, &s, fidx, instr)?;
            }
        }
    }
    if module.entry as usize >= s.funcs {
        return Err(fail("the entry index is out of range"));
    }
    // Import slots: each names a definition of its own kind, and each
    // definition takes at most one slot. The decoder checks the same
    // rule; a hand-built module reaches identity without a decoder.
    let mut claimed_classes = vec![false; s.classes];
    let mut claimed_funcs = vec![false; s.funcs];
    for (idx, import) in module.imports.iter().enumerate() {
        let claimed = if import.kind.is_func() {
            &mut claimed_funcs
        } else {
            &mut claimed_classes
        };
        let at = import.def as usize;
        if at >= claimed.len() {
            return Err(fail(format!("import {idx}: definition index out of range")));
        }
        if claimed[at] {
            return Err(fail(format!(
                "import {idx}: the definition already has an import slot"
            )));
        }
        claimed[at] = true;
    }
    Ok(())
}

/// Validate the operand indices of one instruction. The match is
/// exhaustive without a wildcard arm, so a new operand kind fails to
/// compile until its canonical form is decided.
fn preflight_instr(
    module: &Module,
    s: &Space,
    fidx: usize,
    instr: &Instr,
) -> Result<(), IdentityError> {
    let bad = |what: &str| fail(format!("function {fidx}: {what} out of range"));
    let strings = module.strings.len();
    let selectors = module.selectors.len();
    match instr {
        Instr::ConstUnit
        | Instr::ConstBool(_)
        | Instr::ConstInt(_)
        | Instr::Pop
        | Instr::Add
        | Instr::Sub
        | Instr::Mul
        | Instr::Div
        | Instr::Rem
        | Instr::Neg
        | Instr::Not
        | Instr::LtInt
        | Instr::LeInt
        | Instr::GtInt
        | Instr::GeInt
        | Instr::EqInt
        | Instr::NeInt
        | Instr::EqBool
        | Instr::NeBool
        | Instr::EqStr
        | Instr::NeStr
        | Instr::EqRef
        | Instr::NeRef
        | Instr::ListLen
        | Instr::ListAt
        | Instr::ListPush
        | Instr::MapLen
        | Instr::MapHas
        | Instr::MapAt
        | Instr::MapPut
        | Instr::SbNew
        | Instr::SbAppendStr
        | Instr::SbAppendInt
        | Instr::SbAppendBool
        | Instr::SbBuild
        | Instr::BbNew
        | Instr::BbAppend
        | Instr::BbLen
        | Instr::BbBuild
        | Instr::Freeze
        | Instr::Return
        | Instr::CallArgs
        | Instr::FaultCode
        | Instr::Unreachable => Ok(()),
        Instr::ConstStr(idx) => {
            if *idx as usize >= strings {
                return Err(bad("string index"));
            }
            Ok(())
        }
        Instr::LoadLocal(_)
        | Instr::StoreLocal(_)
        | Instr::LoadCapture(_)
        | Instr::LoadField(_)
        | Instr::StoreField(_)
        | Instr::TupleGet(_)
        | Instr::Jump(_)
        | Instr::JumpIfFalse(_)
        | Instr::JumpIfTrue(_) => Ok(()),
        Instr::Call(func) => {
            if *func as usize >= s.funcs {
                return Err(bad("call target"));
            }
            Ok(())
        }
        Instr::CallG { func, app } => {
            if *func as usize >= s.funcs {
                return Err(bad("call target"));
            }
            if *app as usize >= s.apps {
                return Err(bad("type application"));
            }
            Ok(())
        }
        Instr::CallVirtual { selector, argc: _ } => {
            if *selector as usize >= selectors {
                return Err(bad("selector"));
            }
            Ok(())
        }
        Instr::CallVirtualG {
            selector,
            argc: _,
            app,
        } => {
            if *selector as usize >= selectors {
                return Err(bad("selector"));
            }
            if *app as usize >= s.apps {
                return Err(bad("type application"));
            }
            Ok(())
        }
        Instr::CallValue { argc: _ } => Ok(()),
        Instr::MakeClosure { func, captures: _ } => {
            if *func as usize >= s.funcs {
                return Err(bad("closure function"));
            }
            Ok(())
        }
        Instr::New(class) => {
            if *class as usize >= s.classes {
                return Err(bad("class"));
            }
            Ok(())
        }
        Instr::NewG { class, app } => {
            if *class as usize >= s.classes {
                return Err(bad("class"));
            }
            if *app as usize >= s.apps {
                return Err(bad("type application"));
            }
            Ok(())
        }
        Instr::TupleNew { ty, count: _ }
        | Instr::ListNew { ty, count: _ }
        | Instr::MapNew { ty, count: _ } => {
            if *ty as usize >= s.types {
                return Err(bad("type index"));
            }
            Ok(())
        }
        Instr::IsType(ty) | Instr::CastType(ty) => {
            if *ty as usize >= s.types {
                return Err(bad("type index"));
            }
            Ok(())
        }
        Instr::Perform { op: _, argc: _ }
        | Instr::PerformValue { argc: _ }
        | Instr::OpConst(_)
        | Instr::TableEdit { .. }
        | Instr::AsCall(_) => Ok(()),
    }
}

// ----------------------------------------------------------------
// Reference graph and iterative Tarjan.
// ----------------------------------------------------------------

/// The reference graph plus the closure-body classification.
struct Graph {
    space: Space,
    /// Successors per node, in ascending reference (encounter) order.
    succ: Vec<Vec<u32>>,
    /// True for a function that is a lifted closure body: it is a
    /// `MakeClosure` target and nothing else references it as a
    /// callable definition.
    closure_body: Vec<bool>,
    /// The case-class indices per class, in class-index (arm) order.
    /// Only an abstract enum parent has entries.
    arms: Vec<Vec<u32>>,
    /// True for an imported class. An imported definition is an
    /// identity leaf: its hash is the pin, not a computed digest.
    extern_class: Vec<bool>,
    /// True for an imported function.
    extern_func: Vec<bool>,
}

impl Graph {
    fn build(module: &Module) -> Graph {
        let s = Space::of(module);
        let mut succ: Vec<Vec<u32>> = vec![Vec::new(); s.total()];
        let mut made_closure = vec![false; s.funcs];
        let mut called = vec![false; s.funcs];
        let extern_class = module.extern_classes();
        let extern_func = module.extern_funcs();
        for (cidx, class) in module.classes.iter().enumerate() {
            if extern_class[cidx] {
                // An imported class takes its identity from the pinned
                // interface hash, so it references nothing.
                continue;
            }
            let node = s.class_node(cidx as u32);
            if class.parent != NO_PARENT {
                succ[node as usize].push(s.class_node(class.parent));
            }
            for (_, fty) in &class.fields {
                succ[node as usize].push(s.type_node(*fty));
            }
            for (_, func) in &class.methods {
                succ[node as usize].push(s.func_node(*func));
                called[*func as usize] = true;
            }
        }
        // An abstract enum parent references its case classes: the
        // closed arm set is part of the family identity, and the
        // cycle puts the family into one component, so the
        // name-ordered member ordinals separate structurally
        // identical arms and families.
        let mut arms: Vec<Vec<u32>> = vec![Vec::new(); s.classes];
        for (cidx, class) in module.classes.iter().enumerate() {
            if extern_class[cidx] || class.parent == NO_PARENT {
                continue;
            }
            if class.kind == BcClassKind::Case && !extern_class[class.parent as usize] {
                succ[class.parent as usize].push(s.class_node(cidx as u32));
                arms[class.parent as usize].push(cidx as u32);
            }
        }
        for (fidx, func) in module.funcs.iter().enumerate() {
            if extern_func[fidx] {
                // An imported function takes its identity from the
                // pinned interface hash.
                continue;
            }
            let node = s.func_node(fidx as u32);
            let list = &mut succ[node as usize];
            for t in func
                .params
                .iter()
                .chain([&func.ret])
                .chain(func.captures.iter())
                .chain(func.local_types.iter())
            {
                list.push(s.type_node(*t));
            }
            for block in &func.blocks {
                for instr in block {
                    match instr {
                        Instr::Call(f) => {
                            list.push(s.func_node(*f));
                            called[*f as usize] = true;
                        }
                        Instr::CallG { func: f, app } => {
                            list.push(s.func_node(*f));
                            list.push(s.app_node(*app));
                            called[*f as usize] = true;
                        }
                        Instr::CallVirtualG { app, .. } => list.push(s.app_node(*app)),
                        Instr::MakeClosure { func: f, .. } => {
                            list.push(s.func_node(*f));
                            made_closure[*f as usize] = true;
                        }
                        Instr::New(c) => list.push(s.class_node(*c)),
                        Instr::NewG { class, app } => {
                            list.push(s.class_node(*class));
                            list.push(s.app_node(*app));
                        }
                        Instr::TupleNew { ty, .. }
                        | Instr::ListNew { ty, .. }
                        | Instr::MapNew { ty, .. }
                        | Instr::IsType(ty)
                        | Instr::CastType(ty) => list.push(s.type_node(*ty)),
                        _ => {}
                    }
                }
            }
        }
        called[module.entry as usize] = true;
        for (tidx, ty) in module.types.iter().enumerate() {
            let node = s.type_node(tidx as u32);
            let list = &mut succ[node as usize];
            match ty {
                BcType::Class(c) => list.push(s.class_node(*c)),
                BcType::Inst(c, args) => {
                    list.push(s.class_node(*c));
                    for a in args {
                        list.push(s.type_node(*a));
                    }
                }
                BcType::List(e) => list.push(s.type_node(*e)),
                BcType::Map(k, v) => {
                    list.push(s.type_node(*k));
                    list.push(s.type_node(*v));
                }
                BcType::Tuple(elems) => {
                    for e in elems {
                        list.push(s.type_node(*e));
                    }
                }
                BcType::Fn(params, _, ret, _) => {
                    for p in params {
                        list.push(s.type_node(*p));
                    }
                    list.push(s.type_node(*ret));
                }
                BcType::Vm(t) => list.push(s.type_node(*t)),
                BcType::PendingCall(a, r) => {
                    list.push(s.type_node(*a));
                    list.push(s.type_node(*r));
                }
                BcType::Op(_, f) => list.push(s.type_node(*f)),
                _ => {}
            }
        }
        for (aidx, app) in module.apps.iter().enumerate() {
            let node = s.app_node(aidx as u32);
            for t in &app.types {
                succ[node as usize].push(s.type_node(*t));
            }
        }
        let closure_body: Vec<bool> = (0..s.funcs)
            .map(|f| made_closure[f] && !called[f] && !extern_func[f])
            .collect();
        Graph {
            space: s,
            succ,
            closure_body,
            arms,
            extern_class,
            extern_func,
        }
    }
}

/// Iterative Tarjan with an explicit work stack. Roots run in
/// ascending node index; successors run in ascending reference
/// order. Components pop callees-first, and that emission order is
/// the hash schedule. Returns the components and the component index
/// per node.
fn tarjan(graph: &Graph) -> (Vec<Vec<u32>>, Vec<u32>) {
    let n = graph.space.total();
    const UNSET: u32 = u32::MAX;
    let mut index = vec![UNSET; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<u32> = Vec::new();
    let mut next = 0u32;
    let mut comps: Vec<Vec<u32>> = Vec::new();
    let mut comp_of = vec![UNSET; n];
    // The explicit DFS work stack: (node, next successor position).
    let mut work: Vec<(u32, usize)> = Vec::new();
    for root in 0..n as u32 {
        if index[root as usize] != UNSET {
            continue;
        }
        work.push((root, 0));
        index[root as usize] = next;
        low[root as usize] = next;
        next += 1;
        stack.push(root);
        on_stack[root as usize] = true;
        while let Some((node, pos)) = work.last().copied() {
            let succs = &graph.succ[node as usize];
            if pos < succs.len() {
                work.last_mut().expect("frame").1 += 1;
                let child = succs[pos];
                if index[child as usize] == UNSET {
                    index[child as usize] = next;
                    low[child as usize] = next;
                    next += 1;
                    stack.push(child);
                    on_stack[child as usize] = true;
                    work.push((child, 0));
                } else if on_stack[child as usize] {
                    let li = low[node as usize].min(index[child as usize]);
                    low[node as usize] = li;
                }
            } else {
                work.pop();
                if let Some((parent, _)) = work.last() {
                    let li = low[*parent as usize].min(low[node as usize]);
                    low[*parent as usize] = li;
                }
                if low[node as usize] == index[node as usize] {
                    let mut comp = Vec::new();
                    loop {
                        let member = stack.pop().expect("tarjan stack");
                        on_stack[member as usize] = false;
                        comp_of[member as usize] = comps.len() as u32;
                        comp.push(member);
                        if member == node {
                            break;
                        }
                    }
                    comps.push(comp);
                }
            }
        }
    }
    (comps, comp_of)
}

// ----------------------------------------------------------------
// Canonical encoding and hashing.
// ----------------------------------------------------------------

/// The domain tags.
const TAG_TYPE: &[u8] = b"lm-type-v1\0";
const TAG_APP: &[u8] = b"lm-app-v1\0";
const TAG_BODY: &[u8] = b"lm-closure-body-v1\0";
const TAG_COMPONENT: &[u8] = b"lm-def-component-v1\0";
const TAG_MEMBER: &[u8] = b"lm-def-member-v1\0";
const TAG_CLOSURE: &[u8] = b"lm-def-closure-v1\0";
const TAG_CLOSURE_CYCLIC: &[u8] = b"lm-def-closure-cyclic-v1\0";
const TAG_MODULE: &[u8] = b"lm-module-sem-v1\0";
const TAG_VERIFICATION: &[u8] = b"lm-module-verify-v1\0";

/// One identity reference inside canonical bytes.
enum IdentRef {
    /// A definition outside the current component, by member hash.
    Hash([u8; 32]),
    /// A member of the current component, by canonical ordinal.
    Ordinal(u32),
}

fn write_ident(out: &mut Vec<u8>, r: &IdentRef) {
    match r {
        IdentRef::Hash(h) => {
            out.push(0x00);
            out.extend_from_slice(h);
        }
        IdentRef::Ordinal(o) => {
            out.push(0x01);
            out.extend_from_slice(&o.to_le_bytes());
        }
    }
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// The per-module hashing state, filled in Tarjan emission order.
struct HashState {
    class_hash: Vec<Option<[u8; 32]>>,
    func_hash: Vec<Option<[u8; 32]>>,
    /// Final structural type digests (all class references by hash).
    type_final: Vec<Option<[u8; 32]>>,
    app_final: Vec<Option<[u8; 32]>>,
    /// Final closure body digests.
    body_final: Vec<Option<[u8; 32]>>,
    /// The component hash per node, for the cyclic-closure fallback.
    comp_hash: Vec<[u8; 32]>,
}

/// One resolution context: the digests and identities visible while
/// serializing one component, or the final all-hash view.
struct Resolver<'a> {
    module: &'a Module,
    graph: &'a Graph,
    state: &'a HashState,
    /// The current component index, or `None` for the final view.
    comp: Option<u32>,
    comp_of: &'a [u32],
    /// Ordinals of the definition members of the current component.
    /// The lookups are maps, not scans: identity runs on untrusted
    /// bytes before the verifier, so the load path stays bounded.
    ordinals: &'a HashMap<u32, u32>,
    /// Intra-component digest overlays.
    type_intra: &'a HashMap<u32, [u8; 32]>,
    app_intra: &'a HashMap<u32, [u8; 32]>,
    body_intra: &'a HashMap<u32, [u8; 32]>,
    /// Closure bodies on the active serialization path. A reference
    /// to one is a hand-built cycle and gets a marker.
    on_path: &'a [u32],
    /// The in-component closure list, for the cycle-marker position.
    closure_list: &'a [u32],
}

impl<'a> Resolver<'a> {
    fn in_comp(&self, node: u32) -> bool {
        match self.comp {
            Some(c) => self.comp_of[node as usize] == c,
            None => false,
        }
    }

    fn ordinal_of(&self, node: u32) -> u32 {
        *self
            .ordinals
            .get(&node)
            .expect("every in-component definition member has an ordinal")
    }

    fn class_ident(&self, c: u32) -> IdentRef {
        let node = self.graph.space.class_node(c);
        if self.in_comp(node) {
            IdentRef::Ordinal(self.ordinal_of(node))
        } else {
            IdentRef::Hash(self.state.class_hash[c as usize].expect("class hash scheduled"))
        }
    }

    fn func_ident(&self, f: u32) -> IdentRef {
        let node = self.graph.space.func_node(f);
        if self.in_comp(node) {
            IdentRef::Ordinal(self.ordinal_of(node))
        } else {
            IdentRef::Hash(self.state.func_hash[f as usize].expect("function hash scheduled"))
        }
    }

    fn type_digest(&self, t: u32) -> [u8; 32] {
        let node = self.graph.space.type_node(t);
        if self.in_comp(node) {
            *self
                .type_intra
                .get(&t)
                .expect("in-component type digest computed")
        } else {
            self.state.type_final[t as usize].expect("type digest scheduled")
        }
    }

    fn app_digest(&self, a: u32) -> [u8; 32] {
        let node = self.graph.space.app_node(a);
        if self.in_comp(node) {
            *self
                .app_intra
                .get(&a)
                .expect("in-component app digest computed")
        } else {
            self.state.app_final[a as usize].expect("app digest scheduled")
        }
    }

    fn body_digest(&self, f: u32) -> [u8; 32] {
        if let Some(d) = self.body_intra.get(&f) {
            return *d;
        }
        self.state.body_final[f as usize].expect("body digest scheduled")
    }

    /// The canonical row bytes: operation names inline, variables by
    /// index.
    fn row_bytes(&self, out: &mut Vec<u8>, row: &[BcRow]) {
        out.extend_from_slice(&(row.len() as u32).to_le_bytes());
        for elem in row {
            match elem {
                BcRow::Op(idx) => {
                    out.push(0x00);
                    write_str(out, &self.module.strings[*idx as usize]);
                }
                BcRow::Var(v) => {
                    out.push(0x01);
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
    }

    /// The structural digest bytes of one type entry, using the child
    /// digests and the class identities of this context.
    fn type_digest_of(&self, ty: &BcType) -> [u8; 32] {
        let mut out = Vec::new();
        out.extend_from_slice(TAG_TYPE);
        match ty {
            BcType::Unit => out.push(0),
            BcType::Bool => out.push(1),
            BcType::Int => out.push(2),
            BcType::Str => out.push(3),
            BcType::Class(c) => {
                out.push(4);
                write_ident(&mut out, &self.class_ident(*c));
            }
            BcType::Inst(c, args) => {
                out.push(5);
                write_ident(&mut out, &self.class_ident(*c));
                out.extend_from_slice(&(args.len() as u32).to_le_bytes());
                for a in args {
                    out.extend_from_slice(&self.type_digest(*a));
                }
            }
            BcType::List(e) => {
                out.push(6);
                out.extend_from_slice(&self.type_digest(*e));
            }
            BcType::Map(k, v) => {
                out.push(7);
                out.extend_from_slice(&self.type_digest(*k));
                out.extend_from_slice(&self.type_digest(*v));
            }
            BcType::Tuple(elems) => {
                out.push(8);
                out.extend_from_slice(&(elems.len() as u32).to_le_bytes());
                for e in elems {
                    out.extend_from_slice(&self.type_digest(*e));
                }
            }
            BcType::Fn(params, muts, ret, row) => {
                out.push(9);
                out.extend_from_slice(&(params.len() as u32).to_le_bytes());
                for p in params {
                    out.extend_from_slice(&self.type_digest(*p));
                }
                for m in muts {
                    out.push(u8::from(*m));
                }
                out.extend_from_slice(&self.type_digest(*ret));
                self.row_bytes(&mut out, row);
            }
            BcType::Var(i) => {
                out.push(10);
                out.extend_from_slice(&i.to_le_bytes());
            }
            BcType::StringBuilder => out.push(11),
            BcType::ByteBuffer => out.push(12),
            BcType::Fault => out.push(13),
            BcType::Request => out.push(14),
            BcType::PolicyTable => out.push(15),
            BcType::EmptyVm => out.push(16),
            BcType::Vm(t) => {
                out.push(17);
                out.extend_from_slice(&self.type_digest(*t));
            }
            BcType::PendingCall(a, r) => {
                out.push(18);
                out.extend_from_slice(&self.type_digest(*a));
                out.extend_from_slice(&self.type_digest(*r));
            }
            BcType::Op(op, f) => {
                out.push(19);
                out.extend_from_slice(&op.to_le_bytes());
                out.extend_from_slice(&self.type_digest(*f));
            }
        }
        sha256(&out)
    }

    /// The digest bytes of one type application.
    fn app_digest_of(&self, a: u32) -> [u8; 32] {
        let app = &self.module.apps[a as usize];
        let mut out = Vec::new();
        out.extend_from_slice(TAG_APP);
        out.extend_from_slice(&(app.types.len() as u32).to_le_bytes());
        for t in &app.types {
            out.extend_from_slice(&self.type_digest(*t));
        }
        out.extend_from_slice(&(app.rows.len() as u32).to_le_bytes());
        for row in &app.rows {
            self.row_bytes(&mut out, row);
        }
        sha256(&out)
    }

    /// The canonical member bytes of one class.
    ///
    /// The class name is part of the identity: a class is a nominal
    /// type (specification 5.3, 8.6). Two classes with different names
    /// are different definitions, whatever their shape. Function
    /// identity stays anonymous, so a function rename still moves no
    /// definition hash.
    fn class_bytes(&self, c: u32) -> Vec<u8> {
        let class = &self.module.classes[c as usize];
        let mut out = Vec::new();
        write_str(&mut out, &class.name);
        out.push(match class.kind {
            BcClassKind::Normal => 0,
            BcClassKind::Abstract => 1,
            BcClassKind::Case => 2,
        });
        out.extend_from_slice(&class.type_params.to_le_bytes());
        match class.parent() {
            None => out.push(0xff),
            Some(p) => {
                out.push(0xfe);
                write_ident(&mut out, &self.class_ident(p));
            }
        }
        out.extend_from_slice(&(class.fields.len() as u32).to_le_bytes());
        for (name, ty) in &class.fields {
            write_str(&mut out, name);
            out.extend_from_slice(&self.type_digest(*ty));
        }
        out.extend_from_slice(&(class.methods.len() as u32).to_le_bytes());
        for (sel, func) in &class.methods {
            write_str(&mut out, &self.module.selectors[*sel as usize]);
            write_ident(&mut out, &self.func_ident(*func));
        }
        // An abstract enum parent carries its closed arm set, in arm
        // order.
        if class.kind == BcClassKind::Abstract {
            let arms = &self.graph.arms[c as usize];
            out.extend_from_slice(&(arms.len() as u32).to_le_bytes());
            for arm in arms {
                write_ident(&mut out, &self.class_ident(*arm));
            }
        }
        out
    }

    /// The canonical member bytes of one function: the signature, the
    /// row, the locals, and every instruction with substituted
    /// operands. The function name is excluded.
    fn func_bytes(&self, f: u32) -> Vec<u8> {
        let func = &self.module.funcs[f as usize];
        let mut out = Vec::new();
        out.extend_from_slice(&func.type_params.to_le_bytes());
        out.extend_from_slice(&func.effect_params.to_le_bytes());
        out.extend_from_slice(&(func.params.len() as u32).to_le_bytes());
        for p in &func.params {
            out.extend_from_slice(&self.type_digest(*p));
        }
        for m in &func.param_muts {
            out.push(u8::from(*m));
        }
        out.extend_from_slice(&self.type_digest(func.ret));
        self.row_bytes(&mut out, &func.row);
        out.extend_from_slice(&(func.captures.len() as u32).to_le_bytes());
        for c in &func.captures {
            out.extend_from_slice(&self.type_digest(*c));
        }
        out.extend_from_slice(&(func.local_types.len() as u32).to_le_bytes());
        for t in &func.local_types {
            out.extend_from_slice(&self.type_digest(*t));
        }
        out.extend_from_slice(&(func.blocks.len() as u32).to_le_bytes());
        for block in &func.blocks {
            out.extend_from_slice(&(block.len() as u32).to_le_bytes());
            for instr in block {
                self.instr_bytes(&mut out, instr);
            }
        }
        out
    }

    /// The canonical encoding of one instruction. The match is
    /// exhaustive without a wildcard arm, so a future instruction
    /// with a new index operand fails to compile until its canonical
    /// form is decided. Canonical forms per operand kind:
    ///
    /// - function/class index: identity reference (hash or ordinal);
    /// - closure body: 0x02 plus the body digest;
    /// - string index: inline content;
    /// - type index: structural digest;
    /// - application index: application digest;
    /// - selector index: selector name;
    /// - local slot, capture, field, tuple position, block target,
    ///   argument count, operation slot, table-edit operands: raw
    ///   little-endian value (function-local or manifest-dense, both
    ///   order-stable).
    fn instr_bytes(&self, out: &mut Vec<u8>, instr: &Instr) {
        let u = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
        match instr {
            Instr::ConstUnit => out.push(0x00),
            Instr::ConstBool(v) => {
                out.push(0x01);
                out.push(u8::from(*v));
            }
            Instr::ConstInt(v) => {
                out.push(0x02);
                out.extend_from_slice(&v.to_le_bytes());
            }
            Instr::ConstStr(idx) => {
                out.push(0x03);
                write_str(out, &self.module.strings[*idx as usize]);
            }
            Instr::LoadLocal(slot) => {
                out.push(0x04);
                u(out, *slot);
            }
            Instr::StoreLocal(slot) => {
                out.push(0x05);
                u(out, *slot);
            }
            Instr::Pop => out.push(0x06),
            Instr::Add => out.push(0x10),
            Instr::Sub => out.push(0x11),
            Instr::Mul => out.push(0x12),
            Instr::Div => out.push(0x13),
            Instr::Rem => out.push(0x14),
            Instr::Neg => out.push(0x15),
            Instr::Not => out.push(0x16),
            Instr::LtInt => out.push(0x20),
            Instr::LeInt => out.push(0x21),
            Instr::GtInt => out.push(0x22),
            Instr::GeInt => out.push(0x23),
            Instr::EqInt => out.push(0x24),
            Instr::NeInt => out.push(0x25),
            Instr::EqBool => out.push(0x26),
            Instr::NeBool => out.push(0x27),
            Instr::EqStr => out.push(0x28),
            Instr::NeStr => out.push(0x29),
            Instr::EqRef => out.push(0x2a),
            Instr::NeRef => out.push(0x2b),
            Instr::Call(f) => {
                out.push(0x30);
                write_ident(out, &self.func_ident(*f));
            }
            Instr::CallG { func, app } => {
                out.push(0x60);
                write_ident(out, &self.func_ident(*func));
                out.extend_from_slice(&self.app_digest(*app));
            }
            Instr::CallVirtual { selector, argc } => {
                out.push(0x40);
                write_str(out, &self.module.selectors[*selector as usize]);
                u(out, *argc);
            }
            Instr::CallVirtualG {
                selector,
                argc,
                app,
            } => {
                out.push(0x61);
                write_str(out, &self.module.selectors[*selector as usize]);
                u(out, *argc);
                out.extend_from_slice(&self.app_digest(*app));
            }
            Instr::CallValue { argc } => {
                out.push(0x41);
                u(out, *argc);
            }
            Instr::MakeClosure { func, captures } => {
                out.push(0x42);
                if self.graph.closure_body[*func as usize] {
                    if self.on_path.contains(func) {
                        // A hand-built `MakeClosure` cycle: a
                        // deterministic marker instead of recursion.
                        out.push(0x03);
                        let position = self
                            .closure_list
                            .iter()
                            .position(|c| c == func)
                            .unwrap_or(usize::MAX) as u32;
                        u(out, position);
                    } else {
                        out.push(0x02);
                        out.extend_from_slice(&self.body_digest(*func));
                    }
                } else {
                    write_ident(out, &self.func_ident(*func));
                }
                u(out, *captures);
            }
            Instr::LoadCapture(idx) => {
                out.push(0x43);
                u(out, *idx);
            }
            Instr::New(class) => {
                out.push(0x44);
                write_ident(out, &self.class_ident(*class));
            }
            Instr::NewG { class, app } => {
                out.push(0x62);
                write_ident(out, &self.class_ident(*class));
                out.extend_from_slice(&self.app_digest(*app));
            }
            Instr::LoadField(field) => {
                out.push(0x45);
                u(out, *field);
            }
            Instr::StoreField(field) => {
                out.push(0x46);
                u(out, *field);
            }
            Instr::TupleNew { ty, count } => {
                out.push(0x63);
                out.extend_from_slice(&self.type_digest(*ty));
                u(out, *count);
            }
            Instr::TupleGet(index) => {
                out.push(0x64);
                u(out, *index);
            }
            Instr::IsType(ty) => {
                out.push(0x65);
                out.extend_from_slice(&self.type_digest(*ty));
            }
            Instr::CastType(ty) => {
                out.push(0x66);
                out.extend_from_slice(&self.type_digest(*ty));
            }
            Instr::ListNew { ty, count } => {
                out.push(0x47);
                out.extend_from_slice(&self.type_digest(*ty));
                u(out, *count);
            }
            Instr::ListLen => out.push(0x48),
            Instr::ListAt => out.push(0x49),
            Instr::ListPush => out.push(0x4a),
            Instr::MapNew { ty, count } => {
                out.push(0x4b);
                out.extend_from_slice(&self.type_digest(*ty));
                u(out, *count);
            }
            Instr::MapLen => out.push(0x4c),
            Instr::MapHas => out.push(0x4d),
            Instr::MapAt => out.push(0x4e),
            Instr::MapPut => out.push(0x4f),
            Instr::SbNew => out.push(0x50),
            Instr::SbAppendStr => out.push(0x51),
            Instr::SbAppendInt => out.push(0x52),
            Instr::SbAppendBool => out.push(0x53),
            Instr::SbBuild => out.push(0x54),
            Instr::BbNew => out.push(0x55),
            Instr::BbAppend => out.push(0x56),
            Instr::BbLen => out.push(0x57),
            Instr::BbBuild => out.push(0x58),
            Instr::Freeze => out.push(0x59),
            Instr::Jump(b) => {
                out.push(0x31);
                u(out, *b);
            }
            Instr::JumpIfFalse(b) => {
                out.push(0x32);
                u(out, *b);
            }
            Instr::JumpIfTrue(b) => {
                out.push(0x33);
                u(out, *b);
            }
            Instr::Return => out.push(0x34),
            Instr::Perform { op, argc } => {
                out.push(0x70);
                u(out, *op);
                u(out, *argc);
            }
            Instr::PerformValue { argc } => {
                out.push(0x71);
                u(out, *argc);
            }
            Instr::OpConst(op) => {
                out.push(0x72);
                u(out, *op);
            }
            Instr::TableEdit { action, kind, slot } => {
                out.push(0x73);
                u(out, *action);
                u(out, *kind);
                u(out, *slot);
            }
            Instr::AsCall(op) => {
                out.push(0x74);
                u(out, *op);
            }
            Instr::CallArgs => out.push(0x75),
            Instr::FaultCode => out.push(0x76),
            Instr::Unreachable => out.push(0x77),
        }
    }
}

// ----------------------------------------------------------------
// Component processing and the module hash.
// ----------------------------------------------------------------

/// Compute the closure body digests of the given in-component closure
/// functions, iteratively over the `MakeClosure` nesting with an
/// explicit stack. A hand-built `MakeClosure` cycle gets a marker
/// instead of unbounded recursion.
#[allow(clippy::too_many_arguments)]
fn closure_body_digests(
    module: &Module,
    graph: &Graph,
    state: &HashState,
    comp: Option<u32>,
    comp_of: &[u32],
    ordinals: &HashMap<u32, u32>,
    type_intra: &HashMap<u32, [u8; 32]>,
    app_intra: &HashMap<u32, [u8; 32]>,
    closures: &[u32],
) -> HashMap<u32, [u8; 32]> {
    let mut done: HashMap<u32, [u8; 32]> = HashMap::new();
    let mut visiting: Vec<u32> = Vec::new();
    for &start in closures {
        if done.contains_key(&start) {
            continue;
        }
        // Iterative post-order over same-component nested closures.
        let mut stack: Vec<(u32, bool)> = vec![(start, false)];
        while let Some((f, expanded)) = stack.pop() {
            if done.contains_key(&f) {
                continue;
            }
            if expanded {
                // `f` stays on the path while its own body serializes.
                // A body that makes a closure of itself must reach the
                // cycle marker, not a digest that is still in progress.
                let digest = {
                    let resolver = Resolver {
                        module,
                        graph,
                        state,
                        comp,
                        comp_of,
                        ordinals,
                        type_intra,
                        app_intra,
                        body_intra: &done,
                        on_path: &visiting,
                        closure_list: closures,
                    };
                    let mut bytes = Vec::new();
                    bytes.extend_from_slice(TAG_BODY);
                    bytes.extend_from_slice(&resolver.func_bytes(f));
                    sha256(&bytes)
                };
                visiting.retain(|x| *x != f);
                done.insert(f, digest);
                continue;
            }
            visiting.push(f);
            stack.push((f, true));
            // Push unresolved same-component nested closure targets.
            let func = &module.funcs[f as usize];
            for block in &func.blocks {
                for instr in block {
                    if let Instr::MakeClosure { func: target, .. } = instr {
                        let is_closure = graph.closure_body[*target as usize];
                        let same = match comp {
                            Some(c) => comp_of[graph.space.func_node(*target) as usize] == c,
                            None => state.body_final[*target as usize].is_none(),
                        };
                        if is_closure
                            && same
                            && !visiting.contains(target)
                            && !done.contains_key(target)
                        {
                            stack.push((*target, false));
                        }
                    }
                }
            }
        }
    }
    done
}

/// Compute the identity of one module: definition hashes for every
/// class and function plus the module semantic hash. The module may
/// be unverified; every index is validated first.
pub fn module_identity(module: &Module) -> Result<ModuleIdentity, IdentityError> {
    preflight(module)?;
    let graph = Graph::build(module);
    let s = graph.space;
    let (comps, comp_of) = tarjan(&graph);
    let mut state = HashState {
        class_hash: vec![None; s.classes],
        func_hash: vec![None; s.funcs],
        type_final: vec![None; s.types],
        app_final: vec![None; s.apps],
        body_final: vec![None; s.funcs],
        comp_hash: vec![[0u8; 32]; s.total()],
    };
    let manifest = lm_abi::manifest_digest();
    // The empty overlays of the final all-hash view.
    let empty_ordinals: HashMap<u32, u32> = HashMap::new();
    let empty_digests: HashMap<u32, [u8; 32]> = HashMap::new();
    // An imported definition takes the pinned interface hash as its
    // identity. It references nothing, so it is a singleton component
    // and every hash schedule reaches it before any user of it.
    for import in &module.imports {
        let node = if import.kind.is_func() {
            state.func_hash[import.def as usize] = Some(import.hash);
            s.func_node(import.def)
        } else {
            state.class_hash[import.def as usize] = Some(import.hash);
            s.class_node(import.def)
        };
        state.comp_hash[node as usize] = import.hash;
    }
    for (comp_idx, comp) in comps.iter().enumerate() {
        let comp_idx = comp_idx as u32;
        // An imported definition carries its pin, so the component
        // encoding never runs for it.
        if comp.len() == 1 {
            let node = comp[0] as usize;
            if node < s.classes && graph.extern_class[node] {
                continue;
            }
            if (s.classes..s.classes + s.funcs).contains(&node)
                && graph.extern_func[node - s.classes]
            {
                continue;
            }
        }
        // Partition the component nodes, each list ascending.
        let mut classes: Vec<u32> = Vec::new();
        let mut member_funcs: Vec<u32> = Vec::new();
        let mut closure_funcs: Vec<u32> = Vec::new();
        let mut types: Vec<u32> = Vec::new();
        let mut apps: Vec<u32> = Vec::new();
        for &node in comp {
            let n = node as usize;
            if n < s.classes {
                classes.push(node);
            } else if n < s.classes + s.funcs {
                let f = (n - s.classes) as u32;
                if graph.closure_body[f as usize] {
                    closure_funcs.push(f);
                } else {
                    member_funcs.push(f);
                }
            } else if n < s.classes + s.funcs + s.types {
                types.push((n - s.classes - s.funcs) as u32);
            } else {
                apps.push((n - s.classes - s.funcs - s.types) as u32);
            }
        }
        classes.sort_unstable();
        member_funcs.sort_unstable();
        closure_funcs.sort_unstable();
        types.sort_unstable();
        apps.sort_unstable();
        // Canonical member order: by exported name, then kind, then
        // the stable generated index.
        let mut members: Vec<(u8, u32, u32)> = Vec::new();
        for &c in &classes {
            members.push((0, c, s.class_node(c)));
        }
        for &f in &member_funcs {
            members.push((1, f, s.func_node(f)));
        }
        members.sort_by(|a, b| {
            let name_a = member_name(module, a.0, a.1);
            let name_b = member_name(module, b.0, b.1);
            name_a.cmp(name_b).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1))
        });
        let ordinals: HashMap<u32, u32> = members
            .iter()
            .enumerate()
            .map(|(i, (_, _, node))| (*node, i as u32))
            .collect();
        // Intra-component digests: types in ascending index (their
        // references point at earlier entries), then applications,
        // then closure bodies.
        let mut type_intra: HashMap<u32, [u8; 32]> = HashMap::new();
        let mut app_intra: HashMap<u32, [u8; 32]> = HashMap::new();
        for &t in &types {
            let digest = {
                let resolver = Resolver {
                    module,
                    graph: &graph,
                    state: &state,
                    comp: Some(comp_idx),
                    comp_of: &comp_of,
                    ordinals: &ordinals,
                    type_intra: &type_intra,
                    app_intra: &app_intra,
                    body_intra: &empty_digests,
                    on_path: &[],
                    closure_list: &closure_funcs,
                };
                resolver.type_digest_of(&module.types[t as usize])
            };
            type_intra.insert(t, digest);
        }
        for &a in &apps {
            let digest = {
                let resolver = Resolver {
                    module,
                    graph: &graph,
                    state: &state,
                    comp: Some(comp_idx),
                    comp_of: &comp_of,
                    ordinals: &ordinals,
                    type_intra: &type_intra,
                    app_intra: &app_intra,
                    body_intra: &empty_digests,
                    on_path: &[],
                    closure_list: &closure_funcs,
                };
                resolver.app_digest_of(a)
            };
            app_intra.insert(a, digest);
        }
        let body_intra = closure_body_digests(
            module,
            &graph,
            &state,
            Some(comp_idx),
            &comp_of,
            &ordinals,
            &type_intra,
            &app_intra,
            &closure_funcs,
        );
        // Serialize the members in canonical order and hash the
        // component.
        let mut comp_bytes = Vec::new();
        comp_bytes.extend_from_slice(TAG_COMPONENT);
        comp_bytes.extend_from_slice(&COMPILER_ABI_VERSION.to_le_bytes());
        comp_bytes.extend_from_slice(&manifest);
        comp_bytes.extend_from_slice(&(members.len() as u32).to_le_bytes());
        {
            let resolver = Resolver {
                module,
                graph: &graph,
                state: &state,
                comp: Some(comp_idx),
                comp_of: &comp_of,
                ordinals: &ordinals,
                type_intra: &type_intra,
                app_intra: &app_intra,
                body_intra: &body_intra,
                on_path: &[],
                closure_list: &closure_funcs,
            };
            for (kind, idx, _) in &members {
                let bytes = match kind {
                    0 => resolver.class_bytes(*idx),
                    _ => resolver.func_bytes(*idx),
                };
                comp_bytes.push(*kind);
                comp_bytes.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
                comp_bytes.extend_from_slice(&bytes);
            }
        }
        let comp_hash = sha256(&comp_bytes);
        for &node in comp {
            state.comp_hash[node as usize] = comp_hash;
        }
        for (ordinal, (kind, idx, _)) in members.iter().enumerate() {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(TAG_MEMBER);
            bytes.extend_from_slice(&comp_hash);
            bytes.extend_from_slice(&(ordinal as u32).to_le_bytes());
            let hash = sha256(&bytes);
            match kind {
                0 => state.class_hash[*idx as usize] = Some(hash),
                _ => state.func_hash[*idx as usize] = Some(hash),
            }
        }
        // Final digests for this component's types, applications, and
        // closure bodies: every reference by final hash. Later
        // components read these.
        for &t in &types {
            let digest = {
                let resolver = Resolver {
                    module,
                    graph: &graph,
                    state: &state,
                    comp: None,
                    comp_of: &comp_of,
                    ordinals: &empty_ordinals,
                    type_intra: &empty_digests,
                    app_intra: &empty_digests,
                    body_intra: &empty_digests,
                    on_path: &[],
                    closure_list: &closure_funcs,
                };
                resolver.type_digest_of(&module.types[t as usize])
            };
            state.type_final[t as usize] = Some(digest);
        }
        for &a in &apps {
            let digest = {
                let resolver = Resolver {
                    module,
                    graph: &graph,
                    state: &state,
                    comp: None,
                    comp_of: &comp_of,
                    ordinals: &empty_ordinals,
                    type_intra: &empty_digests,
                    app_intra: &empty_digests,
                    body_intra: &empty_digests,
                    on_path: &[],
                    closure_list: &closure_funcs,
                };
                resolver.app_digest_of(a)
            };
            state.app_final[a as usize] = Some(digest);
        }
        let body_final = closure_body_digests(
            module,
            &graph,
            &state,
            None,
            &comp_of,
            &empty_ordinals,
            &empty_digests,
            &empty_digests,
            &closure_funcs,
        );
        for (f, digest) in body_final {
            state.body_final[f as usize] = Some(digest);
        }
    }
    // Closure definition hashes: parent identity plus occurrence.
    fill_closure_hashes(module, &graph, &mut state);
    let class_hashes: Vec<[u8; 32]> = state
        .class_hash
        .iter()
        .map(|h| h.expect("every class hash is scheduled"))
        .collect();
    let func_hashes: Vec<[u8; 32]> = state
        .func_hash
        .iter()
        .map(|h| h.expect("every function hash is scheduled"))
        .collect();
    // The module semantic hash: format version, compiler ABI, the
    // operation manifest, the explicit empty import set, the export
    // table (name to definition hash, name-sorted), and the entry
    // definition hash.
    let mut out = Vec::new();
    out.extend_from_slice(TAG_MODULE);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&COMPILER_ABI_VERSION.to_le_bytes());
    out.extend_from_slice(&manifest);
    // The import set, sorted by module path, then name, then kind.
    // The sort keeps the hash independent of the order in which the
    // compiler discovered the slots.
    let mut imports: Vec<(&str, &str, u8, [u8; 32])> = module
        .imports
        .iter()
        .map(|i| (i.module.as_str(), i.name.as_str(), i.kind.tag(), i.hash))
        .collect();
    imports.sort();
    out.extend_from_slice(&(imports.len() as u32).to_le_bytes());
    for (path, name, kind, hash) in &imports {
        write_str(&mut out, path);
        write_str(&mut out, name);
        out.push(*kind);
        out.extend_from_slice(hash);
    }
    let mut exports: Vec<(u8, &str, [u8; 32])> = Vec::new();
    for (c, class) in module.classes.iter().enumerate() {
        exports.push((0, &class.name, class_hashes[c]));
    }
    for (f, func) in module.funcs.iter().enumerate() {
        if !graph.closure_body[f] {
            exports.push((1, &func.name, func_hashes[f]));
        }
    }
    exports.sort_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(&b.0)).then(a.2.cmp(&b.2)));
    out.extend_from_slice(&(exports.len() as u32).to_le_bytes());
    for (kind, name, hash) in &exports {
        out.push(*kind);
        write_str(&mut out, name);
        out.extend_from_slice(hash);
    }
    out.extend_from_slice(&func_hashes[module.entry as usize]);
    let semantic_hash = sha256(&out);
    Ok(ModuleIdentity {
        class_hashes,
        func_hashes,
        semantic_hash,
    })
}

/// The canonical name of one component member.
fn member_name(module: &Module, kind: u8, idx: u32) -> &str {
    if kind == 0 {
        &module.classes[idx as usize].name
    } else {
        &module.funcs[idx as usize].name
    }
}

/// Give every closure body its definition hash: domain-separated from
/// the parent definition hash and the occurrence index of its first
/// `MakeClosure` site. A hand-built cycle falls back to the component
/// hash.
fn fill_closure_hashes(module: &Module, graph: &Graph, state: &mut HashState) {
    // First reference per closure: (parent function, occurrence).
    let mut first_ref: Vec<Option<(u32, u32)>> = vec![None; graph.space.funcs];
    for (fidx, func) in module.funcs.iter().enumerate() {
        let mut occurrence = 0u32;
        for block in &func.blocks {
            for instr in block {
                if let Instr::MakeClosure { func: target, .. } = instr {
                    if graph.closure_body[*target as usize] && first_ref[*target as usize].is_none()
                    {
                        first_ref[*target as usize] = Some((fidx as u32, occurrence));
                    }
                    occurrence += 1;
                }
            }
        }
    }
    // Resolve chains with an explicit path walk: a closure hash needs
    // its parent hash first, and parents may themselves be closures.
    // Each function resolves once, so the walk is linear.
    let total = graph.space.funcs;
    let mut path: Vec<u32> = Vec::new();
    for start in 0..total as u32 {
        if !graph.closure_body[start as usize] || state.func_hash[start as usize].is_some() {
            continue;
        }
        path.clear();
        let mut at = start;
        // Walk parents until a resolved ancestor or a cycle.
        loop {
            if state.func_hash[at as usize].is_some() || path.contains(&at) {
                break;
            }
            path.push(at);
            match first_ref[at as usize] {
                Some((parent, _)) => at = parent,
                None => break,
            }
        }
        // Resolve backwards along the path where possible.
        for &f in path.iter().rev() {
            let Some((parent, occurrence)) = first_ref[f as usize] else {
                continue;
            };
            let Some(parent_hash) = state.func_hash[parent as usize] else {
                continue;
            };
            let mut bytes = Vec::new();
            bytes.extend_from_slice(TAG_CLOSURE);
            bytes.extend_from_slice(&parent_hash);
            bytes.extend_from_slice(&occurrence.to_le_bytes());
            state.func_hash[f as usize] = Some(sha256(&bytes));
        }
    }
    // Hand-built cycles and unreferenced closure flags: fall back to
    // the component hash plus the function index inside it.
    for f in 0..total {
        if state.func_hash[f].is_none() {
            let node = graph.space.func_node(f as u32);
            let mut bytes = Vec::new();
            bytes.extend_from_slice(TAG_CLOSURE_CYCLIC);
            bytes.extend_from_slice(&state.comp_hash[node as usize]);
            bytes.extend_from_slice(&(f as u32).to_le_bytes());
            state.func_hash[f] = Some(sha256(&bytes));
        }
    }
}
