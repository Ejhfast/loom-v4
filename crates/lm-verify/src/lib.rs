//! Independent bytecode verifier.
//!
//! The verifier receives a decoded module and rejects it unless every
//! table and every function is well formed. It validates the type,
//! selector, type-application, and class tables first. It then
//! reconstructs the operand-stack types and the local-slot types at
//! each block entry with a worklist, and it checks jumps, calls,
//! generic substitution, claimed effect rows, field access, closure
//! creation, tuples, casts, and collection operations. A generic
//! function body is verified once with its type variables opaque;
//! call sites substitute the callee signature through the type
//! application. The verifier shares no code with the source checker.

use lm_bytecode::corepin::CoreLayout;
use lm_bytecode::{BcClassKind, BcRow, BcType, Func, Instr, Module};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt;

/// The largest operand-stack depth the verifier accepts for one function.
const MAX_STATIC_STACK: usize = 4096;

/// The largest portable tuple arity.
const MAX_TUPLE_ARITY: usize = 16;

/// The largest local slot count of one function. The bound rejects a
/// forged `local_count` before any allocation is sized from it.
const MAX_LOCAL_SLOTS: u32 = 65_536;

/// The largest dataflow footprint of one function: block count times
/// local slots. The bound keeps hostile inputs from demanding an
/// unbounded state table.
const MAX_DATAFLOW_CELLS: u64 = 1 << 24;

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

/// The abstract state at one program point. Types are indices into
/// the extended type universe. `None` marks a local slot without a
/// known value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    locals: Vec<Option<u32>>,
    stack: Vec<u32>,
}

/// The extended type universe: the module table plus the types
/// created by substitution during verification.
struct Universe {
    types: Vec<BcType>,
    index: HashMap<BcType, u32>,
}

impl Universe {
    fn intern(&mut self, ty: BcType) -> u32 {
        if let Some(idx) = self.index.get(&ty) {
            return *idx;
        }
        let idx = self.types.len() as u32;
        self.types.push(ty.clone());
        self.index.insert(ty, idx);
        idx
    }
}

/// Shared lookup context for one module.
struct Ctx<'m> {
    module: &'m Module,
    /// Class index to the type index of its `Class` entry, when the
    /// module contains one.
    class_ty: Vec<Option<u32>>,
    uni: RefCell<Universe>,
    /// The resolved pinned core definitions of this module.
    core: CoreLayout,
}

impl<'m> Ctx<'m> {
    fn ty(&self, idx: u32) -> BcType {
        self.uni.borrow().types[idx as usize].clone()
    }

    fn intern(&self, ty: BcType) -> u32 {
        self.uni.borrow_mut().intern(ty)
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

    /// The sort key of one row element, for canonical order checks.
    fn row_key(&self, elem: &BcRow) -> (u8, String, u32) {
        match elem {
            BcRow::Op(idx) => (
                0,
                self.module
                    .strings
                    .get(*idx as usize)
                    .cloned()
                    .unwrap_or_default(),
                0,
            ),
            BcRow::Var(v) => (1, String::new(), *v),
        }
    }

    /// Return true when the row is sorted and has no duplicate.
    fn row_canonical(&self, row: &[BcRow]) -> bool {
        row.windows(2)
            .all(|w| self.row_key(&w[0]) < self.row_key(&w[1]))
    }

    /// Return true when every element of `sub` is included in `sup`.
    fn row_included(&self, sub: &[BcRow], sup: &[BcRow]) -> bool {
        sub.iter().all(|elem| match elem {
            BcRow::Var(v) => sup.contains(&BcRow::Var(*v)),
            BcRow::Op(n) => {
                let name = &self.module.strings[*n as usize];
                sup.iter().any(|s| match s {
                    BcRow::Op(m) => {
                        let sup_name = &self.module.strings[*m as usize];
                        sup_name == name
                            || name
                                .split_once('.')
                                .map(|(group, _)| group == sup_name)
                                .unwrap_or(false)
                    }
                    BcRow::Var(_) => false,
                })
            }
        })
    }

    /// Substitute effect variables in a row and re-canonicalize.
    fn row_subst(&self, row: &[BcRow], rows: &[Vec<BcRow>]) -> Vec<BcRow> {
        let mut out: Vec<BcRow> = Vec::new();
        for elem in row {
            match elem {
                BcRow::Var(v) => match rows.get(*v as usize) {
                    Some(replacement) => out.extend_from_slice(replacement),
                    None => out.push(*elem),
                },
                BcRow::Op(_) => out.push(*elem),
            }
        }
        out.sort_by_key(|e| self.row_key(e));
        out.dedup();
        out
    }

    /// Substitute type variables and effect variables in one type.
    fn subst(&self, ty: u32, targs: &[u32], rows: &[Vec<BcRow>]) -> u32 {
        if targs.is_empty() && rows.is_empty() {
            return ty;
        }
        match self.ty(ty) {
            BcType::Var(i) => targs.get(i as usize).copied().unwrap_or(ty),
            BcType::Inst(c, args) => {
                let args: Vec<u32> = args.iter().map(|a| self.subst(*a, targs, rows)).collect();
                self.intern(BcType::Inst(c, args))
            }
            BcType::List(e) => {
                let e = self.subst(e, targs, rows);
                self.intern(BcType::List(e))
            }
            BcType::Map(k, v) => {
                let k = self.subst(k, targs, rows);
                let v = self.subst(v, targs, rows);
                self.intern(BcType::Map(k, v))
            }
            BcType::Tuple(elems) => {
                let elems: Vec<u32> = elems.iter().map(|e| self.subst(*e, targs, rows)).collect();
                self.intern(BcType::Tuple(elems))
            }
            BcType::Fn(params, muts, ret, row) => {
                let params: Vec<u32> = params.iter().map(|p| self.subst(*p, targs, rows)).collect();
                let ret = self.subst(ret, targs, rows);
                let row = self.row_subst(&row, rows);
                self.intern(BcType::Fn(params, muts, ret, row))
            }
            BcType::Vm(t) => {
                let t = self.subst(t, targs, rows);
                self.intern(BcType::Vm(t))
            }
            BcType::PendingCall(a, r) => {
                let a = self.subst(a, targs, rows);
                let r = self.subst(r, targs, rows);
                self.intern(BcType::PendingCall(a, r))
            }
            _ => ty,
        }
    }

    /// Return true when a value of type `found` is valid where the
    /// code expects type `expected`.
    fn is_subtype(&self, found: u32, expected: u32) -> bool {
        if found == expected {
            return true;
        }
        match (self.ty(found), self.ty(expected)) {
            (BcType::Class(a), BcType::Class(b)) => self.class_extends(a, b),
            (BcType::Inst(a, xs), BcType::Inst(b, ys)) => self.class_extends(a, b) && xs == ys,
            (BcType::Tuple(xs), BcType::Tuple(ys)) => {
                xs.len() == ys.len()
                    && xs
                        .iter()
                        .zip(ys.iter())
                        .all(|(x, y)| self.is_subtype(*x, *y))
            }
            (BcType::Fn(fp, fm, fr, frow), BcType::Fn(ep, em, er, erow)) => {
                // A function that needs a `mut` argument is not valid
                // where the expected type promises a read-only call.
                fp.len() == ep.len()
                    && fp
                        .iter()
                        .zip(ep.iter())
                        .all(|(f, e)| self.is_subtype(*e, *f))
                    && fm.iter().zip(em.iter()).all(|(f, e)| !*f || *e)
                    && self.is_subtype(fr, er)
                    && self.row_included(&frow, &erow)
            }
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
        match (self.ty(a), self.ty(b)) {
            (BcType::Class(ca), BcType::Class(cb)) => {
                let common = self.common_ancestor(ca, cb)?;
                Some(self.intern(BcType::Class(common)))
            }
            (BcType::Inst(ca, xs), BcType::Inst(cb, ys)) => {
                if xs != ys {
                    return None;
                }
                let common = self.common_ancestor(ca, cb)?;
                Some(self.intern(BcType::Inst(common, xs)))
            }
            (BcType::Tuple(xs), BcType::Tuple(ys)) => {
                if xs.len() != ys.len() {
                    return None;
                }
                let mut elems = Vec::with_capacity(xs.len());
                for (x, y) in xs.iter().zip(ys.iter()) {
                    elems.push(self.join(*x, *y)?);
                }
                Some(self.intern(BcType::Tuple(elems)))
            }
            _ => None,
        }
    }

    fn common_ancestor(&self, a: u32, b: u32) -> Option<u32> {
        let mut anc = Some(a);
        while let Some(c) = anc {
            if self.class_extends(b, c) {
                return Some(c);
            }
            anc = self.module.classes[c as usize].parent();
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

    /// The nominal class and arguments of one instance type.
    fn as_instance(&self, ty: u32) -> Option<(u32, Vec<u32>)> {
        match self.ty(ty) {
            BcType::Class(c) => Some((c, vec![])),
            BcType::Inst(c, args) => Some((c, args)),
            _ => None,
        }
    }

    /// Return true when the type is a heap object type. An `Op` value
    /// is an immediate.
    fn is_heap(&self, idx: u32) -> bool {
        matches!(
            self.ty(idx),
            BcType::Str
                | BcType::Class(_)
                | BcType::Inst(_, _)
                | BcType::List(_)
                | BcType::Map(_, _)
                | BcType::Tuple(_)
                | BcType::Fn(_, _, _, _)
                | BcType::StringBuilder
                | BcType::ByteBuffer
                | BcType::Digest
                | BcType::Fault
                | BcType::Request
                | BcType::PolicyTable
                | BcType::EmptyVm
                | BcType::Vm(_)
                | BcType::PendingCall(_, _)
        )
    }

    /// Check that every type variable inside `ty` is below `limit`
    /// and every row variable is below `elimit`.
    fn vars_bounded(&self, ty: u32, limit: u32, elimit: u32) -> bool {
        match self.ty(ty) {
            BcType::Var(i) => i < limit,
            BcType::Inst(_, args) => args.iter().all(|a| self.vars_bounded(*a, limit, elimit)),
            BcType::List(e) => self.vars_bounded(e, limit, elimit),
            BcType::Map(k, v) => {
                self.vars_bounded(k, limit, elimit) && self.vars_bounded(v, limit, elimit)
            }
            BcType::Tuple(elems) => elems.iter().all(|e| self.vars_bounded(*e, limit, elimit)),
            BcType::Fn(params, _, ret, row) => {
                params.iter().all(|p| self.vars_bounded(*p, limit, elimit))
                    && self.vars_bounded(ret, limit, elimit)
                    && self.row_vars_bounded(&row, elimit)
            }
            BcType::Vm(t) => self.vars_bounded(t, limit, elimit),
            BcType::PendingCall(a, r) => {
                self.vars_bounded(a, limit, elimit) && self.vars_bounded(r, limit, elimit)
            }
            BcType::Op(_, f) => self.vars_bounded(f, limit, elimit),
            _ => true,
        }
    }

    fn row_vars_bounded(&self, row: &[BcRow], elimit: u32) -> bool {
        row.iter().all(|e| match e {
            BcRow::Var(v) => *v < elimit,
            BcRow::Op(_) => true,
        })
    }

    /// True when a claimed row covers one exact operation name: the
    /// row holds the exact name or its group.
    fn row_has_name(&self, row: &[BcRow], name: &str) -> bool {
        let group = name.split_once('.').map(|(g, _)| g);
        row.iter().any(|elem| match elem {
            BcRow::Op(idx) => {
                let text = &self.module.strings[*idx as usize];
                text == name || group.map(|g| text == g).unwrap_or(false)
            }
            BcRow::Var(_) => false,
        })
    }

    /// Convert one manifest type into a universe type index. The core
    /// enums must be present when a signature names them.
    fn abi_ty(&self, t: lm_abi::AbiType) -> Result<u32, String> {
        match t {
            lm_abi::AbiType::Unit => Ok(TY_UNIT),
            lm_abi::AbiType::Int => Ok(TY_INT),
            lm_abi::AbiType::Str => Ok(TY_STR),
            lm_abi::AbiType::ResultOptionStrIoError => {
                let (Some(option), Some(result), Some(io_error)) =
                    (self.core.option, self.core.result, self.core.io_error)
                else {
                    return Err("the module does not carry the pinned core Option, Result, \
                         and IoError definitions"
                        .to_string());
                };
                let opt_str = self.intern(BcType::Inst(option, vec![TY_STR]));
                let err = self.intern(BcType::Class(io_error));
                Ok(self.intern(BcType::Inst(result, vec![opt_str, err])))
            }
        }
    }

    /// The function type of one fixed operation as a universe index.
    fn fixed_sig_type(&self, op: u32) -> Result<u32, String> {
        let def = lm_abi::op(op);
        let mut params = Vec::with_capacity(def.params.len());
        for p in def.params {
            params.push(self.abi_ty(*p)?);
        }
        let ret = self.abi_ty(def.reply)?;
        let muts = vec![false; params.len()];
        Ok(self.intern(BcType::Fn(params, muts, ret, vec![])))
    }

    /// The argument-view type of one fixed operation: unit for a
    /// zero-parameter operation, a tuple otherwise.
    fn op_args_view(&self, op: u32) -> Result<u32, String> {
        let def = lm_abi::op(op);
        if def.params.is_empty() {
            return Ok(TY_UNIT);
        }
        let mut elems = Vec::with_capacity(def.params.len());
        for p in def.params {
            elems.push(self.abi_ty(*p)?);
        }
        Ok(self.intern(BcType::Tuple(elems)))
    }

    /// One VM event instance type, for example `RunResult[t]`.
    fn event_inst(&self, parent: Option<u32>, what: &str, arg: u32) -> Result<u32, String> {
        let Some(parent) = parent else {
            return Err(format!(
                "the module does not carry the pinned core {what} definition"
            ));
        };
        Ok(self.intern(BcType::Inst(parent, vec![arg])))
    }
}

/// The verifier version. It takes part in the verified-code cache
/// key: a rule change invalidates every cached admission.
///
/// Version 4 adds the typing rules of the three digest
/// instructions.
pub const VERIFIER_VERSION: u32 = 4;

/// Verify a full module. Every table and every function must pass.
///
/// The core layout comes from the core role table the artifact
/// carries. The verifier proves the shape of every filled slot, so it
/// reads no definition hash and no source name.
pub fn verify_module(module: &Module) -> Result<(), VerifyError> {
    let ctx = verify_structure(module)?;
    let imported = module.extern_funcs();
    for (idx, func) in module.funcs.iter().enumerate() {
        // An imported function has no body to check. The structural
        // pass already proved it carries a signature only.
        if imported[idx] {
            continue;
        }
        verify_func(&ctx, func, idx as u32)?;
    }
    Ok(())
}

/// Validate every module-level rule without the per-function
/// dataflow: the tables and the entry shape. The verified-code cache
/// may skip only the dataflow, never this pass, so a hash-equal
/// byte stream with a non-canonical table is rejected on every load.
pub fn verify_structure_only(module: &Module) -> Result<(), VerifyError> {
    verify_structure(module).map(|_| ())
}

fn verify_structure(module: &Module) -> Result<Ctx<'_>, VerifyError> {
    let core = lm_bytecode::corepin::declared_layout(module);
    let ctx = verify_tables(module, core)?;
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
    let entry_func = &module.funcs[entry];
    if !entry_func.params.is_empty() {
        return Err(err(
            module.entry,
            "the entry function must not have parameters",
        ));
    }
    if !entry_func.captures.is_empty() {
        return Err(err(
            module.entry,
            "the entry function must not have captures",
        ));
    }
    if entry_func.type_params != 0 || entry_func.effect_params != 0 {
        return Err(err(module.entry, "the entry function must not be generic"));
    }
    Ok(ctx)
}

// ----------------------------------------------------------------
// The core role slots.
// ----------------------------------------------------------------

/// The core role indices. The order is `corepin::PINNED_LABELS`, and
/// the test `the_role_indices_match_the_pinned_labels` proves it.
const ROLE_OPTION: usize = 0;
const ROLE_OPTION_SOME: usize = 1;
const ROLE_OPTION_NONE: usize = 2;
const ROLE_RESULT: usize = 3;
const ROLE_RESULT_OK: usize = 4;
const ROLE_RESULT_ERR: usize = 5;
const ROLE_IO_ERROR: usize = 6;
const ROLE_IO_ERROR_FAILED: usize = 7;
const ROLE_RUN_RESULT: usize = 8;
const ROLE_RUN_DONE: usize = 9;
const ROLE_RUN_FAULT: usize = 10;
const ROLE_STEP_EVENT: usize = 11;
const ROLE_STEP_RAN: usize = 12;
const ROLE_STEP_WAITING: usize = 13;
const ROLE_STEP_DONE: usize = 14;
const ROLE_STEP_FAULT: usize = 15;
const ROLE_DRIVE_EVENT: usize = 16;
const ROLE_DRIVE_ASKED: usize = 17;
const ROLE_DRIVE_DONE: usize = 18;
const ROLE_DRIVE_FAULT: usize = 19;

/// The field shape one core arm must carry.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldShape {
    /// The type variable at this position of the family arity.
    Var(u32),
    Str,
    Fault,
    Request,
}

/// One core family: the parent role, the generic arity, and the arm
/// roles in declaration order.
const CORE_FAMILIES: [(usize, u32, &[usize], &str); 6] = [
    (
        ROLE_OPTION,
        1,
        &[ROLE_OPTION_SOME, ROLE_OPTION_NONE],
        "Option",
    ),
    (ROLE_RESULT, 2, &[ROLE_RESULT_OK, ROLE_RESULT_ERR], "Result"),
    (ROLE_IO_ERROR, 0, &[ROLE_IO_ERROR_FAILED], "IoError"),
    (
        ROLE_RUN_RESULT,
        1,
        &[ROLE_RUN_DONE, ROLE_RUN_FAULT],
        "RunResult",
    ),
    (
        ROLE_STEP_EVENT,
        1,
        &[
            ROLE_STEP_RAN,
            ROLE_STEP_WAITING,
            ROLE_STEP_DONE,
            ROLE_STEP_FAULT,
        ],
        "StepEvent",
    ),
    (
        ROLE_DRIVE_EVENT,
        1,
        &[ROLE_DRIVE_ASKED, ROLE_DRIVE_DONE, ROLE_DRIVE_FAULT],
        "DriveEvent",
    ),
];

/// The field layout every core arm must carry, by role.
const CORE_ARM_FIELDS: [(usize, &[FieldShape]); 14] = [
    (ROLE_OPTION_SOME, &[FieldShape::Var(0)]),
    (ROLE_OPTION_NONE, &[]),
    (ROLE_RESULT_OK, &[FieldShape::Var(0)]),
    (ROLE_RESULT_ERR, &[FieldShape::Var(1)]),
    (ROLE_IO_ERROR_FAILED, &[FieldShape::Str]),
    (ROLE_RUN_DONE, &[FieldShape::Var(0)]),
    (ROLE_RUN_FAULT, &[FieldShape::Fault]),
    (ROLE_STEP_RAN, &[]),
    (ROLE_STEP_WAITING, &[]),
    (ROLE_STEP_DONE, &[FieldShape::Var(0)]),
    (ROLE_STEP_FAULT, &[FieldShape::Fault]),
    (ROLE_DRIVE_ASKED, &[FieldShape::Request]),
    (ROLE_DRIVE_DONE, &[FieldShape::Var(0)]),
    (ROLE_DRIVE_FAULT, &[FieldShape::Fault]),
];

/// Prove the shape of every declared core role slot.
///
/// The artifact declares which class fills each role. The verifier
/// never trusts that claim: it proves the kind, the generic arity, the
/// parent slot, and the exact field layout of every filled slot. A
/// crafted table therefore rejects instead of handing the runtime a
/// class it cannot allocate through.
///
/// The rules read structure only. No name and no definition hash takes
/// part, so a rename changes nothing the verifier reads.
fn verify_core_roles(module: &Module) -> Result<(), VerifyError> {
    let terr = |message: String| VerifyError {
        func: u32::MAX,
        message,
    };
    let slot = |role: usize| -> Option<u32> {
        let idx = module.core_roles[role];
        if idx == lm_bytecode::NO_ROLE {
            None
        } else {
            Some(idx)
        }
    };
    // A role slot names a class of this module, and no two roles name
    // one class. The decoder proves the same rule; a hand-built module
    // reaches the verifier without a decoder.
    let mut taken: Vec<u32> = Vec::new();
    for role in 0..lm_bytecode::CORE_ROLE_COUNT {
        let Some(idx) = slot(role) else { continue };
        if idx as usize >= module.classes.len() {
            return Err(terr(format!(
                "core role {role} names a class outside the table"
            )));
        }
        if taken.contains(&idx) {
            return Err(terr(format!(
                "core role {role} names a class another role took"
            )));
        }
        taken.push(idx);
    }
    for (family_role, arity, arm_roles, family) in CORE_FAMILIES {
        let Some(parent) = slot(family_role) else {
            // A family the artifact does not declare must declare no
            // arm either. The runtime allocates through the arms.
            for arm in arm_roles {
                if slot(*arm).is_some() {
                    return Err(terr(format!(
                        "the core family `{family}` declares an arm without its parent"
                    )));
                }
            }
            continue;
        };
        let class = &module.classes[parent as usize];
        if class.kind != BcClassKind::Abstract
            || class.type_params != arity
            || class.parent().is_some()
            || !class.fields.is_empty()
        {
            return Err(terr(format!(
                "the core family `{family}` names a class that is not its enum parent"
            )));
        }
        for arm_role in arm_roles {
            let Some(arm) = slot(*arm_role) else {
                return Err(terr(format!(
                    "the core family `{family}` resolves without every arm"
                )));
            };
            let arm_class = &module.classes[arm as usize];
            if arm_class.kind != BcClassKind::Case
                || arm_class.type_params != arity
                || arm_class.parent() != Some(parent)
            {
                return Err(terr(format!(
                    "the core family `{family}` names an arm that is not its case class"
                )));
            }
            let fields = CORE_ARM_FIELDS
                .iter()
                .find(|(role, _)| role == arm_role)
                .map(|(_, fields)| *fields)
                .expect("every arm role states its field layout");
            if arm_class.fields.len() != fields.len() {
                return Err(terr(format!(
                    "the core family `{family}` names an arm with the wrong field count"
                )));
            }
            for (position, want) in fields.iter().enumerate() {
                let found = &module.types[arm_class.fields[position].1 as usize];
                let ok = match want {
                    FieldShape::Var(i) => found == &BcType::Var(*i),
                    FieldShape::Str => found == &BcType::Str,
                    FieldShape::Fault => found == &BcType::Fault,
                    FieldShape::Request => found == &BcType::Request,
                };
                if !ok {
                    return Err(terr(format!(
                        "the core family `{family}` names an arm whose field {position} \
                         has the wrong type"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Validate the type, selector, application, class, and function
/// tables.
fn verify_tables(module: &Module, core: CoreLayout) -> Result<Ctx<'_>, VerifyError> {
    let terr = |message: String| VerifyError {
        func: u32::MAX,
        message,
    };
    // The selector table must hold no duplicate name. The canonical
    // identity encoding replaces a selector index with its name, so a
    // duplicate name lets two different dispatch keys hash alike. The
    // verified-code cache keys on that hash, so this rule keeps the
    // index-to-name map injective and belongs in the structural pass.
    let mut selector_names: HashMap<&str, u32> = HashMap::new();
    for (idx, name) in module.selectors.iter().enumerate() {
        if let Some(first) = selector_names.insert(name.as_str(), idx as u32) {
            return Err(terr(format!(
                "selector {idx} duplicates the name of selector {first}"
            )));
        }
    }
    // The type table must start with the canonical primitive prefix.
    let prefix = [BcType::Unit, BcType::Bool, BcType::Int, BcType::Str];
    if module.types.len() < prefix.len() || module.types[..prefix.len()] != prefix[..] {
        return Err(terr(
            "the type table does not start with Unit, Bool, Int, String".to_string(),
        ));
    }
    let mut index: HashMap<BcType, u32> = HashMap::new();
    for (idx, ty) in module.types.iter().enumerate() {
        if index.insert(ty.clone(), idx as u32).is_some() {
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
            BcType::StringBuilder | BcType::ByteBuffer | BcType::Digest => {}
            BcType::Var(_) => {}
            BcType::Class(c) => {
                if *c as usize >= module.classes.len() {
                    return Err(terr(format!(
                        "type {idx} names class {c}, which does not exist"
                    )));
                }
                if module.classes[*c as usize].type_params != 0 {
                    return Err(terr(format!(
                        "type {idx} names the generic class {c} without arguments"
                    )));
                }
            }
            BcType::Inst(c, args) => {
                if *c as usize >= module.classes.len() {
                    return Err(terr(format!(
                        "type {idx} names class {c}, which does not exist"
                    )));
                }
                let arity = module.classes[*c as usize].type_params;
                if arity == 0 || args.len() != arity as usize {
                    return Err(terr(format!(
                        "type {idx} applies class {c} with the wrong argument count"
                    )));
                }
                for a in args {
                    check_ref(*a)?;
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
            BcType::Tuple(elems) => {
                if elems.is_empty() || elems.len() > MAX_TUPLE_ARITY {
                    return Err(terr(format!(
                        "type {idx} is a tuple with an invalid arity {}",
                        elems.len()
                    )));
                }
                for e in elems {
                    check_ref(*e)?;
                }
            }
            BcType::Fn(params, muts, ret, row) => {
                if muts.len() != params.len() {
                    return Err(terr(format!(
                        "type {idx} has a function type whose mut markers do \
                         not align with the parameters"
                    )));
                }
                for p in params {
                    check_ref(*p)?;
                }
                check_ref(*ret)?;
                for elem in row {
                    if let BcRow::Op(s) = elem {
                        if *s as usize >= module.strings.len() {
                            return Err(terr(format!(
                                "type {idx} has a row with an invalid string index"
                            )));
                        }
                        if !lm_abi::row_name_valid(&module.strings[*s as usize]) {
                            return Err(terr(format!(
                                "type {idx} has a row that names `{}`, which is \
                                 not in the operation manifest",
                                module.strings[*s as usize]
                            )));
                        }
                    }
                }
            }
            BcType::Fault | BcType::Request | BcType::PolicyTable | BcType::EmptyVm => {}
            BcType::Vm(t) => check_ref(*t)?,
            BcType::PendingCall(a, r) => {
                check_ref(*a)?;
                check_ref(*r)?;
            }
            BcType::Op(op, f) => {
                if *op >= lm_abi::OP_COUNT || lm_abi::op(*op).kind != lm_abi::OpKind::Fixed {
                    return Err(terr(format!(
                        "type {idx} names an invalid first-class operation slot {op}"
                    )));
                }
                check_ref(*f)?;
                // The function type must equal the manifest
                // signature; the check runs after the context exists.
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
        uni: RefCell::new(Universe {
            types: module.types.clone(),
            index,
        }),
        core,
    };
    // Row canonicality inside function types.
    for (idx, ty) in module.types.iter().enumerate() {
        if let BcType::Fn(_, _, _, row) = ty {
            if !ctx.row_canonical(row) {
                return Err(terr(format!("type {idx} has a non-canonical row")));
            }
        }
    }
    // First-class operation types must carry the exact manifest
    // signature.
    for (idx, ty) in module.types.iter().enumerate() {
        if let BcType::Op(op, f) = ty {
            let sig = ctx.fixed_sig_type(*op).map_err(&terr)?;
            if *f != sig {
                return Err(terr(format!(
                    "type {idx} claims a wrong signature for operation {}",
                    lm_abi::op_name(*op)
                )));
            }
        }
    }
    // Validate the type applications.
    for (aidx, app) in module.apps.iter().enumerate() {
        for t in &app.types {
            if *t as usize >= module.types.len() {
                return Err(terr(format!(
                    "application {aidx} references an invalid type index"
                )));
            }
        }
        for row in &app.rows {
            for elem in row {
                if let BcRow::Op(s) = elem {
                    if *s as usize >= module.strings.len() {
                        return Err(terr(format!(
                            "application {aidx} has a row with an invalid string index"
                        )));
                    }
                    if !lm_abi::row_name_valid(&module.strings[*s as usize]) {
                        return Err(terr(format!(
                            "application {aidx} has a row that names `{}`, which \
                             is not in the operation manifest",
                            module.strings[*s as usize]
                        )));
                    }
                }
            }
            if !ctx.row_canonical(row) {
                return Err(terr(format!("application {aidx} has a non-canonical row")));
            }
        }
    }
    // Validate the import slots. Each slot names one definition of its
    // own kind, and no definition takes two slots. An imported
    // definition carries a signature and no body: the linker replaces
    // it with the provider definition, and the loader admits a module
    // only when the import table is empty.
    let extern_classes = module.extern_classes();
    let extern_funcs = module.extern_funcs();
    {
        let mut claimed_classes = vec![false; module.classes.len()];
        let mut claimed_funcs = vec![false; module.funcs.len()];
        for (idx, import) in module.imports.iter().enumerate() {
            let ierr = |message: String| terr(format!("import {idx}: {message}"));
            if import.module.is_empty() || import.name.is_empty() {
                return Err(ierr("the slot needs a module path and a name".to_string()));
            }
            let claimed = if import.kind.is_func() {
                &mut claimed_funcs
            } else {
                &mut claimed_classes
            };
            let at = import.def as usize;
            if at >= claimed.len() {
                return Err(ierr("the definition index is out of range".to_string()));
            }
            if claimed[at] {
                return Err(ierr(
                    "the definition already has an import slot".to_string(),
                ));
            }
            claimed[at] = true;
        }
    }
    if extern_funcs.get(module.entry as usize) == Some(&true) {
        return Err(terr("the entry function cannot be imported".to_string()));
    }
    // Validate classes.
    for (cidx, class) in module.classes.iter().enumerate() {
        let cerr = |message: String| terr(format!("class {cidx}: {message}"));
        if extern_classes[cidx] {
            if let Some(p) = class.parent() {
                if !extern_classes[p as usize] {
                    return Err(cerr(
                        "an imported class cannot inherit a local class".to_string(),
                    ));
                }
            }
        }
        match class.kind {
            BcClassKind::Abstract => {
                if class.parent().is_some() {
                    return Err(cerr("an abstract enum parent cannot inherit".to_string()));
                }
            }
            BcClassKind::Case => {
                let Some(p) = class.parent() else {
                    return Err(cerr("a case class needs its enum parent".to_string()));
                };
                if p as usize >= cidx {
                    return Err(cerr(format!("parent {p} is not an earlier class entry")));
                }
                if module.classes[p as usize].kind != BcClassKind::Abstract {
                    return Err(cerr(
                        "a case class parent must be an abstract enum parent".to_string(),
                    ));
                }
                if module.classes[p as usize].type_params != class.type_params {
                    return Err(cerr(
                        "a case class must keep the family type arity".to_string(),
                    ));
                }
            }
            BcClassKind::Normal => {}
        }
        if let Some(p) = class.parent() {
            if p as usize >= cidx {
                return Err(cerr(format!("parent {p} is not an earlier class entry")));
            }
            let parent = &module.classes[p as usize];
            if class.kind != BcClassKind::Case {
                if parent.kind != BcClassKind::Normal {
                    return Err(cerr(
                        "only a case class may inherit a sealed enum class".to_string(),
                    ));
                }
                if parent.type_params != 0 || class.type_params != 0 {
                    return Err(cerr(
                        "a generic class cannot take part in inheritance".to_string(),
                    ));
                }
            }
            // The field layout must extend the parent layout exactly.
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
            if !ctx.vars_bounded(*fty, class.type_params, 0) {
                return Err(cerr(format!(
                    "field `{fname}` uses a type variable outside the class arity"
                )));
            }
        }
        // The canonical self type of the class.
        let own_ty = if class.type_params == 0 {
            ctx.class_ty[cidx]
        } else {
            let vars: Vec<u32> = (0..class.type_params)
                .map(|i| ctx.uni.borrow().index.get(&BcType::Var(i)).copied())
                .collect::<Option<Vec<u32>>>()
                .unwrap_or_default()
                .to_vec();
            if vars.len() == class.type_params as usize {
                ctx.uni
                    .borrow()
                    .index
                    .get(&BcType::Inst(cidx as u32, vars))
                    .copied()
            } else {
                None
            }
        };
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
            if extern_funcs[*func as usize] != extern_classes[cidx] {
                return Err(cerr(format!(
                    "method function {func} does not follow the import state of \
                     its class"
                )));
            }
            let f = &module.funcs[*func as usize];
            if !f.captures.is_empty() {
                return Err(cerr("a method function must not have captures".to_string()));
            }
            if f.type_params < class.type_params {
                return Err(cerr(format!(
                    "method function {func} does not carry the class type arity"
                )));
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
                    // `get` keeps a malformed marker vector from
                    // panicking before the signature validation runs.
                    if base.param_muts.get(1..) != f.param_muts.get(1..) {
                        return Err(cerr(format!(
                            "override of selector {sel} changes the parameter mut markers"
                        )));
                    }
                    if base.type_params != f.type_params || base.effect_params != f.effect_params {
                        return Err(cerr(format!(
                            "override of selector {sel} changes the generic arity"
                        )));
                    }
                    if !ctx.is_subtype(f.ret, base.ret) {
                        return Err(cerr(format!(
                            "override of selector {sel} widens the result type"
                        )));
                    }
                    if !ctx.row_included(&f.row, &base.row) {
                        return Err(cerr(format!(
                            "override of selector {sel} widens the effect row"
                        )));
                    }
                }
            }
        }
    }
    // Validate the declared core role slots. The class table is
    // validated above, so every field type index is inside the type
    // table by now.
    verify_core_roles(module)?;
    // Validate function signatures and the declared local-type
    // tables. The verifier validates the table instead of trusting
    // it: entries must be valid types, the parameter prefix must
    // equal the signature, and every variable must be in scope.
    for (fidx, func) in module.funcs.iter().enumerate() {
        if extern_funcs[fidx] {
            // An imported function is a declaration: a signature with
            // no body, no captures, and no extra local slots.
            if !func.blocks.is_empty() {
                return Err(err(fidx as u32, "an imported function must have no body"));
            }
            if !func.captures.is_empty() {
                return Err(err(
                    fidx as u32,
                    "an imported function must have no captures",
                ));
            }
            if func.local_types.len() != func.params.len() {
                return Err(err(
                    fidx as u32,
                    "an imported function must declare only its parameter slots",
                ));
            }
        }
        if func.param_muts.len() != func.params.len() {
            return Err(err(
                fidx as u32,
                "the parameter mut markers do not align with the parameters",
            ));
        }
        if func.local_types.len() < func.params.len() {
            return Err(err(fidx as u32, "more parameters than local slots"));
        }
        if func.local_types[..func.params.len()] != func.params[..] {
            return Err(err(
                fidx as u32,
                "the local-type table prefix does not equal the parameter types",
            ));
        }
        for t in func
            .params
            .iter()
            .chain(func.captures.iter())
            .chain(func.local_types.iter())
            .chain([&func.ret])
        {
            if *t as usize >= module.types.len() {
                return Err(err(
                    fidx as u32,
                    "the signature references an invalid type index",
                ));
            }
            if !ctx.vars_bounded(*t, func.type_params, func.effect_params) {
                return Err(err(
                    fidx as u32,
                    "the signature uses a variable outside the declared generic arity",
                ));
            }
        }
        for elem in &func.row {
            match elem {
                BcRow::Op(s) => {
                    if *s as usize >= module.strings.len() {
                        return Err(err(
                            fidx as u32,
                            "the declared row references an invalid string index",
                        ));
                    }
                    if !lm_abi::row_name_valid(&module.strings[*s as usize]) {
                        return Err(err(
                            fidx as u32,
                            format!(
                                "the declared row names `{}`, which is not in the \
                                 operation manifest",
                                module.strings[*s as usize]
                            ),
                        ));
                    }
                }
                BcRow::Var(v) => {
                    if *v >= func.effect_params {
                        return Err(err(
                            fidx as u32,
                            "the declared row uses an effect variable outside the arity",
                        ));
                    }
                }
            }
        }
        if !ctx.row_canonical(&func.row) {
            return Err(err(fidx as u32, "the declared row is not canonical"));
        }
    }
    Ok(ctx)
}

/// The expected operand count of one perform instruction. VM control
/// operations count their receiver.
fn perform_argc(op: u32) -> u32 {
    let def = lm_abi::op(op);
    match def.kind {
        lm_abi::OpKind::Fixed => def.params.len() as u32,
        lm_abi::OpKind::VmControl => match op {
            lm_abi::OP_VM_NEW => 0,
            lm_abi::OP_VM_RUN | lm_abi::OP_VM_STEP | lm_abi::OP_VM_DRIVE | lm_abi::OP_VM_TABLE => 1,
            lm_abi::OP_VM_DISPATCH => 2,
            lm_abi::OP_VM_FROM_OBJECT | lm_abi::OP_VM_ANSWER | lm_abi::OP_VM_REJECT => 3,
            _ => unreachable!("every VmControl slot has an arity"),
        },
    }
}

/// Validate one type application against a callee's generic arity and
/// the caller's variable scope.
fn check_app(
    ctx: &Ctx<'_>,
    caller: &Func,
    fidx: u32,
    at: &dyn Fn(&str) -> String,
    app_idx: u32,
    want_types: u32,
    want_rows: u32,
) -> Result<(), VerifyError> {
    let app = ctx
        .module
        .apps
        .get(app_idx as usize)
        .ok_or_else(|| err(fidx, at("type application index out of range")))?;
    if app.types.len() != want_types as usize {
        return Err(err(fidx, at("type application arity mismatch")));
    }
    if app.rows.len() != want_rows as usize {
        return Err(err(fidx, at("type application row arity mismatch")));
    }
    for t in &app.types {
        if !ctx.vars_bounded(*t, caller.type_params, caller.effect_params) {
            return Err(err(
                fidx,
                at("type application uses a variable outside the caller scope"),
            ));
        }
    }
    for row in &app.rows {
        if !ctx.row_vars_bounded(row, caller.effect_params) {
            return Err(err(
                fidx,
                at("type application row uses a variable outside the caller scope"),
            ));
        }
    }
    Ok(())
}

fn verify_func(ctx: &Ctx<'_>, func: &Func, fidx: u32) -> Result<(), VerifyError> {
    let module = ctx.module;
    // Reject a forged slot count before any allocation is sized from
    // it. The dataflow pass allocates one state cell per block and
    // local, so both bounds run first.
    if func.local_count() > MAX_LOCAL_SLOTS {
        return Err(err(
            fidx,
            format!(
                "the local slot count {} exceeds the portable limit {MAX_LOCAL_SLOTS}",
                func.local_count()
            ),
        ));
    }
    if (func.blocks.len() as u64) * (func.local_count() as u64 + 1) > MAX_DATAFLOW_CELLS {
        return Err(err(
            fidx,
            "the function exceeds the verifier state budget; split it",
        ));
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
            let at_dyn: &dyn Fn(&str) -> String = &at;
            match instr {
                Instr::ConstStr(idx) => {
                    if *idx as usize >= module.strings.len() {
                        return Err(err(fidx, at("string index out of range")));
                    }
                }
                Instr::LoadLocal(slot) | Instr::StoreLocal(slot) => {
                    if *slot >= func.local_count() {
                        return Err(err(fidx, at("local slot out of range")));
                    }
                }
                Instr::Call(callee) => {
                    let Some(target) = module.funcs.get(*callee as usize) else {
                        return Err(err(fidx, at("call target out of range")));
                    };
                    if !target.captures.is_empty() {
                        return Err(err(fidx, at("direct call to a function with captures")));
                    }
                    if target.type_params != 0 || target.effect_params != 0 {
                        return Err(err(fidx, at("a generic callee needs a type application")));
                    }
                }
                Instr::CallG { func: callee, app } => {
                    let Some(target) = module.funcs.get(*callee as usize) else {
                        return Err(err(fidx, at("call target out of range")));
                    };
                    if !target.captures.is_empty() {
                        return Err(err(fidx, at("direct call to a function with captures")));
                    }
                    if target.type_params == 0 && target.effect_params == 0 {
                        return Err(err(fidx, at("a type application on a non-generic callee")));
                    }
                    check_app(
                        ctx,
                        func,
                        fidx,
                        at_dyn,
                        *app,
                        target.type_params,
                        target.effect_params,
                    )?;
                }
                Instr::CallVirtual { selector, .. } => {
                    if *selector as usize >= module.selectors.len() {
                        return Err(err(fidx, at("selector index out of range")));
                    }
                }
                Instr::CallVirtualG { selector, app, .. } => {
                    if *selector as usize >= module.selectors.len() {
                        return Err(err(fidx, at("selector index out of range")));
                    }
                    // The full arity check needs the receiver type and
                    // runs in the dataflow pass. The structural pass
                    // bounds the index and the variable scopes, so the
                    // dataflow pass can index the table safely.
                    let Some(a) = module.apps.get(*app as usize) else {
                        return Err(err(fidx, at("type application index out of range")));
                    };
                    for t in &a.types {
                        if !ctx.vars_bounded(*t, func.type_params, func.effect_params) {
                            return Err(err(
                                fidx,
                                at("type application uses a variable outside the caller scope"),
                            ));
                        }
                    }
                    for row in &a.rows {
                        if !ctx.row_vars_bounded(row, func.effect_params) {
                            return Err(err(
                                fidx,
                                at("type application row uses a variable outside the caller scope"),
                            ));
                        }
                    }
                }
                Instr::MakeClosure { func: f, captures } => {
                    let Some(target) = module.funcs.get(*f as usize) else {
                        return Err(err(fidx, at("closure function out of range")));
                    };
                    if target.captures.len() != *captures as usize {
                        return Err(err(fidx, at("closure capture count mismatch")));
                    }
                    if target.type_params != func.type_params
                        || target.effect_params != func.effect_params
                    {
                        return Err(err(
                            fidx,
                            at("a closure body must keep the enclosing generic arity"),
                        ));
                    }
                }
                Instr::LoadCapture(idx) => {
                    if *idx as usize >= func.captures.len() {
                        return Err(err(fidx, at("capture index out of range")));
                    }
                }
                Instr::New(class) => {
                    let Some(c) = module.classes.get(*class as usize) else {
                        return Err(err(fidx, at("class index out of range")));
                    };
                    if c.kind == BcClassKind::Abstract {
                        return Err(err(fidx, at("cannot allocate an abstract enum parent")));
                    }
                    if c.type_params != 0 {
                        return Err(err(fidx, at("a generic class needs a type application")));
                    }
                }
                Instr::NewG { class, app } => {
                    let Some(c) = module.classes.get(*class as usize) else {
                        return Err(err(fidx, at("class index out of range")));
                    };
                    if c.kind == BcClassKind::Abstract {
                        return Err(err(fidx, at("cannot allocate an abstract enum parent")));
                    }
                    if c.type_params == 0 {
                        return Err(err(fidx, at("a type application on a non-generic class")));
                    }
                    check_app(ctx, func, fidx, at_dyn, *app, c.type_params, 0)?;
                }
                Instr::ListNew { ty, .. }
                | Instr::MapNew { ty, .. }
                | Instr::TupleNew { ty, .. }
                | Instr::IsType(ty)
                | Instr::CastType(ty) => {
                    if *ty as usize >= module.types.len() {
                        return Err(err(fidx, at("type index out of range")));
                    }
                }
                Instr::Jump(target) | Instr::JumpIfFalse(target) | Instr::JumpIfTrue(target) => {
                    if *target as usize >= func.blocks.len() {
                        return Err(err(fidx, at("jump target is not a block")));
                    }
                }
                Instr::Perform { op, argc } => {
                    if *op >= lm_abi::OP_COUNT {
                        return Err(err(fidx, at("perform operation slot out of range")));
                    }
                    let want = perform_argc(*op);
                    if *argc != want {
                        return Err(err(fidx, at("perform argument count mismatch")));
                    }
                }
                Instr::OpConst(op) | Instr::AsCall(op) => {
                    if *op >= lm_abi::OP_COUNT || lm_abi::op(*op).kind != lm_abi::OpKind::Fixed {
                        return Err(err(
                            fidx,
                            at("first-class operation slot is out of range or not fixed"),
                        ));
                    }
                }
                Instr::TableEdit { action, kind, slot } => {
                    if *action > 3 || *kind > 1 {
                        return Err(err(fidx, at("invalid table edit encoding")));
                    }
                    let bound = if *kind == 0 {
                        lm_abi::OP_COUNT
                    } else {
                        lm_abi::GROUP_COUNT
                    };
                    if *slot >= bound {
                        return Err(err(fidx, at("table edit target out of range")));
                    }
                    if *action == 2
                        && (*kind != 0 || lm_abi::op(*slot).kind != lm_abi::OpKind::Fixed)
                    {
                        return Err(err(
                            fidx,
                            at("a mock target must be an exact fixed operation"),
                        ));
                    }
                }
                _ => {}
            }
        }
    }
    // Dataflow pass: reconstruct types at every reachable block entry.
    let mut states: Vec<Option<State>> = vec![None; func.blocks.len()];
    let mut locals = vec![None; func.local_count() as usize];
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
            BcType::List(e) => Ok(e),
            _ => Err(fail(format!("expected a list type, found type {ty}"))),
        }
    };
    let as_map = |ty: u32| -> Result<(u32, u32), VerifyError> {
        match ctx.ty(ty) {
            BcType::Map(k, v) => Ok((k, v)),
            _ => Err(fail(format!("expected a map type, found type {ty}"))),
        }
    };
    // The claimed row of a call must sit inside the caller's row.
    let charge_row = |row: &[BcRow]| -> Result<(), VerifyError> {
        if ctx.row_included(row, &func.row) {
            Ok(())
        } else {
            Err(fail(
                "the callee row is not inside the caller's declared row".to_string(),
            ))
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
            // The declared local-type table is the typing judgment:
            // a store must fit the declared slot type, and the slot
            // holds the declared type afterwards. This keeps a
            // widened local at its declared type instead of the
            // concrete stored type.
            let ty = pop(state)?;
            let declared = func.local_types[*slot as usize];
            if !ctx.is_subtype(ty, declared) {
                return Err(fail(format!(
                    "store to local {slot} expects the declared type {declared}, \
                     found type {ty}"
                )));
            }
            state.locals[*slot as usize] = Some(declared);
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
            let excluded = |t: u32| matches!(ctx.ty(t), BcType::Str | BcType::Tuple(_));
            let heap_ok = ctx.is_heap(a) && ctx.is_heap(b) && !excluded(a) && !excluded(b);
            if !heap_ok || !(ctx.is_subtype(a, b) || ctx.is_subtype(b, a)) {
                return Err(fail(format!(
                    "reference equality needs related object types, found {a} and {b}"
                )));
            }
            push(state, TY_BOOL)?;
        }
        Instr::Call(callee) => {
            let sig = &module.funcs[*callee as usize];
            charge_row(&sig.row)?;
            pop_args(state, &sig.params)?;
            push(state, sig.ret)?;
        }
        Instr::CallG { func: callee, app } => {
            let sig = &module.funcs[*callee as usize];
            let app = &module.apps[*app as usize];
            let row = ctx.row_subst(&sig.row, &app.rows);
            charge_row(&row)?;
            let params: Vec<u32> = sig
                .params
                .iter()
                .map(|p| ctx.subst(*p, &app.types, &app.rows))
                .collect();
            pop_args(state, &params)?;
            let ret = ctx.subst(sig.ret, &app.types, &app.rows);
            push(state, ret)?;
        }
        Instr::CallVirtual { selector, argc } => {
            let argc = *argc as usize;
            if state.stack.len() < argc + 1 {
                return Err(fail("virtual call on a short stack".to_string()));
            }
            let recv_ty = state.stack[state.stack.len() - 1 - argc];
            let class = match ctx.ty(recv_ty) {
                BcType::Class(c) => c,
                _ => {
                    return Err(fail(format!(
                        "virtual call receiver type {recv_ty} needs the generic form \
                         or is not a class"
                    )));
                }
            };
            let target = ctx
                .find_method(class, *selector)
                .ok_or_else(|| fail(format!("selector {selector} is not a class method")))?;
            let sig = &module.funcs[target as usize];
            if sig.type_params != 0 || sig.effect_params != 0 {
                return Err(fail(
                    "a generic method call needs a type application".to_string(),
                ));
            }
            charge_row(&sig.row)?;
            if sig.params.len() != argc + 1 {
                return Err(fail("virtual call argument count mismatch".to_string()));
            }
            pop_args(state, &sig.params[1..])?;
            pop_expect(state, sig.params[0])?;
            push(state, sig.ret)?;
        }
        Instr::CallVirtualG {
            selector,
            argc,
            app,
        } => {
            let argc = *argc as usize;
            if state.stack.len() < argc + 1 {
                return Err(fail("virtual call on a short stack".to_string()));
            }
            let recv_ty = state.stack[state.stack.len() - 1 - argc];
            let Some((class, class_args)) = ctx.as_instance(recv_ty) else {
                return Err(fail(format!(
                    "virtual call receiver type {recv_ty} is not a class"
                )));
            };
            let target = ctx
                .find_method(class, *selector)
                .ok_or_else(|| fail(format!("selector {selector} is not a class method")))?;
            let sig = &module.funcs[target as usize];
            let app = &module.apps[*app as usize];
            let mut targs = class_args;
            targs.extend_from_slice(&app.types);
            if sig.type_params as usize != targs.len()
                || sig.effect_params as usize != app.rows.len()
            {
                return Err(fail(
                    "virtual call type application arity mismatch".to_string(),
                ));
            }
            let row = ctx.row_subst(&sig.row, &app.rows);
            charge_row(&row)?;
            if sig.params.len() != argc + 1 {
                return Err(fail("virtual call argument count mismatch".to_string()));
            }
            let params: Vec<u32> = sig
                .params
                .iter()
                .map(|p| ctx.subst(*p, &targs, &app.rows))
                .collect();
            pop_args(state, &params[1..])?;
            pop_expect(state, params[0])?;
            let ret = ctx.subst(sig.ret, &targs, &app.rows);
            push(state, ret)?;
        }
        Instr::CallValue { argc } => {
            let argc = *argc as usize;
            if state.stack.len() < argc + 1 {
                return Err(fail("closure call on a short stack".to_string()));
            }
            let callee_ty = state.stack[state.stack.len() - 1 - argc];
            let (params, ret, row) = match ctx.ty(callee_ty) {
                BcType::Fn(params, _, ret, row) => (params, ret, row),
                _ => {
                    return Err(fail(format!(
                        "closure call target type {callee_ty} is not a function type"
                    )));
                }
            };
            charge_row(&row)?;
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
            let fn_ty = BcType::Fn(
                target.params.clone(),
                target.param_muts.clone(),
                target.ret,
                target.row.clone(),
            );
            let idx = {
                let uni = ctx.uni.borrow();
                uni.index.get(&fn_ty).copied()
            };
            let idx = idx.filter(|i| (*i as usize) < module.types.len());
            let idx = idx.ok_or_else(|| {
                fail("the closure function type is not in the type table".to_string())
            })?;
            push(state, idx)?;
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
        Instr::NewG { class, app } => {
            let app = &module.apps[*app as usize];
            let ty = ctx.intern(BcType::Inst(*class, app.types.clone()));
            push(state, ty)?;
        }
        Instr::LoadField(field) => {
            let recv = pop(state)?;
            let Some((class, class_args)) = ctx.as_instance(recv) else {
                return Err(fail(format!("field load on non-class type {recv}")));
            };
            let fields = &module.classes[class as usize].fields;
            let (_, fty) = fields
                .get(*field as usize)
                .ok_or_else(|| fail("field index out of range".to_string()))?;
            let fty = ctx.subst(*fty, &class_args, &[]);
            push(state, fty)?;
        }
        Instr::StoreField(field) => {
            let value = pop(state)?;
            let recv = pop(state)?;
            let Some((class, class_args)) = ctx.as_instance(recv) else {
                return Err(fail(format!("field store on non-class type {recv}")));
            };
            let fields = &module.classes[class as usize].fields;
            let (_, fty) = fields
                .get(*field as usize)
                .ok_or_else(|| fail("field index out of range".to_string()))?;
            let fty = ctx.subst(*fty, &class_args, &[]);
            if !ctx.is_subtype(value, fty) {
                return Err(fail(format!(
                    "field store expects type {fty}, found type {value}"
                )));
            }
        }
        Instr::TupleNew { ty, count } => {
            let elems = match ctx.ty(*ty) {
                BcType::Tuple(elems) => elems,
                _ => return Err(fail(format!("expected a tuple type, found type {ty}"))),
            };
            if elems.len() != *count as usize {
                return Err(fail("tuple arity does not match its type".to_string()));
            }
            for want in elems.iter().rev() {
                pop_expect(state, *want)?;
            }
            push(state, *ty)?;
        }
        Instr::TupleGet(index) => {
            let t = pop(state)?;
            let elems = match ctx.ty(t) {
                BcType::Tuple(elems) => elems,
                _ => return Err(fail(format!("tuple read on non-tuple type {t}"))),
            };
            let elem = elems
                .get(*index as usize)
                .ok_or_else(|| fail("tuple index out of range".to_string()))?;
            push(state, *elem)?;
        }
        Instr::IsType(ty) | Instr::CastType(ty) => {
            let value = pop(state)?;
            let Some((vc, va)) = ctx.as_instance(value) else {
                return Err(fail(format!("type test on non-instance type {value}")));
            };
            let Some((tc, ta)) = ctx.as_instance(*ty) else {
                return Err(fail(format!(
                    "type test target {ty} is not an instance type"
                )));
            };
            // Sibling enum cases share their family parent, so a test
            // between them is legal and false at run time. The
            // exhaustiveness backstop emits such tests on flow-narrowed
            // values. Classes without a common ancestor stay rejected.
            if ctx.common_ancestor(vc, tc).is_none() {
                return Err(fail("type test between unrelated classes".to_string()));
            }
            // Class arguments are invariant, and every legal nominal
            // relation in this slice keeps the argument vector. A test
            // that changes an argument would forge a generic type.
            if va != ta {
                return Err(fail("type test changes the generic arguments".to_string()));
            }
            match instr {
                Instr::IsType(_) => push(state, TY_BOOL)?,
                _ => push(state, *ty)?,
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
            let idx = {
                let uni = ctx.uni.borrow();
                uni.index.get(&BcType::StringBuilder).copied()
            };
            let idx =
                idx.ok_or_else(|| fail("StringBuilder is not in the type table".to_string()))?;
            push(state, idx)?;
        }
        Instr::SbAppendStr | Instr::SbAppendInt | Instr::SbAppendBool => {
            let want = match instr {
                Instr::SbAppendStr => TY_STR,
                Instr::SbAppendInt => TY_INT,
                _ => TY_BOOL,
            };
            pop_expect(state, want)?;
            let sb = pop(state)?;
            if ctx.ty(sb) != BcType::StringBuilder {
                return Err(fail(format!("append on non-builder type {sb}")));
            }
            push(state, sb)?;
        }
        Instr::SbBuild => {
            let sb = pop(state)?;
            if ctx.ty(sb) != BcType::StringBuilder {
                return Err(fail(format!("build on non-builder type {sb}")));
            }
            push(state, TY_STR)?;
        }
        Instr::BbNew => {
            let idx = {
                let uni = ctx.uni.borrow();
                uni.index.get(&BcType::ByteBuffer).copied()
            };
            let idx = idx.ok_or_else(|| fail("ByteBuffer is not in the type table".to_string()))?;
            push(state, idx)?;
        }
        Instr::BbAppend => {
            pop_expect(state, TY_INT)?;
            let bb = pop(state)?;
            if ctx.ty(bb) != BcType::ByteBuffer {
                return Err(fail(format!("append on non-buffer type {bb}")));
            }
            push(state, bb)?;
        }
        Instr::BbLen => {
            let bb = pop(state)?;
            if ctx.ty(bb) != BcType::ByteBuffer {
                return Err(fail(format!("len on non-buffer type {bb}")));
            }
            push(state, TY_INT)?;
        }
        Instr::BbBuild => {
            let bb = pop(state)?;
            if ctx.ty(bb) != BcType::ByteBuffer {
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
        Instr::Digest => {
            let ty = pop(state)?;
            if !ctx.is_heap(ty) {
                return Err(fail(format!("digest on non-object type {ty}")));
            }
            let idx = {
                let uni = ctx.uni.borrow();
                uni.index.get(&BcType::Digest).copied()
            };
            let idx = idx.ok_or_else(|| fail("Digest is not in the type table".to_string()))?;
            push(state, idx)?;
        }
        Instr::EqDigest | Instr::NeDigest => {
            let b = pop(state)?;
            let a = pop(state)?;
            if ctx.ty(a) != BcType::Digest || ctx.ty(b) != BcType::Digest {
                return Err(fail(format!(
                    "digest comparison on non-digest types {a} and {b}"
                )));
            }
            push(state, TY_BOOL)?;
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
        Instr::Perform { op, .. } => {
            let op = *op;
            let name = lm_abi::op_name(op);
            if !ctx.row_has_name(&func.row, &name) {
                return Err(fail(format!(
                    "the perform of `{name}` is not inside the claimed row"
                )));
            }
            let def = lm_abi::op(op);
            match def.kind {
                lm_abi::OpKind::Fixed => {
                    for want in def.params.iter().rev() {
                        let want = ctx.abi_ty(*want).map_err(&fail)?;
                        pop_expect(state, want)?;
                    }
                    let reply = ctx.abi_ty(def.reply).map_err(&fail)?;
                    push(state, reply)?;
                }
                lm_abi::OpKind::VmControl => {
                    let pop_vm = |state: &mut State| -> Result<u32, VerifyError> {
                        let v = pop(state)?;
                        match ctx.ty(v) {
                            BcType::Vm(t) => Ok(t),
                            _ => Err(fail(format!(
                                "`{name}` needs a loaded Vm receiver, found type {v}"
                            ))),
                        }
                    };
                    match op {
                        lm_abi::OP_VM_NEW => {
                            let empty = ctx.intern(BcType::EmptyVm);
                            push(state, empty)?;
                        }
                        lm_abi::OP_VM_FROM_OBJECT => {
                            let args_ty = pop(state)?;
                            let fn_ty = pop(state)?;
                            let recv = pop(state)?;
                            if ctx.ty(recv) != BcType::EmptyVm {
                                return Err(fail(
                                    "`Vm.FromObject` needs an EmptyVm receiver".to_string(),
                                ));
                            }
                            let BcType::Fn(params, _, ret, _) = ctx.ty(fn_ty) else {
                                return Err(fail(
                                    "`Vm.FromObject` needs a function value".to_string(),
                                ));
                            };
                            let want = if params.is_empty() {
                                TY_UNIT
                            } else {
                                ctx.intern(BcType::Tuple(params))
                            };
                            if !ctx.is_subtype(args_ty, want) {
                                return Err(fail(
                                    "`Vm.FromObject` arguments do not match the \
                                     program parameters"
                                        .to_string(),
                                ));
                            }
                            let vm = ctx.intern(BcType::Vm(ret));
                            push(state, vm)?;
                        }
                        lm_abi::OP_VM_RUN | lm_abi::OP_VM_STEP | lm_abi::OP_VM_DRIVE => {
                            let t = pop_vm(state)?;
                            let (parent, what) = match op {
                                lm_abi::OP_VM_RUN => (ctx.core.run_result, "RunResult"),
                                lm_abi::OP_VM_STEP => (ctx.core.step_event, "StepEvent"),
                                _ => (ctx.core.drive_event, "DriveEvent"),
                            };
                            let event = ctx.event_inst(parent, what, t).map_err(&fail)?;
                            push(state, event)?;
                        }
                        lm_abi::OP_VM_TABLE => {
                            pop_vm(state)?;
                            let table = ctx.intern(BcType::PolicyTable);
                            push(state, table)?;
                        }
                        lm_abi::OP_VM_ANSWER => {
                            let value = pop(state)?;
                            let call = pop(state)?;
                            pop_vm(state)?;
                            let BcType::PendingCall(_, reply) = ctx.ty(call) else {
                                return Err(fail(
                                    "`Vm.Answer` needs a PendingCall token".to_string(),
                                ));
                            };
                            if !ctx.is_subtype(value, reply) {
                                return Err(fail(format!(
                                    "`Vm.Answer` reply expects type {reply}, found \
                                     type {value}"
                                )));
                            }
                            push(state, TY_UNIT)?;
                        }
                        lm_abi::OP_VM_REJECT => {
                            let fault = pop(state)?;
                            let request = pop(state)?;
                            pop_vm(state)?;
                            if ctx.ty(fault) != BcType::Fault || ctx.ty(request) != BcType::Request
                            {
                                return Err(fail(
                                    "`Vm.Reject` needs a Request and a Fault".to_string(),
                                ));
                            }
                            push(state, TY_UNIT)?;
                        }
                        lm_abi::OP_VM_DISPATCH => {
                            let request = pop(state)?;
                            pop_vm(state)?;
                            if ctx.ty(request) != BcType::Request {
                                return Err(fail("`Vm.Dispatch` needs a Request".to_string()));
                            }
                            push(state, TY_UNIT)?;
                        }
                        _ => unreachable!("every VmControl slot has a rule"),
                    }
                }
            }
        }
        Instr::PerformValue { argc } => {
            let argc = *argc as usize;
            if state.stack.len() < argc + 1 {
                return Err(fail("perform through a value on a short stack".to_string()));
            }
            let callee_ty = state.stack[state.stack.len() - 1 - argc];
            let BcType::Op(op, fn_ty) = ctx.ty(callee_ty) else {
                return Err(fail(format!(
                    "perform target type {callee_ty} is not an operation value"
                )));
            };
            let name = lm_abi::op_name(op);
            if !ctx.row_has_name(&func.row, &name) {
                return Err(fail(format!(
                    "the perform of `{name}` is not inside the claimed row"
                )));
            }
            let BcType::Fn(params, _, ret, _) = ctx.ty(fn_ty) else {
                unreachable!("a verified Op type embeds a function type");
            };
            if params.len() != argc {
                return Err(fail("perform argument count mismatch".to_string()));
            }
            pop_args(state, &params)?;
            pop(state)?;
            push(state, ret)?;
        }
        Instr::OpConst(op) => {
            let sig = ctx.fixed_sig_type(*op).map_err(&fail)?;
            let ty = ctx.intern(BcType::Op(*op, sig));
            push(state, ty)?;
        }
        Instr::TableEdit { action, kind, slot } => {
            if *action == 2 {
                let handler = pop(state)?;
                let want = ctx.fixed_sig_type(*slot).map_err(&fail)?;
                if !ctx.is_subtype(handler, want) {
                    return Err(fail(format!(
                        "a mock handler must have the exact operation signature \
                         with an empty row, found type {handler}"
                    )));
                }
            }
            let table = pop(state)?;
            if ctx.ty(table) != BcType::PolicyTable {
                return Err(fail(format!("table edit on non-table type {table}")));
            }
            if *action == 0 {
                // The dependent grant rule: `pass` is charged to the
                // granter's claimed row.
                let name = if *kind == 0 {
                    lm_abi::op_name(*slot)
                } else {
                    lm_abi::GROUPS[*slot as usize].to_string()
                };
                if !ctx.row_has_name(&func.row, &name) {
                    return Err(fail(format!(
                        "the pass of `{name}` is not inside the claimed row"
                    )));
                }
            }
            push(state, TY_UNIT)?;
        }
        Instr::AsCall(op) => {
            let request = pop(state)?;
            if ctx.ty(request) != BcType::Request {
                return Err(fail(format!("as_call on non-request type {request}")));
            }
            let view = ctx.op_args_view(*op).map_err(&fail)?;
            let def = lm_abi::op(*op);
            let reply = ctx.abi_ty(def.reply).map_err(&fail)?;
            let call = ctx.intern(BcType::PendingCall(view, reply));
            let out = ctx
                .event_inst(ctx.core.option, "Option", call)
                .map_err(&fail)?;
            push(state, out)?;
        }
        Instr::CallArgs => {
            let call = pop(state)?;
            let BcType::PendingCall(view, _) = ctx.ty(call) else {
                return Err(fail(format!("args view on non-call type {call}")));
            };
            push(state, view)?;
        }
        Instr::FaultCode => {
            let fault = pop(state)?;
            if ctx.ty(fault) != BcType::Fault {
                return Err(fail(format!("fault code on non-fault type {fault}")));
            }
            push(state, TY_STR)?;
        }
        Instr::Unreachable => {
            // A diverging terminator: no stack effect, no successor.
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lm_bytecode::{BcClass, Func, Instr::*, Module, TypeApp, NO_PARENT};

    fn base_types() -> Vec<BcType> {
        vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str]
    }

    fn plain_func(name: &str, params: Vec<u32>, ret: u32, blocks: Vec<Vec<Instr>>) -> Func {
        Func {
            name: name.to_string(),
            type_params: 0,
            effect_params: 0,
            param_muts: vec![false; params.len()],
            local_types: {
                let mut locals = params.clone();
                locals.resize(2, TY_INT);
                locals
            },
            params,
            ret,
            row: vec![],
            captures: vec![],
            blocks,
        }
    }

    fn module_with(blocks: Vec<Vec<Instr>>) -> Module {
        Module {
            strings: vec!["s".to_string()],
            types: base_types(),
            selectors: vec![],
            apps: vec![],
            classes: vec![],
            funcs: vec![plain_func("main", vec![], TY_INT, blocks)],
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
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
            apps: vec![],
            classes: vec![BcClass {
                name: "Counter".to_string(),
                key: "Counter".to_string(),
                parent: NO_PARENT,
                type_params: 0,
                kind: BcClassKind::Normal,
                fields: vec![("value".to_string(), TY_INT)],
                methods: vec![(0, 1)],
            }],
            funcs: vec![
                plain_func("main", vec![], TY_INT, entry_blocks),
                plain_func(
                    "bump",
                    vec![4],
                    TY_INT,
                    vec![vec![LoadLocal(0), LoadField(0), Return]],
                ),
            ],
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
        }
    }

    /// A generic module: `Box[T] { value: T }`, its `<new>` function,
    /// and an entry that builds `Box[Int]` and reads the field.
    fn generic_module(entry_blocks: Vec<Vec<Instr>>) -> Module {
        let mut types = base_types();
        types.push(BcType::Var(0)); // 4
        types.push(BcType::Inst(0, vec![4])); // 5 Box[$0]
        types.push(BcType::Inst(0, vec![TY_INT])); // 6 Box[Int]
        Module {
            strings: vec![],
            types,
            selectors: vec![],
            apps: vec![
                TypeApp {
                    types: vec![TY_INT],
                    rows: vec![],
                },
                TypeApp {
                    types: vec![4],
                    rows: vec![],
                },
            ],
            classes: vec![BcClass {
                name: "Box".to_string(),
                key: "Box".to_string(),
                parent: NO_PARENT,
                type_params: 1,
                kind: BcClassKind::Normal,
                fields: vec![("value".to_string(), 4)],
                methods: vec![],
            }],
            funcs: vec![
                plain_func("main", vec![], TY_INT, entry_blocks),
                Func {
                    name: "<new Box>".to_string(),
                    type_params: 1,
                    effect_params: 0,
                    params: vec![4],
                    param_muts: vec![false],
                    ret: 5,
                    row: vec![],
                    captures: vec![],
                    local_types: vec![4, 5],
                    blocks: vec![vec![
                        NewG { class: 0, app: 1 },
                        StoreLocal(1),
                        LoadLocal(1),
                        LoadLocal(0),
                        StoreField(0),
                        LoadLocal(1),
                        Return,
                    ]],
                },
            ],
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
        }
    }

    #[test]
    fn accepts_simple_function() {
        let m = module_with(vec![vec![ConstInt(1), ConstInt(2), Add, Return]]);
        assert!(verify_module(&m).is_ok());
    }

    #[test]
    fn accepts_class_construction_and_virtual_call() {
        let mut m = class_module(vec![vec![
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
        // The entry stores a Counter into local 0, so the declared
        // slot type must accept it.
        m.funcs[0].local_types = vec![4, TY_INT];
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    #[test]
    fn accepts_generic_construction_and_field_read() {
        let m = generic_module(vec![vec![
            ConstInt(41),
            CallG { func: 1, app: 0 },
            LoadField(0),
            Return,
        ]]);
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    #[test]
    fn rejects_generic_call_with_wrong_result_use() {
        // Box[Int].value is Int; using it as Bool must fail.
        let m = generic_module(vec![vec![
            ConstInt(41),
            CallG { func: 1, app: 0 },
            LoadField(0),
            Not,
            Pop,
            ConstInt(0),
            Return,
        ]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("expected type"), "{e}");
    }

    #[test]
    fn rejects_generic_call_without_application() {
        let m = generic_module(vec![vec![ConstInt(41), Call(1), LoadField(0), Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("type application"), "{e}");
    }

    #[test]
    fn rejects_application_arity_mismatch() {
        let mut m = generic_module(vec![vec![
            ConstInt(41),
            CallG { func: 1, app: 0 },
            LoadField(0),
            Return,
        ]]);
        m.apps[0].types.push(TY_INT);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("arity"), "{e}");
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
    fn rejects_overlong_tuple_type() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types.push(BcType::Tuple(vec![TY_INT; 17]));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("arity"), "{e}");
    }

    #[test]
    fn rejects_tuple_get_out_of_range() {
        let mut m = module_with(vec![vec![
            ConstInt(1),
            ConstInt(2),
            TupleNew { ty: 4, count: 2 },
            TupleGet(5),
            Return,
        ]]);
        m.types.push(BcType::Tuple(vec![TY_INT, TY_INT]));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("tuple index"), "{e}");
    }

    #[test]
    fn rejects_tuple_new_count_mismatch() {
        let mut m = module_with(vec![vec![
            ConstInt(1),
            TupleNew { ty: 4, count: 1 },
            TupleGet(0),
            Return,
        ]]);
        m.types.push(BcType::Tuple(vec![TY_INT, TY_INT]));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("arity"), "{e}");
    }

    #[test]
    fn rejects_new_on_abstract_class() {
        let mut m = class_module(vec![vec![New(1), Pop, ConstInt(0), Return]]);
        m.classes.push(BcClass {
            name: "Opt".to_string(),
            key: "Opt".to_string(),
            parent: NO_PARENT,
            type_params: 0,
            kind: BcClassKind::Abstract,
            fields: vec![],
            methods: vec![],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("abstract"), "{e}");
    }

    #[test]
    fn rejects_subclass_of_case_class() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        m.classes.push(BcClass {
            name: "Opt".to_string(),
            key: "Opt".to_string(),
            parent: NO_PARENT,
            type_params: 0,
            kind: BcClassKind::Abstract,
            fields: vec![],
            methods: vec![],
        });
        m.classes.push(BcClass {
            name: "Opt.None".to_string(),
            key: "Opt.None".to_string(),
            parent: 1,
            type_params: 0,
            kind: BcClassKind::Case,
            fields: vec![],
            methods: vec![],
        });
        m.classes.push(BcClass {
            name: "Bad".to_string(),
            key: "Bad".to_string(),
            parent: 2,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("case"), "{e}");
    }

    #[test]
    fn rejects_type_test_between_unrelated_classes() {
        let mut m = class_module(vec![vec![New(0), IsType(5), Pop, ConstInt(0), Return]]);
        m.types.push(BcType::Class(1)); // 5
        m.classes.push(BcClass {
            name: "Other".to_string(),
            key: "Other".to_string(),
            parent: NO_PARENT,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("unrelated"), "{e}");
    }

    #[test]
    fn rejects_row_not_inside_caller() {
        // Callee claims Io.Print; caller declares the empty row.
        let mut m = module_with(vec![vec![Call(1), Return]]);
        m.strings = vec!["Io.Print".to_string()];
        m.funcs.push(Func {
            name: "printer".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![BcRow::Op(0)],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![ConstInt(1), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("row"), "{e}");
    }

    #[test]
    fn accepts_row_inside_caller_with_group() {
        // Caller declares Io; callee claims Io.Print.
        let mut m = module_with(vec![vec![Call(1), Return]]);
        m.strings = vec!["Io.Print".to_string(), "Io".to_string()];
        m.funcs[0].row = vec![BcRow::Op(1)];
        m.funcs.push(Func {
            name: "printer".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![BcRow::Op(0)],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![ConstInt(1), Return]],
        });
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    #[test]
    fn rejects_non_canonical_declared_row() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.strings = vec!["Io".to_string(), "Fs".to_string()];
        m.funcs[0].row = vec![BcRow::Op(0), BcRow::Op(1)]; // Io before Fs
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("canonical"), "{e}");
    }

    #[test]
    fn rejects_row_var_outside_arity() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.funcs[0].row = vec![BcRow::Var(0)];
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("effect variable"), "{e}");
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
            key: "Fast".to_string(),
            parent: 0,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![("value".to_string(), TY_INT)],
            methods: vec![(0, 2)],
        });
        m.funcs.push(plain_func(
            "bump2",
            vec![5, TY_INT],
            TY_INT,
            vec![vec![ConstInt(1), Return]],
        ));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("parameter types"), "{e}");
    }

    #[test]
    fn rejects_override_that_widens_the_row() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        m.strings = vec!["Io.Print".to_string()];
        m.types.push(BcType::Class(1)); // 5
        m.classes.push(BcClass {
            name: "Loud".to_string(),
            key: "Loud".to_string(),
            parent: 0,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![("value".to_string(), TY_INT)],
            methods: vec![(0, 2)],
        });
        m.funcs.push(Func {
            name: "bump2".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![5],
            param_muts: vec![false],
            ret: TY_INT,
            row: vec![BcRow::Op(0)],
            captures: vec![],
            local_types: vec![5],
            blocks: vec![vec![ConstInt(1), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("widens the effect row"), "{e}");
    }

    #[test]
    fn rejects_layout_that_breaks_parent_prefix() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        m.types.push(BcType::Class(1));
        m.classes.push(BcClass {
            name: "Bad".to_string(),
            key: "Bad".to_string(),
            parent: 0,
            type_params: 0,
            kind: BcClassKind::Normal,
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
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![],
            captures: vec![TY_INT],
            local_types: vec![],
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
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![],
            captures: vec![TY_INT],
            local_types: vec![],
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
        m.types
            .push(BcType::Fn(vec![TY_INT], vec![false], TY_INT, vec![]));
        m.funcs.push(Func {
            name: "closure".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![TY_INT],
            param_muts: vec![false],
            ret: TY_INT,
            row: vec![],
            captures: vec![TY_INT],
            local_types: vec![TY_INT],
            blocks: vec![vec![LoadCapture(0), LoadLocal(0), Add, Return]],
        });
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    #[test]
    fn rejects_call_value_row_outside_caller() {
        let mut m = module_with(vec![vec![
            MakeClosure {
                func: 1,
                captures: 0,
            },
            CallValue { argc: 0 },
            Return,
        ]]);
        m.strings = vec!["Io.Print".to_string()];
        m.types
            .push(BcType::Fn(vec![], vec![], TY_INT, vec![BcRow::Op(0)]));
        m.funcs.push(Func {
            name: "printer".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![BcRow::Op(0)],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![ConstInt(1), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("row"), "{e}");
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
        m.funcs[0].param_muts = vec![false];
        m.funcs[0].local_types = vec![TY_INT, TY_INT];
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("entry function"), "{e}");
    }

    #[test]
    fn rejects_generic_entry() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.funcs[0].type_params = 1;
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("generic"), "{e}");
    }

    #[test]
    fn joins_subclass_stacks_at_merge_points() {
        // Both branches push a different subclass of Animal. The join
        // must settle at the common ancestor.
        let mut types = base_types();
        types.push(BcType::Class(0)); // 4 Animal
        types.push(BcType::Class(1)); // 5 Dog
        types.push(BcType::Class(2)); // 6 Cat
        let class = |name: &str, parent: u32| BcClass {
            name: name.to_string(),
            key: name.to_string(),
            parent,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        };
        let m = Module {
            strings: vec![],
            types,
            selectors: vec![],
            apps: vec![],
            classes: vec![class("Animal", NO_PARENT), class("Dog", 0), class("Cat", 0)],
            funcs: vec![Func {
                name: "main".to_string(),
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: TY_UNIT,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![
                    vec![ConstBool(true), JumpIfFalse(1), New(1), Jump(2)],
                    vec![New(2), Jump(2)],
                    vec![Pop, ConstUnit, Return],
                ],
            }],
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
        };
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }
}
