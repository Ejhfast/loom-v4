//! Independent bytecode verifier.
//!
//! The verifier receives a decoded module and rejects it unless every
//! table and every function is well formed. It validates the type,
//! selector, and class tables first. It then reconstructs the
//! operand-stack types and the local-slot types at each block entry
//! with a worklist, and it checks jumps, calls, field access, closure
//! creation, and collection operations. It shares no code with the
//! source checker.

use lm_bytecode::{BcType, Func, Instr, Module};
use std::collections::{HashMap, VecDeque};
use std::fmt;

/// The largest operand-stack depth the verifier accepts for one function.
const MAX_STATIC_STACK: usize = 4096;

/// Canonical type-table indices for the primitive types. Every module
/// must begin its type table with these entries in this order.
pub const TY_UNIT: u32 = 0;
pub const TY_BOOL: u32 = 1;
pub const TY_INT: u32 = 2;
pub const TY_STR: u32 = 3;

/// A verification failure. The message names the exact position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError {
    pub func: u32,
    pub message: String,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "function {}: {}", self.func, self.message)
    }
}

fn err(func: u32, message: impl Into<String>) -> VerifyError {
    VerifyError {
        func,
        message: message.into(),
    }
}

/// The abstract state at one program point. Types are type-table
/// indices. `None` marks a local slot without a known value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    locals: Vec<Option<u32>>,
    stack: Vec<u32>,
}

/// Shared lookup context for one module.
struct Ctx<'m> {
    module: &'m Module,
    /// Class index to the type-table index of `Class(index)`.
    class_ty: Vec<Option<u32>>,
    /// Structural type to its table index.
    type_index: HashMap<&'m BcType, u32>,
}

impl<'m> Ctx<'m> {
    fn ty(&self, idx: u32) -> &'m BcType {
        &self.module.types[idx as usize]
    }

    /// Return true when class `child` equals `ancestor` or inherits it.
    fn class_extends(&self, mut child: u32, ancestor: u32) -> bool {
        loop {
            if child == ancestor {
                return true;
            }
            match self.module.classes[child as usize].parent() {
                Some(p) => child = p,
                None => return false,
            }
        }
    }

    /// Return true when a value of type `found` is valid where the
    /// code expects type `expected`.
    fn is_subtype(&self, found: u32, expected: u32) -> bool {
        if found == expected {
            return true;
        }
        match (self.ty(found), self.ty(expected)) {
            (BcType::Class(a), BcType::Class(b)) => self.class_extends(*a, *b),
            _ => false,
        }
    }

    /// Join two types at a control-flow merge. Classes join at their
    /// nearest common ancestor. Unrelated types have no join.
    fn join(&self, a: u32, b: u32) -> Option<u32> {
        if self.is_subtype(a, b) {
            return Some(b);
        }
        if self.is_subtype(b, a) {
            return Some(a);
        }
        if let (BcType::Class(ca), BcType::Class(cb)) = (self.ty(a), self.ty(b)) {
            let mut anc = Some(*ca);
            while let Some(c) = anc {
                if self.class_extends(*cb, c) {
                    return self.class_ty[c as usize];
                }
                anc = self.module.classes[c as usize].parent();
            }
        }
        None
    }

    /// Resolve a selector on a class, walking the ancestor chain.
    fn find_method(&self, mut class: u32, selector: u32) -> Option<u32> {
        loop {
            let c = &self.module.classes[class as usize];
            for (sel, func) in &c.methods {
                if *sel == selector {
                    return Some(*func);
                }
            }
            match c.parent() {
                Some(p) => class = p,
                None => return None,
            }
        }
    }

    /// Return true when the type is a heap object type.
    fn is_heap(&self, idx: u32) -> bool {
        matches!(
            self.ty(idx),
            BcType::Str
                | BcType::Class(_)
                | BcType::List(_)
                | BcType::Map(_, _)
                | BcType::Fn(_, _)
                | BcType::StringBuilder
                | BcType::ByteBuffer
        )
    }
}

/// Verify a full module. Every table and every function must pass.
pub fn verify_module(module: &Module) -> Result<(), VerifyError> {
    let ctx = verify_tables(module)?;
    let entry = module.entry as usize;
    if entry >= module.funcs.len() {
        return Err(err(
            module.entry,
            format!(
                "entry index {} is not inside the function table of length {}",
                module.entry,
                module.funcs.len()
            ),
        ));
    }
    if !module.funcs[entry].params.is_empty() {
        return Err(err(
            module.entry,
            "the entry function must not have parameters",
        ));
    }
    if !module.funcs[entry].captures.is_empty() {
        return Err(err(
            module.entry,
            "the entry function must not have captures",
        ));
    }
    for (idx, func) in module.funcs.iter().enumerate() {
        verify_func(&ctx, func, idx as u32)?;
    }
    Ok(())
}

/// Validate the type, selector, class, and function tables.
fn verify_tables(module: &Module) -> Result<Ctx<'_>, VerifyError> {
    let terr = |message: String| VerifyError {
        func: u32::MAX,
        message,
    };
    // The type table must start with the canonical primitive prefix.
    let prefix = [BcType::Unit, BcType::Bool, BcType::Int, BcType::Str];
    if module.types.len() < prefix.len() || module.types[..prefix.len()] != prefix[..] {
        return Err(terr(
            "the type table does not start with Unit, Bool, Int, String".to_string(),
        ));
    }
    let mut type_index: HashMap<&BcType, u32> = HashMap::new();
    for (idx, ty) in module.types.iter().enumerate() {
        if type_index.insert(ty, idx as u32).is_some() {
            return Err(terr(format!("type {idx} duplicates an earlier type entry")));
        }
        let check_ref = |child: u32| -> Result<(), VerifyError> {
            if child as usize >= idx {
                return Err(terr(format!(
                    "type {idx} references type {child}, which is not an earlier entry"
                )));
            }
            Ok(())
        };
        match ty {
            BcType::Unit | BcType::Bool | BcType::Int | BcType::Str => {}
            BcType::StringBuilder | BcType::ByteBuffer => {}
            BcType::Class(c) => {
                if *c as usize >= module.classes.len() {
                    return Err(terr(format!(
                        "type {idx} names class {c}, which does not exist"
                    )));
                }
            }
            BcType::List(e) => check_ref(*e)?,
            BcType::Map(k, v) => {
                check_ref(*k)?;
                check_ref(*v)?;
                if !matches!(
                    module.types[*k as usize],
                    BcType::Bool | BcType::Int | BcType::Str
                ) {
                    return Err(terr(format!(
                        "type {idx} is a map with a key type that is not Bool, Int, or String"
                    )));
                }
            }
            BcType::Fn(params, ret) => {
                for p in params {
                    check_ref(*p)?;
                }
                check_ref(*ret)?;
            }
        }
    }
    // Class types by class index.
    let mut class_ty = vec![None; module.classes.len()];
    for (idx, ty) in module.types.iter().enumerate() {
        if let BcType::Class(c) = ty {
            class_ty[*c as usize] = Some(idx as u32);
        }
    }
    let ctx = Ctx {
        module,
        class_ty,
        type_index,
    };
    // Validate classes.
    for (cidx, class) in module.classes.iter().enumerate() {
        let cerr = |message: String| terr(format!("class {cidx}: {message}"));
        if let Some(p) = class.parent() {
            if p as usize >= cidx {
                return Err(cerr(format!("parent {p} is not an earlier class entry")));
            }
            // The field layout must extend the parent layout exactly.
            let parent = &module.classes[p as usize];
            if class.fields.len() < parent.fields.len()
                || class.fields[..parent.fields.len()] != parent.fields[..]
            {
                return Err(cerr(
                    "the field layout does not extend the parent layout".to_string(),
                ));
            }
        }
        for (fname, fty) in &class.fields {
            if *fty as usize >= module.types.len() {
                return Err(cerr(format!("field `{fname}` has an invalid type index")));
            }
        }
        let own_ty = ctx.class_ty[cidx];
        let mut seen = Vec::new();
        for (sel, func) in &class.methods {
            if *sel as usize >= module.selectors.len() {
                return Err(cerr(format!("selector {sel} does not exist")));
            }
            if seen.contains(sel) {
                return Err(cerr(format!("selector {sel} appears twice")));
            }
            seen.push(*sel);
            if *func as usize >= module.funcs.len() {
                return Err(cerr(format!("method function {func} does not exist")));
            }
            let f = &module.funcs[*func as usize];
            if !f.captures.is_empty() {
                return Err(cerr("a method function must not have captures".to_string()));
            }
            let self_ok = match (f.params.first(), own_ty) {
                (Some(p0), Some(t)) => *p0 == t,
                _ => false,
            };
            if !self_ok {
                return Err(cerr(format!(
                    "method function {func} does not receive this class as `self`"
                )));
            }
            // Override compatibility with the nearest ancestor method.
            if let Some(parent) = class.parent() {
                if let Some(base_func) = ctx.find_method(parent, *sel) {
                    let base = &module.funcs[base_func as usize];
                    if base.params.len() != f.params.len() || base.params[1..] != f.params[1..] {
                        return Err(cerr(format!(
                            "override of selector {sel} changes the parameter types"
                        )));
                    }
                    if !ctx.is_subtype(f.ret, base.ret) {
                        return Err(cerr(format!(
                            "override of selector {sel} widens the result type"
                        )));
                    }
                }
            }
        }
    }
    // Validate function signatures.
    for (fidx, func) in module.funcs.iter().enumerate() {
        for t in func
            .params
            .iter()
            .chain(func.captures.iter())
            .chain([&func.ret])
        {
            if *t as usize >= module.types.len() {
                return Err(err(
                    fidx as u32,
                    "the signature references an invalid type index",
                ));
            }
        }
    }
    Ok(ctx)
}

fn verify_func(ctx: &Ctx<'_>, func: &Func, fidx: u32) -> Result<(), VerifyError> {
    let module = ctx.module;
    if func.params.len() > func.local_count as usize {
        return Err(err(fidx, "more parameters than local slots"));
    }
    if func.blocks.is_empty() {
        return Err(err(fidx, "the function has no blocks"));
    }
    // Structural pass: every block ends with a terminator and every
    // operand index is inside its table.
    for (bidx, block) in func.blocks.iter().enumerate() {
        match block.last() {
            Some(last) if last.is_terminator() => {}
            _ => {
                return Err(err(
                    fidx,
                    format!("block {bidx} does not end with a terminator"),
                ));
            }
        }
        for (iidx, instr) in block.iter().enumerate() {
            if instr.is_terminator() && iidx + 1 != block.len() {
                return Err(err(
                    fidx,
                    format!("block {bidx} has a terminator before its end"),
                ));
            }
            let at = |what: &str| format!("block {bidx}, instruction {iidx}: {what}");
            match instr {
                Instr::ConstStr(idx) => {
                    if *idx as usize >= module.strings.len() {
                        return Err(err(fidx, at("string index out of range")));
                    }
                }
                Instr::LoadLocal(slot) | Instr::StoreLocal(slot) => {
                    if *slot >= func.local_count {
                        return Err(err(fidx, at("local slot out of range")));
                    }
                }
                Instr::Call(callee) => {
                    if *callee as usize >= module.funcs.len() {
                        return Err(err(fidx, at("call target out of range")));
                    }
                    if !module.funcs[*callee as usize].captures.is_empty() {
                        return Err(err(fidx, at("direct call to a function with captures")));
                    }
                }
                Instr::CallVirtual { selector, .. } => {
                    if *selector as usize >= module.selectors.len() {
                        return Err(err(fidx, at("selector index out of range")));
                    }
                }
                Instr::MakeClosure { func: f, captures } => {
                    if *f as usize >= module.funcs.len() {
                        return Err(err(fidx, at("closure function out of range")));
                    }
                    if module.funcs[*f as usize].captures.len() != *captures as usize {
                        return Err(err(fidx, at("closure capture count mismatch")));
                    }
                }
                Instr::LoadCapture(idx) => {
                    if *idx as usize >= func.captures.len() {
                        return Err(err(fidx, at("capture index out of range")));
                    }
                }
                Instr::New(class) => {
                    if *class as usize >= module.classes.len() {
                        return Err(err(fidx, at("class index out of range")));
                    }
                }
                Instr::ListNew { ty, .. } | Instr::MapNew { ty, .. } => {
                    if *ty as usize >= module.types.len() {
                        return Err(err(fidx, at("type index out of range")));
                    }
                }
                Instr::Jump(target) | Instr::JumpIfFalse(target) | Instr::JumpIfTrue(target) => {
                    if *target as usize >= func.blocks.len() {
                        return Err(err(fidx, at("jump target is not a block")));
                    }
                }
                _ => {}
            }
        }
    }
    // Dataflow pass: reconstruct types at every reachable block entry.
    let mut states: Vec<Option<State>> = vec![None; func.blocks.len()];
    let mut locals = vec![None; func.local_count as usize];
    for (i, p) in func.params.iter().enumerate() {
        locals[i] = Some(*p);
    }
    states[0] = Some(State {
        locals,
        stack: Vec::new(),
    });
    let mut worklist = VecDeque::new();
    worklist.push_back(0usize);
    while let Some(bidx) = worklist.pop_front() {
        let mut state = states[bidx].clone().expect("queued block has a state");
        for (iidx, instr) in func.blocks[bidx].iter().enumerate() {
            step(
                ctx,
                func,
                fidx,
                bidx,
                iidx,
                instr,
                &mut state,
                |target, edge_state| {
                    merge(ctx, fidx, target, edge_state, &mut states, &mut worklist)
                },
            )?;
        }
    }
    Ok(())
}

/// Merge an edge state into a target block. Queue the block again when
/// its entry state changes.
fn merge(
    ctx: &Ctx<'_>,
    fidx: u32,
    target: usize,
    edge: State,
    states: &mut [Option<State>],
    worklist: &mut VecDeque<usize>,
) -> Result<(), VerifyError> {
    match &mut states[target] {
        slot @ None => {
            *slot = Some(edge);
            worklist.push_back(target);
        }
        Some(existing) => {
            if existing.stack.len() != edge.stack.len() {
                return Err(err(
                    fidx,
                    format!("block {target} entry stack shapes do not agree"),
                ));
            }
            let mut changed = false;
            for (have, new) in existing.stack.iter_mut().zip(edge.stack.iter()) {
                if *have != *new {
                    let joined = ctx.join(*have, *new).ok_or_else(|| {
                        err(
                            fidx,
                            format!("block {target} entry stack types have no common type"),
                        )
                    })?;
                    if joined != *have {
                        *have = joined;
                        changed = true;
                    }
                }
            }
            for (have, new) in existing.locals.iter_mut().zip(edge.locals.iter()) {
                let merged = match (*have, *new) {
                    (Some(a), Some(b)) => {
                        if a == b {
                            Some(a)
                        } else {
                            ctx.join(a, b)
                        }
                    }
                    _ => None,
                };
                if merged != *have {
                    *have = merged;
                    changed = true;
                }
            }
            if changed {
                worklist.push_back(target);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn step(
    ctx: &Ctx<'_>,
    func: &Func,
    fidx: u32,
    bidx: usize,
    iidx: usize,
    instr: &Instr,
    state: &mut State,
    mut edge: impl FnMut(usize, State) -> Result<(), VerifyError>,
) -> Result<(), VerifyError> {
    let module = ctx.module;
    let fail = |what: String| err(fidx, format!("block {bidx}, instruction {iidx}: {what}"));
    let pop = |state: &mut State| -> Result<u32, VerifyError> {
        state
            .stack
            .pop()
            .ok_or_else(|| fail("pop from an empty stack".to_string()))
    };
    let pop_expect = |state: &mut State, want: u32| -> Result<u32, VerifyError> {
        let ty = pop(state)?;
        if !ctx.is_subtype(ty, want) {
            return Err(fail(format!(
                "expected type {want} on the stack, found type {ty}"
            )));
        }
        Ok(ty)
    };
    let push = |state: &mut State, ty: u32| -> Result<(), VerifyError> {
        if state.stack.len() >= MAX_STATIC_STACK {
            return Err(fail("static stack depth limit exceeded".to_string()));
        }
        state.stack.push(ty);
        Ok(())
    };
    // Pop `count` values that must match `params` in declaration order.
    let pop_args = |state: &mut State, params: &[u32]| -> Result<(), VerifyError> {
        for want in params.iter().rev() {
            pop_expect(state, *want)?;
        }
        Ok(())
    };
    let as_list = |ty: u32| -> Result<u32, VerifyError> {
        match ctx.ty(ty) {
            BcType::List(e) => Ok(*e),
            _ => Err(fail(format!("expected a list type, found type {ty}"))),
        }
    };
    let as_map = |ty: u32| -> Result<(u32, u32), VerifyError> {
        match ctx.ty(ty) {
            BcType::Map(k, v) => Ok((*k, *v)),
            _ => Err(fail(format!("expected a map type, found type {ty}"))),
        }
    };
    match instr {
        Instr::ConstUnit => push(state, TY_UNIT)?,
        Instr::ConstBool(_) => push(state, TY_BOOL)?,
        Instr::ConstInt(_) => push(state, TY_INT)?,
        Instr::ConstStr(_) => push(state, TY_STR)?,
        Instr::LoadLocal(slot) => {
            let ty = state.locals[*slot as usize]
                .ok_or_else(|| fail("load from a local without a value".to_string()))?;
            push(state, ty)?;
        }
        Instr::StoreLocal(slot) => {
            let ty = pop(state)?;
            state.locals[*slot as usize] = Some(ty);
        }
        Instr::Pop => {
            pop(state)?;
        }
        Instr::Add | Instr::Sub | Instr::Mul | Instr::Div | Instr::Rem => {
            pop_expect(state, TY_INT)?;
            pop_expect(state, TY_INT)?;
            push(state, TY_INT)?;
        }
        Instr::Neg => {
            pop_expect(state, TY_INT)?;
            push(state, TY_INT)?;
        }
        Instr::Not => {
            pop_expect(state, TY_BOOL)?;
            push(state, TY_BOOL)?;
        }
        Instr::LtInt | Instr::LeInt | Instr::GtInt | Instr::GeInt | Instr::EqInt | Instr::NeInt => {
            pop_expect(state, TY_INT)?;
            pop_expect(state, TY_INT)?;
            push(state, TY_BOOL)?;
        }
        Instr::EqBool | Instr::NeBool => {
            pop_expect(state, TY_BOOL)?;
            pop_expect(state, TY_BOOL)?;
            push(state, TY_BOOL)?;
        }
        Instr::EqStr | Instr::NeStr => {
            pop_expect(state, TY_STR)?;
            pop_expect(state, TY_STR)?;
            push(state, TY_BOOL)?;
        }
        Instr::EqRef | Instr::NeRef => {
            let b = pop(state)?;
            let a = pop(state)?;
            let heap_ok = ctx.is_heap(a)
                && ctx.is_heap(b)
                && ctx.ty(a) != &BcType::Str
                && ctx.ty(b) != &BcType::Str;
            if !heap_ok || !(ctx.is_subtype(a, b) || ctx.is_subtype(b, a)) {
                return Err(fail(format!(
                    "reference equality needs related object types, found {a} and {b}"
                )));
            }
            push(state, TY_BOOL)?;
        }
        Instr::Call(callee) => {
            let sig = &module.funcs[*callee as usize];
            pop_args(state, &sig.params)?;
            push(state, sig.ret)?;
        }
        Instr::CallVirtual { selector, argc } => {
            let argc = *argc as usize;
            if state.stack.len() < argc + 1 {
                return Err(fail("virtual call on a short stack".to_string()));
            }
            let recv_ty = state.stack[state.stack.len() - 1 - argc];
            let class = match ctx.ty(recv_ty) {
                BcType::Class(c) => *c,
                _ => {
                    return Err(fail(format!(
                        "virtual call receiver type {recv_ty} is not a class"
                    )));
                }
            };
            let target = ctx
                .find_method(class, *selector)
                .ok_or_else(|| fail(format!("selector {selector} is not a class method")))?;
            let sig = &module.funcs[target as usize];
            if sig.params.len() != argc + 1 {
                return Err(fail("virtual call argument count mismatch".to_string()));
            }
            pop_args(state, &sig.params[1..])?;
            pop_expect(state, sig.params[0])?;
            push(state, sig.ret)?;
        }
        Instr::CallValue { argc } => {
            let argc = *argc as usize;
            if state.stack.len() < argc + 1 {
                return Err(fail("closure call on a short stack".to_string()));
            }
            let callee_ty = state.stack[state.stack.len() - 1 - argc];
            let (params, ret) = match ctx.ty(callee_ty) {
                BcType::Fn(params, ret) => (params.clone(), *ret),
                _ => {
                    return Err(fail(format!(
                        "closure call target type {callee_ty} is not a function type"
                    )));
                }
            };
            if params.len() != argc {
                return Err(fail("closure call argument count mismatch".to_string()));
            }
            pop_args(state, &params)?;
            pop(state)?;
            push(state, ret)?;
        }
        Instr::MakeClosure { func: f, .. } => {
            let target = &module.funcs[*f as usize];
            pop_args(state, &target.captures)?;
            let fn_ty = BcType::Fn(target.params.clone(), target.ret);
            let idx = ctx.type_index.get(&fn_ty).ok_or_else(|| {
                fail("the closure function type is not in the type table".to_string())
            })?;
            push(state, *idx)?;
        }
        Instr::LoadCapture(idx) => {
            let ty = func.captures[*idx as usize];
            push(state, ty)?;
        }
        Instr::New(class) => {
            let ty = ctx.class_ty[*class as usize]
                .ok_or_else(|| fail("the class type is not in the type table".to_string()))?;
            push(state, ty)?;
        }
        Instr::LoadField(field) => {
            let recv = pop(state)?;
            let class = match ctx.ty(recv) {
                BcType::Class(c) => *c,
                _ => return Err(fail(format!("field load on non-class type {recv}"))),
            };
            let fields = &module.classes[class as usize].fields;
            let (_, fty) = fields
                .get(*field as usize)
                .ok_or_else(|| fail("field index out of range".to_string()))?;
            push(state, *fty)?;
        }
        Instr::StoreField(field) => {
            let value = pop(state)?;
            let recv = pop(state)?;
            let class = match ctx.ty(recv) {
                BcType::Class(c) => *c,
                _ => return Err(fail(format!("field store on non-class type {recv}"))),
            };
            let fields = &module.classes[class as usize].fields;
            let (_, fty) = fields
                .get(*field as usize)
                .ok_or_else(|| fail("field index out of range".to_string()))?;
            if !ctx.is_subtype(value, *fty) {
                return Err(fail(format!(
                    "field store expects type {fty}, found type {value}"
                )));
            }
        }
        Instr::ListNew { ty, count } => {
            let elem = as_list(*ty)?;
            for _ in 0..*count {
                pop_expect(state, elem)?;
            }
            push(state, *ty)?;
        }
        Instr::ListLen => {
            let l = pop(state)?;
            as_list(l)?;
            push(state, TY_INT)?;
        }
        Instr::ListAt => {
            pop_expect(state, TY_INT)?;
            let l = pop(state)?;
            let elem = as_list(l)?;
            push(state, elem)?;
        }
        Instr::ListPush => {
            let value = pop(state)?;
            let l = pop(state)?;
            let elem = as_list(l)?;
            if !ctx.is_subtype(value, elem) {
                return Err(fail(format!(
                    "list push expects element type {elem}, found type {value}"
                )));
            }
            push(state, TY_UNIT)?;
        }
        Instr::MapNew { ty, count } => {
            let (k, v) = as_map(*ty)?;
            for _ in 0..*count {
                pop_expect(state, v)?;
                pop_expect(state, k)?;
            }
            push(state, *ty)?;
        }
        Instr::MapLen => {
            let m = pop(state)?;
            as_map(m)?;
            push(state, TY_INT)?;
        }
        Instr::MapHas => {
            let key = pop(state)?;
            let m = pop(state)?;
            let (k, _) = as_map(m)?;
            if !ctx.is_subtype(key, k) {
                return Err(fail(format!("map key expects type {k}, found type {key}")));
            }
            push(state, TY_BOOL)?;
        }
        Instr::MapAt => {
            let key = pop(state)?;
            let m = pop(state)?;
            let (k, v) = as_map(m)?;
            if !ctx.is_subtype(key, k) {
                return Err(fail(format!("map key expects type {k}, found type {key}")));
            }
            push(state, v)?;
        }
        Instr::MapPut => {
            let value = pop(state)?;
            let key = pop(state)?;
            let m = pop(state)?;
            let (k, v) = as_map(m)?;
            if !ctx.is_subtype(key, k) || !ctx.is_subtype(value, v) {
                return Err(fail("map put entry types do not match".to_string()));
            }
            push(state, TY_UNIT)?;
        }
        Instr::SbNew => {
            let idx = ctx
                .type_index
                .get(&BcType::StringBuilder)
                .ok_or_else(|| fail("StringBuilder is not in the type table".to_string()))?;
            push(state, *idx)?;
        }
        Instr::SbAppendStr | Instr::SbAppendInt | Instr::SbAppendBool => {
            let want = match instr {
                Instr::SbAppendStr => TY_STR,
                Instr::SbAppendInt => TY_INT,
                _ => TY_BOOL,
            };
            pop_expect(state, want)?;
            let sb = pop(state)?;
            if ctx.ty(sb) != &BcType::StringBuilder {
                return Err(fail(format!("append on non-builder type {sb}")));
            }
            push(state, sb)?;
        }
        Instr::SbBuild => {
            let sb = pop(state)?;
            if ctx.ty(sb) != &BcType::StringBuilder {
                return Err(fail(format!("build on non-builder type {sb}")));
            }
            push(state, TY_STR)?;
        }
        Instr::BbNew => {
            let idx = ctx
                .type_index
                .get(&BcType::ByteBuffer)
                .ok_or_else(|| fail("ByteBuffer is not in the type table".to_string()))?;
            push(state, *idx)?;
        }
        Instr::BbAppend => {
            pop_expect(state, TY_INT)?;
            let bb = pop(state)?;
            if ctx.ty(bb) != &BcType::ByteBuffer {
                return Err(fail(format!("append on non-buffer type {bb}")));
            }
            push(state, bb)?;
        }
        Instr::BbLen => {
            let bb = pop(state)?;
            if ctx.ty(bb) != &BcType::ByteBuffer {
                return Err(fail(format!("len on non-buffer type {bb}")));
            }
            push(state, TY_INT)?;
        }
        Instr::BbBuild => {
            let bb = pop(state)?;
            if ctx.ty(bb) != &BcType::ByteBuffer {
                return Err(fail(format!("build on non-buffer type {bb}")));
            }
            push(state, TY_STR)?;
        }
        Instr::Freeze => {
            let ty = pop(state)?;
            if !ctx.is_heap(ty) {
                return Err(fail(format!("freeze on non-object type {ty}")));
            }
            push(state, ty)?;
        }
        Instr::Jump(target) => {
            edge(*target as usize, state.clone())?;
        }
        Instr::JumpIfFalse(target) | Instr::JumpIfTrue(target) => {
            pop_expect(state, TY_BOOL)?;
            edge(*target as usize, state.clone())?;
        }
        Instr::Return => {
            pop_expect(state, func.ret)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lm_bytecode::{BcClass, Func, Instr::*, Module, NO_PARENT};

    fn base_types() -> Vec<BcType> {
        vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str]
    }

    fn module_with(blocks: Vec<Vec<Instr>>) -> Module {
        Module {
            strings: vec!["s".to_string()],
            types: base_types(),
            selectors: vec![],
            classes: vec![],
            funcs: vec![Func {
                name: "main".to_string(),
                params: vec![],
                ret: TY_INT,
                captures: vec![],
                local_count: 1,
                blocks,
            }],
            entry: 0,
        }
    }

    /// A module with one class `Counter { value: Int }` and one method
    /// `bump(self): Int` on selector 0, plus an entry function.
    fn class_module(entry_blocks: Vec<Vec<Instr>>) -> Module {
        let mut types = base_types();
        types.push(BcType::Class(0)); // type 4
        Module {
            strings: vec![],
            types,
            selectors: vec!["bump".to_string()],
            classes: vec![BcClass {
                name: "Counter".to_string(),
                parent: NO_PARENT,
                fields: vec![("value".to_string(), TY_INT)],
                methods: vec![(0, 1)],
            }],
            funcs: vec![
                Func {
                    name: "main".to_string(),
                    params: vec![],
                    ret: TY_INT,
                    captures: vec![],
                    local_count: 1,
                    blocks: entry_blocks,
                },
                Func {
                    name: "bump".to_string(),
                    params: vec![4],
                    ret: TY_INT,
                    captures: vec![],
                    local_count: 1,
                    blocks: vec![vec![LoadLocal(0), LoadField(0), Return]],
                },
            ],
            entry: 0,
        }
    }

    #[test]
    fn accepts_simple_function() {
        let m = module_with(vec![vec![ConstInt(1), ConstInt(2), Add, Return]]);
        assert!(verify_module(&m).is_ok());
    }

    #[test]
    fn accepts_class_construction_and_virtual_call() {
        let m = class_module(vec![vec![
            New(0),
            StoreLocal(0),
            LoadLocal(0),
            ConstInt(7),
            StoreField(0),
            LoadLocal(0),
            CallVirtual {
                selector: 0,
                argc: 0,
            },
            Return,
        ]]);
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    #[test]
    fn rejects_bad_jump_target() {
        let m = module_with(vec![vec![Jump(7)]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("jump target"), "{e}");
    }

    #[test]
    fn rejects_wrong_stack_shape() {
        let m = module_with(vec![vec![ConstInt(1), Add, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("empty stack"), "{e}");
    }

    #[test]
    fn rejects_type_confusion() {
        let m = module_with(vec![vec![ConstBool(true), ConstInt(2), Add, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("expected type"), "{e}");
    }

    #[test]
    fn rejects_missing_terminator() {
        let m = module_with(vec![vec![ConstInt(1)]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("terminator"), "{e}");
    }

    #[test]
    fn rejects_load_before_store() {
        let m = module_with(vec![vec![LoadLocal(0), Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("without a value"), "{e}");
    }

    #[test]
    fn rejects_missing_primitive_prefix() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types = vec![BcType::Int, BcType::Unit, BcType::Bool, BcType::Str];
        m.funcs[0].ret = 0;
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("does not start with"), "{e}");
    }

    #[test]
    fn rejects_duplicate_type_entry() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types.push(BcType::Int);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("duplicates"), "{e}");
    }

    #[test]
    fn rejects_forward_type_reference() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types.push(BcType::List(9));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("earlier entry"), "{e}");
    }

    #[test]
    fn rejects_invalid_map_key_type() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types.push(BcType::List(TY_INT)); // 4
        m.types.push(BcType::Map(4, TY_INT)); // key is a list
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("key type"), "{e}");
    }

    #[test]
    fn rejects_field_index_out_of_range() {
        let m = class_module(vec![vec![New(0), LoadField(9), Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("field index"), "{e}");
    }

    #[test]
    fn rejects_wrong_field_store_type() {
        let m = class_module(vec![vec![
            New(0),
            ConstBool(true),
            StoreField(0),
            ConstInt(0),
            Return,
        ]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("field store"), "{e}");
    }

    #[test]
    fn rejects_unknown_selector_on_class() {
        let mut m = class_module(vec![vec![
            New(0),
            CallVirtual {
                selector: 1,
                argc: 0,
            },
            Return,
        ]]);
        m.selectors.push("other".to_string());
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("not a class method"), "{e}");
    }

    #[test]
    fn rejects_virtual_argc_mismatch() {
        let m = class_module(vec![vec![
            New(0),
            ConstInt(1),
            CallVirtual {
                selector: 0,
                argc: 1,
            },
            Return,
        ]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("argument count"), "{e}");
    }

    #[test]
    fn rejects_method_with_wrong_self_type() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        m.funcs[1].params = vec![TY_INT];
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("`self`"), "{e}");
    }

    #[test]
    fn rejects_override_that_changes_parameters() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        // A subclass whose bump takes an extra Int.
        m.types.push(BcType::Class(1)); // 5
        m.classes.push(BcClass {
            name: "Fast".to_string(),
            parent: 0,
            fields: vec![("value".to_string(), TY_INT)],
            methods: vec![(0, 2)],
        });
        m.funcs.push(Func {
            name: "bump2".to_string(),
            params: vec![5, TY_INT],
            ret: TY_INT,
            captures: vec![],
            local_count: 2,
            blocks: vec![vec![ConstInt(1), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("parameter types"), "{e}");
    }

    #[test]
    fn rejects_layout_that_breaks_parent_prefix() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        m.types.push(BcType::Class(1));
        m.classes.push(BcClass {
            name: "Bad".to_string(),
            parent: 0,
            fields: vec![("other".to_string(), TY_BOOL)],
            methods: vec![],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("parent layout"), "{e}");
    }

    #[test]
    fn rejects_direct_call_to_captured_function() {
        let mut m = module_with(vec![vec![Call(1), Return]]);
        m.funcs.push(Func {
            name: "closure".to_string(),
            params: vec![],
            ret: TY_INT,
            captures: vec![TY_INT],
            local_count: 0,
            blocks: vec![vec![LoadCapture(0), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("captures"), "{e}");
    }

    #[test]
    fn rejects_closure_without_fn_type_entry() {
        let mut m = module_with(vec![vec![
            ConstInt(1),
            MakeClosure {
                func: 1,
                captures: 1,
            },
            Pop,
            ConstInt(0),
            Return,
        ]]);
        m.funcs.push(Func {
            name: "closure".to_string(),
            params: vec![],
            ret: TY_INT,
            captures: vec![TY_INT],
            local_count: 0,
            blocks: vec![vec![LoadCapture(0), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("type table"), "{e}");
    }

    #[test]
    fn accepts_closure_create_and_call() {
        let mut m = module_with(vec![vec![
            ConstInt(41),
            MakeClosure {
                func: 1,
                captures: 1,
            },
            ConstInt(1),
            CallValue { argc: 1 },
            Return,
        ]]);
        m.types.push(BcType::Fn(vec![TY_INT], TY_INT));
        m.funcs.push(Func {
            name: "closure".to_string(),
            params: vec![TY_INT],
            ret: TY_INT,
            captures: vec![TY_INT],
            local_count: 1,
            blocks: vec![vec![LoadCapture(0), LoadLocal(0), Add, Return]],
        });
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    #[test]
    fn rejects_call_value_on_non_function() {
        let m = module_with(vec![vec![ConstInt(1), CallValue { argc: 0 }, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("not a function type"), "{e}");
    }

    #[test]
    fn rejects_capture_index_out_of_range() {
        let m = module_with(vec![vec![LoadCapture(0), Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("capture index"), "{e}");
    }

    #[test]
    fn rejects_list_element_type_mismatch() {
        let mut m = module_with(vec![vec![
            ConstBool(true),
            ListNew { ty: 4, count: 1 },
            Pop,
            ConstInt(0),
            Return,
        ]]);
        m.types.push(BcType::List(TY_INT));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("expected type"), "{e}");
    }

    #[test]
    fn rejects_freeze_on_scalar() {
        let m = module_with(vec![vec![ConstInt(1), Freeze, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("freeze"), "{e}");
    }

    #[test]
    fn rejects_entry_with_parameters() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.funcs[0].params = vec![TY_INT];
        m.funcs[0].local_count = 1;
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("entry function"), "{e}");
    }

    #[test]
    fn joins_subclass_stacks_at_merge_points() {
        // Both branches push a different subclass of Animal. The join
        // must settle at the common ancestor.
        let mut types = base_types();
        types.push(BcType::Class(0)); // 4 Animal
        types.push(BcType::Class(1)); // 5 Dog
        types.push(BcType::Class(2)); // 6 Cat
        let animal = BcClass {
            name: "Animal".to_string(),
            parent: NO_PARENT,
            fields: vec![],
            methods: vec![],
        };
        let dog = BcClass {
            name: "Dog".to_string(),
            parent: 0,
            fields: vec![],
            methods: vec![],
        };
        let cat = BcClass {
            name: "Cat".to_string(),
            parent: 0,
            fields: vec![],
            methods: vec![],
        };
        let m = Module {
            strings: vec![],
            types,
            selectors: vec![],
            classes: vec![animal, dog, cat],
            funcs: vec![Func {
                name: "main".to_string(),
                params: vec![],
                ret: TY_UNIT,
                captures: vec![],
                local_count: 0,
                blocks: vec![
                    vec![ConstBool(true), JumpIfFalse(1), New(1), Jump(2)],
                    vec![New(2), Jump(2)],
                    vec![Pop, ConstUnit, Return],
                ],
            }],
            entry: 0,
        };
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }
}
