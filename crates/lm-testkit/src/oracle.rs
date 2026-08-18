//! A test-only typed-HIR evaluator for the pure subset.
//!
//! The oracle executes checked HIR directly, without bytecode, the
//! verifier, or the VM heap. Differential tests compare its terminal
//! value text against the verified-bytecode VM. It is not shipped in
//! `lm-cli` and is not a production path. Guest recursion runs on the
//! Rust stack under an explicit depth bound, so the corpus stays
//! shallow by construction.

use lm_hir::check_module;
use lm_hir::hir::*;
use lm_source::ast::BinOp;
use lm_types::{ClassKind, Type};
use std::cell::RefCell;
use std::rc::Rc;

/// The oracle guest call depth bound.
const MAX_DEPTH: u32 = 1_000;

/// One oracle value.
#[derive(Clone)]
enum OV {
    Unit,
    Bool(bool),
    Int(i64),
    Str(Rc<String>),
    Obj(Rc<RefCell<OObj>>),
}

struct OObj {
    frozen: bool,
    kind: OKind,
}

enum OKind {
    Instance { class: u32, fields: Vec<Option<OV>> },
    List(Vec<OV>),
    Map(Vec<(OV, OV)>),
    Tuple(Vec<OV>),
    Closure { func: u32, captures: Vec<OV> },
    Sb(String),
    Bb(Vec<u8>),
    Bytes(Vec<u8>),
}

/// Why evaluation left the normal path.
enum Stop {
    Fault(&'static str),
    /// An internal oracle limit, reported as a harness error.
    Limit(&'static str),
    Return(OV),
    Break,
    Continue,
}

type EResult = Result<OV, Stop>;

/// Run one program in the oracle. The result is the same terminal
/// text the VM prints: `Done(...)` or `Fault(Code)`. `Err` reports a
/// harness problem, not a guest outcome.
pub fn oracle_run(name: &str, text: &str) -> Result<String, String> {
    let _ = name;
    let ast = lm_source::parse::parse(text).map_err(|d| format!("parse: {}", d.message))?;
    let hir = check_module(&ast).map_err(|d| format!("check: {}", d.message))?;
    let oracle = Oracle { m: &hir };
    match oracle.call(hir.entry as u32, vec![], vec![], 0) {
        Ok(value) => Ok(format!("Done({})", oracle.show(&value))),
        Err(Stop::Fault(code)) => Ok(format!("Fault({code})")),
        Err(Stop::Limit(what)) => Err(format!("oracle limit: {what}")),
        Err(_) => Err("control escaped a callable".to_string()),
    }
}

struct Frame {
    locals: Vec<Option<OV>>,
    captures: Vec<OV>,
}

impl Frame {
    fn set(&mut self, slot: u32, value: OV) {
        let idx = slot as usize;
        if idx >= self.locals.len() {
            self.locals.resize(idx + 1, None);
        }
        self.locals[idx] = Some(value);
    }

    fn get(&self, slot: u32) -> Result<OV, Stop> {
        self.locals
            .get(slot as usize)
            .and_then(|v| v.clone())
            .ok_or(Stop::Limit("read of an unset local"))
    }
}

struct Oracle<'m> {
    m: &'m HirModule,
}

impl<'m> Oracle<'m> {
    fn call(&self, func: u32, args: Vec<OV>, captures: Vec<OV>, depth: u32) -> EResult {
        if depth > MAX_DEPTH {
            return Err(Stop::Limit("call depth"));
        }
        let f = &self.m.funcs[func as usize];
        let mut frame = Frame {
            locals: args.into_iter().map(Some).collect(),
            captures,
        };
        frame.locals.resize(f.locals.len(), None);
        let unit_ret = f.ret == lm_types::UNIT;
        match self.run_block(&f.body, &mut frame, depth, !unit_ret) {
            Ok(value) => Ok(if unit_ret { OV::Unit } else { value }),
            Err(Stop::Return(value)) => Ok(value),
            Err(other) => Err(other),
        }
    }

    /// Run one statement list. With `valued`, the final expression
    /// statement supplies the block value.
    fn run_block(&self, stmts: &[HStmt], frame: &mut Frame, depth: u32, valued: bool) -> EResult {
        let Some((last, init)) = stmts.split_last() else {
            return Ok(OV::Unit);
        };
        for stmt in init {
            self.run_stmt(stmt, frame, depth)?;
        }
        match last {
            HStmt::Expr(e) if valued => self.eval(e, frame, depth),
            stmt => {
                self.run_stmt(stmt, frame, depth)?;
                Ok(OV::Unit)
            }
        }
    }

    fn run_stmt(&self, stmt: &HStmt, frame: &mut Frame, depth: u32) -> Result<(), Stop> {
        match stmt {
            HStmt::Assign { slot, value } => {
                let value = self.eval(value, frame, depth)?;
                frame.set(*slot, value);
                Ok(())
            }
            HStmt::AssignField { recv, field, value } => {
                let recv = self.eval(recv, frame, depth)?;
                let value = self.eval(value, frame, depth)?;
                let obj = self.as_obj(&recv)?;
                if obj.borrow().frozen {
                    return Err(Stop::Fault("FrozenWrite"));
                }
                let mut borrow = obj.borrow_mut();
                match &mut borrow.kind {
                    OKind::Instance { fields, .. } => {
                        fields[*field as usize] = Some(value);
                        Ok(())
                    }
                    _ => Err(Stop::Limit("field write on a non-instance")),
                }
            }
            HStmt::While { cond, body } => {
                loop {
                    match self.eval(cond, frame, depth)? {
                        OV::Bool(true) => {}
                        OV::Bool(false) => break,
                        _ => return Err(Stop::Limit("non-Bool condition")),
                    }
                    match self.run_block(body, frame, depth, false) {
                        Ok(_) => {}
                        Err(Stop::Break) => break,
                        Err(Stop::Continue) => continue,
                        Err(other) => return Err(other),
                    }
                }
                Ok(())
            }
            HStmt::Return { value } => {
                let value = match value {
                    Some(v) => self.eval(v, frame, depth)?,
                    None => OV::Unit,
                };
                Err(Stop::Return(value))
            }
            HStmt::Break => Err(Stop::Break),
            HStmt::Continue => Err(Stop::Continue),
            HStmt::Expr(e) => {
                self.eval(e, frame, depth)?;
                Ok(())
            }
        }
    }

    fn as_obj(&self, value: &OV) -> Result<Rc<RefCell<OObj>>, Stop> {
        match value {
            OV::Obj(o) => Ok(o.clone()),
            _ => Err(Stop::Limit("expected an object value")),
        }
    }

    fn as_int(&self, value: &OV) -> Result<i64, Stop> {
        match value {
            OV::Int(v) => Ok(*v),
            _ => Err(Stop::Limit("expected an Int value")),
        }
    }

    fn alloc(&self, kind: OKind) -> OV {
        let frozen = matches!(
            kind,
            OKind::Tuple(_) | OKind::Closure { .. } | OKind::Bytes(_)
        );
        OV::Obj(Rc::new(RefCell::new(OObj { frozen, kind })))
    }

    fn construct(&self, class: u32, args: Vec<OV>, depth: u32) -> EResult {
        let c = &self.m.classes[class as usize];
        match c.native_repr {
            Some(NativeRepr::Int) => return Ok(OV::Int(0)),
            Some(NativeRepr::Bool) => return Ok(OV::Bool(false)),
            Some(NativeRepr::String) => return Ok(OV::Str(Rc::new(String::new()))),
            Some(NativeRepr::Bytes) => return Ok(self.alloc(OKind::Bytes(Vec::new()))),
            Some(NativeRepr::StringBuilder) => return Ok(self.alloc(OKind::Sb(String::new()))),
            Some(NativeRepr::ByteBuffer) => return Ok(self.alloc(OKind::Bb(Vec::new()))),
            None => {}
        }
        match c.ctor_kind {
            CtorKind::CaseFields => Ok(self.alloc(OKind::Instance {
                class,
                fields: args.into_iter().map(Some).collect(),
            })),
            CtorKind::Defaults | CtorKind::Init => {
                let mut fields: Vec<Option<OV>> = vec![None; c.field_tys.len()];
                let inst = self.alloc(OKind::Instance {
                    class,
                    fields: vec![],
                });
                for (fidx, default) in c.defaults.iter().enumerate() {
                    if let Some(expr) = default {
                        let mut scratch = Frame {
                            locals: vec![],
                            captures: vec![],
                        };
                        fields[fidx] = Some(self.eval(expr, &mut scratch, depth + 1)?);
                    }
                }
                if let OV::Obj(o) = &inst {
                    o.borrow_mut().kind = OKind::Instance { class, fields };
                }
                if let Some(init) = c.init {
                    let mut all = vec![inst.clone()];
                    all.extend(args);
                    self.call(init, all, vec![], depth + 1)?;
                }
                Ok(inst)
            }
        }
    }

    fn eval(&self, expr: &HExpr, frame: &mut Frame, depth: u32) -> EResult {
        match &expr.kind {
            HExprKind::Unit => Ok(OV::Unit),
            HExprKind::Int(v) => Ok(OV::Int(*v)),
            HExprKind::Bool(v) => Ok(OV::Bool(*v)),
            HExprKind::Str(v) => Ok(OV::Str(Rc::new(v.clone()))),
            HExprKind::Local(slot) => frame.get(*slot),
            HExprKind::Capture(idx) => Ok(frame.captures[*idx as usize].clone()),
            HExprKind::Not(inner) => match self.eval(inner, frame, depth)? {
                OV::Bool(v) => Ok(OV::Bool(!v)),
                _ => Err(Stop::Limit("non-Bool operand")),
            },
            HExprKind::Neg(inner) => {
                let v = self.as_int(&self.eval(inner, frame, depth)?)?;
                v.checked_neg()
                    .map(OV::Int)
                    .ok_or(Stop::Fault("IntegerOverflow"))
            }
            HExprKind::Binary {
                op,
                operand_ty,
                left,
                right,
            } => {
                let l = self.eval(left, frame, depth)?;
                let r = self.eval(right, frame, depth)?;
                self.binary(*op, *operand_ty, l, r)
            }
            HExprKind::And(left, right) => match self.eval(left, frame, depth)? {
                OV::Bool(false) => Ok(OV::Bool(false)),
                OV::Bool(true) => self.eval(right, frame, depth),
                _ => Err(Stop::Limit("non-Bool operand")),
            },
            HExprKind::Or(left, right) => match self.eval(left, frame, depth)? {
                OV::Bool(true) => Ok(OV::Bool(true)),
                OV::Bool(false) => self.eval(right, frame, depth),
                _ => Err(Stop::Limit("non-Bool operand")),
            },
            HExprKind::Call { func, args, .. } => {
                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(self.eval(arg, frame, depth)?);
                }
                self.call(*func, values, vec![], depth + 1)
            }
            HExprKind::Construct { class, args, .. } => {
                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(self.eval(arg, frame, depth)?);
                }
                self.construct(*class, values, depth)
            }
            HExprKind::MethodCall {
                recv,
                selector,
                args,
                ..
            } => {
                let recv_v = self.eval(recv, frame, depth)?;
                let mut values = Vec::with_capacity(args.len() + 1);
                values.push(recv_v.clone());
                for arg in args {
                    values.push(self.eval(arg, frame, depth)?);
                }
                let obj = self.as_obj(&recv_v)?;
                let class = match &obj.borrow().kind {
                    OKind::Instance { class, .. } => *class,
                    _ => return Err(Stop::Limit("method call on a non-instance")),
                };
                let func = self
                    .find_method(class, selector)
                    .ok_or(Stop::Limit("unknown selector"))?;
                self.call(func, values, vec![], depth + 1)
            }
            HExprKind::FieldGet { recv, field } => {
                let recv = self.eval(recv, frame, depth)?;
                let obj = self.as_obj(&recv)?;
                let out = match &obj.borrow().kind {
                    OKind::Instance { fields, .. } => fields[*field as usize].clone(),
                    _ => return Err(Stop::Limit("field read on a non-instance")),
                };
                out.ok_or(Stop::Fault("UninitializedField"))
            }
            HExprKind::MakeClosure { func, captures } => {
                let mut values = Vec::with_capacity(captures.len());
                for capture in captures {
                    values.push(self.eval(capture, frame, depth)?);
                }
                Ok(self.alloc(OKind::Closure {
                    func: *func,
                    captures: values,
                }))
            }
            HExprKind::CallValue { callee, args } => {
                let callee = self.eval(callee, frame, depth)?;
                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(self.eval(arg, frame, depth)?);
                }
                let obj = self.as_obj(&callee)?;
                let (func, captures) = match &obj.borrow().kind {
                    OKind::Closure { func, captures } => (*func, captures.clone()),
                    _ => return Err(Stop::Limit("call of a non-closure")),
                };
                self.call(func, values, captures, depth + 1)
            }
            HExprKind::TupleLit(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval(item, frame, depth)?);
                }
                Ok(self.alloc(OKind::Tuple(values)))
            }
            HExprKind::TupleGet { tuple, index } => {
                let tuple = self.eval(tuple, frame, depth)?;
                let obj = self.as_obj(&tuple)?;
                let out = match &obj.borrow().kind {
                    OKind::Tuple(items) => items[*index as usize].clone(),
                    _ => return Err(Stop::Limit("tuple read on a non-tuple")),
                };
                Ok(out)
            }
            HExprKind::IsType { value, ty } => {
                let v = self.eval(value, frame, depth)?;
                Ok(OV::Bool(self.instance_matches(&v, *ty)?))
            }
            HExprKind::CastType { value, ty } => {
                let v = self.eval(value, frame, depth)?;
                if self.instance_matches(&v, *ty)? {
                    Ok(v)
                } else {
                    Err(Stop::Fault("BadCast"))
                }
            }
            HExprKind::ListLit(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval(item, frame, depth)?);
                }
                Ok(self.alloc(OKind::List(values)))
            }
            HExprKind::MapLit(entries) => {
                let mut map: Vec<(OV, OV)> = Vec::new();
                for (k, v) in entries {
                    let k = self.eval(k, frame, depth)?;
                    let v = self.eval(v, frame, depth)?;
                    match map.iter_mut().find(|(mk, _)| self.key_eq(mk, &k)) {
                        Some(entry) => entry.1 = v,
                        None => map.push((k, v)),
                    }
                }
                Ok(self.alloc(OKind::Map(map)))
            }
            HExprKind::Native { op, args } => self.native(*op, args, frame, depth),
            HExprKind::Intrinsic { intrinsic, args } => {
                self.intrinsic(*intrinsic, args, frame, depth)
            }
            HExprKind::Interp(parts) => {
                let mut out = String::new();
                for part in parts {
                    match part {
                        HInterpPart::Lit(text) => out.push_str(text),
                        HInterpPart::Expr(e) => match self.eval(e, frame, depth)? {
                            OV::Int(v) => out.push_str(&v.to_string()),
                            OV::Bool(v) => out.push_str(if v { "true" } else { "false" }),
                            OV::Str(s) => out.push_str(&s),
                            _ => return Err(Stop::Limit("non-scalar interpolation")),
                        },
                    }
                }
                Ok(OV::Str(Rc::new(out)))
            }
            HExprKind::If { arms, else_body } => {
                let valued = expr.ty != lm_types::UNIT;
                for (cond, body) in arms {
                    match self.eval(cond, frame, depth)? {
                        OV::Bool(true) => return self.run_block(body, frame, depth, valued),
                        OV::Bool(false) => {}
                        _ => return Err(Stop::Limit("non-Bool condition")),
                    }
                }
                match else_body {
                    Some(body) => self.run_block(body, frame, depth, valued),
                    None => Ok(OV::Unit),
                }
            }
            HExprKind::Case {
                scrut,
                scrut_slot,
                arms,
            } => {
                let value = self.eval(scrut, frame, depth)?;
                frame.set(*scrut_slot, value.clone());
                for arm in arms {
                    if self.pattern_matches(&arm.pattern, &value, frame)? {
                        return self.run_block(&arm.body, frame, depth, true);
                    }
                }
                // The same backstop code the VM emits behind a proven
                // exhaustive `case`.
                Err(Stop::Fault("UnreachableCode"))
            }
            HExprKind::Perform { .. }
            | HExprKind::Spawn { .. }
            | HExprKind::OpConst(_)
            | HExprKind::TableEdit { .. }
            | HExprKind::CallArgs { .. }
            | HExprKind::FaultCodeGet { .. }
            | HExprKind::FaultDenied { .. }
            | HExprKind::RequestOpName { .. } => Err(Stop::Limit(
                "the oracle models the pure subset only; programs with performs \
                 are outside the oracle",
            )),
        }
    }

    fn find_method(&self, mut class: u32, selector: &str) -> Option<u32> {
        loop {
            let c = &self.m.classes[class as usize];
            if let Some((_, func)) = c.methods.iter().find(|(name, _)| name == selector) {
                return Some(*func);
            }
            match c.parent {
                Some(p) => class = p,
                None => return None,
            }
        }
    }

    fn class_extends(&self, mut child: u32, ancestor: u32) -> bool {
        loop {
            if child == ancestor {
                return true;
            }
            match self.m.classes[child as usize].parent {
                Some(p) => child = p,
                None => return false,
            }
        }
    }

    fn instance_matches(&self, value: &OV, ty: lm_types::TypeId) -> Result<bool, Stop> {
        let target = match self.m.store.get(ty) {
            Type::Class(c) => c.0,
            Type::Inst(c, _) => c.0,
            _ => return Err(Stop::Limit("type test on a non-nominal target")),
        };
        let obj = self.as_obj(value)?;
        let class = match &obj.borrow().kind {
            OKind::Instance { class, .. } => *class,
            _ => return Err(Stop::Limit("type test on a non-instance")),
        };
        Ok(self.class_extends(class, target))
    }

    fn pattern_matches(
        &self,
        pattern: &HPattern,
        value: &OV,
        frame: &mut Frame,
    ) -> Result<bool, Stop> {
        match pattern {
            HPattern::Wildcard => Ok(true),
            HPattern::Bind(slot) => {
                frame.set(*slot, value.clone());
                Ok(true)
            }
            HPattern::Int(want) => Ok(matches!(value, OV::Int(v) if v == want)),
            HPattern::Bool(want) => Ok(matches!(value, OV::Bool(v) if v == want)),
            HPattern::Str(want) => Ok(matches!(value, OV::Str(v) if v.as_str() == want)),
            HPattern::Project { .. } | HPattern::And(_) => {
                Err(Stop::Limit("request patterns run in the VM only"))
            }
            HPattern::Tuple { elems, .. } => {
                let obj = self.as_obj(value)?;
                let items = match &obj.borrow().kind {
                    OKind::Tuple(items) => items.clone(),
                    _ => return Err(Stop::Limit("tuple pattern on a non-tuple")),
                };
                for (sub, item) in elems.iter().zip(items.iter()) {
                    if !self.pattern_matches(sub, item, frame)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            HPattern::Ctor { class, args, .. } => {
                let obj = self.as_obj(value)?;
                let (vclass, fields) = match &obj.borrow().kind {
                    OKind::Instance { class, fields } => (*class, fields.clone()),
                    _ => return Err(Stop::Limit("constructor pattern on a non-instance")),
                };
                if !self.class_extends(vclass, *class) {
                    return Ok(false);
                }
                for (sub, field) in args.iter().zip(fields.iter()) {
                    let field = field.clone().ok_or(Stop::Fault("UninitializedField"))?;
                    if !self.pattern_matches(sub, &field, frame)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        }
    }

    fn key_eq(&self, a: &OV, b: &OV) -> bool {
        match (a, b) {
            (OV::Int(x), OV::Int(y)) => x == y,
            (OV::Bool(x), OV::Bool(y)) => x == y,
            (OV::Str(x), OV::Str(y)) => x == y,
            (OV::Obj(x), OV::Obj(y)) => {
                let x = x.borrow();
                let y = y.borrow();
                matches!((&x.kind, &y.kind), (OKind::Bytes(a), OKind::Bytes(b)) if a == b)
            }
            _ => false,
        }
    }

    fn ref_eq(&self, a: &OV, b: &OV) -> bool {
        match (a, b) {
            (OV::Obj(x), OV::Obj(y)) => Rc::ptr_eq(x, y),
            _ => false,
        }
    }

    /// Structural tuple equality: element pairs compare under the
    /// rules for their declared element types.
    fn tuple_eq(&self, a: &OV, b: &OV, ty: lm_types::TypeId) -> bool {
        let elems = match self.m.store.get(ty) {
            lm_types::Type::Tuple(elems) => elems.clone(),
            _ => return false,
        };
        let (OV::Obj(xa), OV::Obj(xb)) = (a, b) else {
            return false;
        };
        let xa = xa.borrow();
        let xb = xb.borrow();
        let (OKind::Tuple(ia), OKind::Tuple(ib)) = (&xa.kind, &xb.kind) else {
            return false;
        };
        elems.iter().enumerate().all(|(i, e)| {
            if *e == lm_types::UNIT {
                return true;
            }
            if matches!(self.m.store.get(*e), lm_types::Type::Tuple(_)) {
                return self.tuple_eq(&ia[i], &ib[i], *e);
            }
            match (&ia[i], &ib[i]) {
                (OV::Int(x), OV::Int(y)) => x == y,
                (OV::Bool(x), OV::Bool(y)) => x == y,
                (OV::Str(x), OV::Str(y)) if *e == lm_types::STRING => x == y,
                _ => self.ref_eq(&ia[i], &ib[i]),
            }
        })
    }

    fn binary(&self, op: BinOp, operand_ty: lm_types::TypeId, l: OV, r: OV) -> EResult {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                let a = self.as_int(&l)?;
                let b = self.as_int(&r)?;
                let out = match op {
                    BinOp::Add => a.checked_add(b),
                    BinOp::Sub => a.checked_sub(b),
                    BinOp::Mul => a.checked_mul(b),
                    BinOp::Div | BinOp::Rem => {
                        if b == 0 {
                            return Err(Stop::Fault("DivideByZero"));
                        }
                        if op == BinOp::Div {
                            a.checked_div(b)
                        } else {
                            a.checked_rem(b)
                        }
                    }
                    _ => unreachable!(),
                };
                out.map(OV::Int).ok_or(Stop::Fault("IntegerOverflow"))
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let a = self.as_int(&l)?;
                let b = self.as_int(&r)?;
                Ok(OV::Bool(match op {
                    BinOp::Lt => a < b,
                    BinOp::Le => a <= b,
                    BinOp::Gt => a > b,
                    _ => a >= b,
                }))
            }
            BinOp::Eq | BinOp::Ne => {
                let is_tuple = matches!(self.m.store.get(operand_ty), lm_types::Type::Tuple(_));
                let equal = if is_tuple {
                    self.tuple_eq(&l, &r, operand_ty)
                } else {
                    match (&l, &r) {
                        (OV::Int(a), OV::Int(b)) => a == b,
                        (OV::Bool(a), OV::Bool(b)) => a == b,
                        (OV::Str(a), OV::Str(b)) if operand_ty == lm_types::STRING => a == b,
                        _ => self.ref_eq(&l, &r),
                    }
                };
                Ok(OV::Bool(if op == BinOp::Eq { equal } else { !equal }))
            }
        }
    }

    fn intrinsic(
        &self,
        intrinsic: lm_abi::IntrinsicSlot,
        args: &[HExpr],
        frame: &mut Frame,
        depth: u32,
    ) -> EResult {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval(arg, frame, depth)?);
        }
        let frozen_guard = |obj: &Rc<RefCell<OObj>>| -> Result<(), Stop> {
            if obj.borrow().frozen {
                Err(Stop::Fault("FrozenWrite"))
            } else {
                Ok(())
            }
        };
        let binary = match intrinsic {
            lm_abi::INTRINSIC_INT_ADD => Some((BinOp::Add, lm_types::INT)),
            lm_abi::INTRINSIC_INT_SUB => Some((BinOp::Sub, lm_types::INT)),
            lm_abi::INTRINSIC_INT_MUL => Some((BinOp::Mul, lm_types::INT)),
            lm_abi::INTRINSIC_INT_DIV => Some((BinOp::Div, lm_types::INT)),
            lm_abi::INTRINSIC_INT_REM => Some((BinOp::Rem, lm_types::INT)),
            lm_abi::INTRINSIC_INT_EQ => Some((BinOp::Eq, lm_types::INT)),
            lm_abi::INTRINSIC_INT_NE => Some((BinOp::Ne, lm_types::INT)),
            lm_abi::INTRINSIC_INT_LT => Some((BinOp::Lt, lm_types::INT)),
            lm_abi::INTRINSIC_INT_LE => Some((BinOp::Le, lm_types::INT)),
            lm_abi::INTRINSIC_INT_GT => Some((BinOp::Gt, lm_types::INT)),
            lm_abi::INTRINSIC_INT_GE => Some((BinOp::Ge, lm_types::INT)),
            lm_abi::INTRINSIC_BOOL_EQ => Some((BinOp::Eq, lm_types::BOOL)),
            lm_abi::INTRINSIC_BOOL_NE => Some((BinOp::Ne, lm_types::BOOL)),
            lm_abi::INTRINSIC_STRING_EQ => Some((BinOp::Eq, lm_types::STRING)),
            lm_abi::INTRINSIC_STRING_NE => Some((BinOp::Ne, lm_types::STRING)),
            _ => None,
        };
        if let Some((op, ty)) = binary {
            return self.binary(op, ty, values[0].clone(), values[1].clone());
        }
        match intrinsic {
            lm_abi::INTRINSIC_INT_ABS => self
                .as_int(&values[0])?
                .checked_abs()
                .map(OV::Int)
                .ok_or(Stop::Fault("IntegerOverflow")),
            lm_abi::INTRINSIC_INT_NEG => self
                .as_int(&values[0])?
                .checked_neg()
                .map(OV::Int)
                .ok_or(Stop::Fault("IntegerOverflow")),
            lm_abi::INTRINSIC_BOOL_NOT => match values[0] {
                OV::Bool(value) => Ok(OV::Bool(!value)),
                _ => Err(Stop::Limit("non-Bool operand")),
            },
            lm_abi::INTRINSIC_STRING_BYTE_LEN => match &values[0] {
                OV::Str(text) => Ok(OV::Int(text.len() as i64)),
                _ => Err(Stop::Limit("non-String operand")),
            },
            lm_abi::INTRINSIC_STRING_CHAR_COUNT => match &values[0] {
                OV::Str(text) => Ok(OV::Int(text.chars().count() as i64)),
                _ => Err(Stop::Limit("non-String operand")),
            },
            lm_abi::INTRINSIC_STRING_CONCAT => match (&values[0], &values[1]) {
                (OV::Str(left), OV::Str(right)) => {
                    let mut text = String::with_capacity(left.len() + right.len());
                    text.push_str(left);
                    text.push_str(right);
                    Ok(OV::Str(Rc::new(text)))
                }
                _ => Err(Stop::Limit("non-String operand")),
            },
            lm_abi::INTRINSIC_STRING_STARTS_WITH => match (&values[0], &values[1]) {
                (OV::Str(text), OV::Str(prefix)) => Ok(OV::Bool(text.starts_with(prefix.as_str()))),
                _ => Err(Stop::Limit("non-String operand")),
            },
            lm_abi::INTRINSIC_STRING_ENDS_WITH => match (&values[0], &values[1]) {
                (OV::Str(text), OV::Str(suffix)) => Ok(OV::Bool(text.ends_with(suffix.as_str()))),
                _ => Err(Stop::Limit("non-String operand")),
            },
            lm_abi::INTRINSIC_STRING_CONTAINS => match (&values[0], &values[1]) {
                (OV::Str(text), OV::Str(needle)) => Ok(OV::Bool(text.contains(needle.as_str()))),
                _ => Err(Stop::Limit("non-String operand")),
            },
            lm_abi::INTRINSIC_STRING_FIND_INDEX => match (&values[0], &values[1]) {
                (OV::Str(text), OV::Str(needle)) => Ok(OV::Int(
                    text.find(needle.as_str())
                        .map(|index| index as i64)
                        .unwrap_or(-1),
                )),
                _ => Err(Stop::Limit("non-String operand")),
            },
            lm_abi::INTRINSIC_BYTES_LEN => {
                let obj = self.as_obj(&values[0])?;
                let len = match &obj.borrow().kind {
                    OKind::Bytes(bytes) => bytes.len(),
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                Ok(OV::Int(len as i64))
            }
            lm_abi::INTRINSIC_BYTES_AT | lm_abi::INTRINSIC_BYTES_GET => {
                let obj = self.as_obj(&values[0])?;
                let index = self.as_int(&values[1])?;
                let byte = match &obj.borrow().kind {
                    OKind::Bytes(bytes) if index >= 0 => bytes.get(index as usize).copied(),
                    OKind::Bytes(_) => None,
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                match (intrinsic, byte) {
                    (lm_abi::INTRINSIC_BYTES_AT, None) => Err(Stop::Fault("IndexOutOfBounds")),
                    (_, byte) => Ok(OV::Int(byte.map(i64::from).unwrap_or(-1))),
                }
            }
            lm_abi::INTRINSIC_BYTES_SLICE => {
                let obj = self.as_obj(&values[0])?;
                let start = self.as_int(&values[1])?;
                let length = self.as_int(&values[2])?;
                if start < 0 || length < 0 {
                    return Err(Stop::Fault("IndexOutOfBounds"));
                }
                let start = start as usize;
                let end = start
                    .checked_add(length as usize)
                    .ok_or(Stop::Fault("IndexOutOfBounds"))?;
                let bytes = match &obj.borrow().kind {
                    OKind::Bytes(bytes) => bytes
                        .get(start..end)
                        .ok_or(Stop::Fault("IndexOutOfBounds"))?
                        .to_vec(),
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                Ok(self.alloc(OKind::Bytes(bytes)))
            }
            lm_abi::INTRINSIC_BYTES_CONCAT => {
                let left = self.as_obj(&values[0])?;
                let right = self.as_obj(&values[1])?;
                let mut bytes = match &left.borrow().kind {
                    OKind::Bytes(bytes) => bytes.clone(),
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                match &right.borrow().kind {
                    OKind::Bytes(other) => bytes.extend_from_slice(other),
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                }
                Ok(self.alloc(OKind::Bytes(bytes)))
            }
            lm_abi::INTRINSIC_BYTES_STARTS_WITH => {
                let bytes = self.as_obj(&values[0])?;
                let prefix = self.as_obj(&values[1])?;
                let found = match (&bytes.borrow().kind, &prefix.borrow().kind) {
                    (OKind::Bytes(bytes), OKind::Bytes(prefix)) => bytes.starts_with(prefix),
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                Ok(OV::Bool(found))
            }
            lm_abi::INTRINSIC_BYTES_FIND_INDEX => {
                let bytes = self.as_obj(&values[0])?;
                let needle = self.as_obj(&values[1])?;
                let found = match (&bytes.borrow().kind, &needle.borrow().kind) {
                    (OKind::Bytes(bytes), OKind::Bytes(needle)) => {
                        if needle.is_empty() {
                            Some(0)
                        } else {
                            bytes.windows(needle.len()).position(|part| part == needle)
                        }
                    }
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                Ok(OV::Int(found.map(|index| index as i64).unwrap_or(-1)))
            }
            lm_abi::INTRINSIC_BYTES_HEX => {
                let bytes = self.as_obj(&values[0])?;
                let bytes = match &bytes.borrow().kind {
                    OKind::Bytes(bytes) => bytes.clone(),
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let mut text = String::with_capacity(bytes.len() * 2);
                for byte in bytes {
                    text.push(char::from(HEX[(byte >> 4) as usize]));
                    text.push(char::from(HEX[(byte & 0x0f) as usize]));
                }
                Ok(OV::Str(Rc::new(text)))
            }
            lm_abi::INTRINSIC_BYTES_IS_UTF8 => {
                let bytes = self.as_obj(&values[0])?;
                let valid = match &bytes.borrow().kind {
                    OKind::Bytes(bytes) => std::str::from_utf8(bytes).is_ok(),
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                Ok(OV::Bool(valid))
            }
            lm_abi::INTRINSIC_BYTES_TEXT => {
                let bytes = self.as_obj(&values[0])?;
                let bytes = match &bytes.borrow().kind {
                    OKind::Bytes(bytes) => bytes.clone(),
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                String::from_utf8(bytes)
                    .map(|text| OV::Str(Rc::new(text)))
                    .map_err(|_| Stop::Fault("BadCast"))
            }
            lm_abi::INTRINSIC_BYTES_EQ | lm_abi::INTRINSIC_BYTES_NE => {
                let left = self.as_obj(&values[0])?;
                let right = self.as_obj(&values[1])?;
                let equal = match (&left.borrow().kind, &right.borrow().kind) {
                    (OKind::Bytes(left), OKind::Bytes(right)) => left == right,
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                Ok(OV::Bool(equal == (intrinsic == lm_abi::INTRINSIC_BYTES_EQ)))
            }
            lm_abi::INTRINSIC_STRING_BUILDER_APPEND => {
                let builder = self.as_obj(&values[0])?;
                frozen_guard(&builder)?;
                let text = match &values[1] {
                    OV::Str(text) => text.clone(),
                    _ => return Err(Stop::Limit("append of a non-string")),
                };
                match &mut builder.borrow_mut().kind {
                    OKind::Sb(buffer) => buffer.push_str(&text),
                    _ => return Err(Stop::Limit("builder op on a non-builder")),
                }
                Ok(values[0].clone())
            }
            lm_abi::INTRINSIC_STRING_BUILDER_LEN => {
                let builder = self.as_obj(&values[0])?;
                let len = match &builder.borrow().kind {
                    OKind::Sb(buffer) => buffer.len(),
                    _ => return Err(Stop::Limit("builder op on a non-builder")),
                };
                Ok(OV::Int(len as i64))
            }
            lm_abi::INTRINSIC_STRING_BUILDER_CLEAR => {
                let builder = self.as_obj(&values[0])?;
                frozen_guard(&builder)?;
                match &mut builder.borrow_mut().kind {
                    OKind::Sb(buffer) => buffer.clear(),
                    _ => return Err(Stop::Limit("builder op on a non-builder")),
                }
                Ok(values[0].clone())
            }
            lm_abi::INTRINSIC_STRING_BUILDER_BUILD => {
                let builder = self.as_obj(&values[0])?;
                let text = match &builder.borrow().kind {
                    OKind::Sb(buffer) => buffer.clone(),
                    _ => return Err(Stop::Limit("builder op on a non-builder")),
                };
                Ok(OV::Str(Rc::new(text)))
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_APPEND => {
                let buffer = self.as_obj(&values[0])?;
                frozen_guard(&buffer)?;
                let byte = u8::try_from(self.as_int(&values[1])?)
                    .map_err(|_| Stop::Fault("IntegerOverflow"))?;
                match &mut buffer.borrow_mut().kind {
                    OKind::Bb(bytes) => bytes.push(byte),
                    _ => return Err(Stop::Limit("buffer op on a non-buffer")),
                }
                Ok(values[0].clone())
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_EXTEND => {
                let buffer = self.as_obj(&values[0])?;
                frozen_guard(&buffer)?;
                let source = self.as_obj(&values[1])?;
                let source = match &source.borrow().kind {
                    OKind::Bytes(bytes) => bytes.clone(),
                    _ => return Err(Stop::Limit("extend from a non-bytes value")),
                };
                match &mut buffer.borrow_mut().kind {
                    OKind::Bb(bytes) => bytes.extend_from_slice(&source),
                    _ => return Err(Stop::Limit("buffer op on a non-buffer")),
                }
                Ok(values[0].clone())
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_RESERVE => {
                let buffer = self.as_obj(&values[0])?;
                frozen_guard(&buffer)?;
                let additional = usize::try_from(self.as_int(&values[1])?)
                    .map_err(|_| Stop::Fault("IntegerOverflow"))?;
                match &mut buffer.borrow_mut().kind {
                    OKind::Bb(bytes) => bytes
                        .try_reserve(additional)
                        .map_err(|_| Stop::Fault("HeapLimit"))?,
                    _ => return Err(Stop::Limit("buffer op on a non-buffer")),
                }
                Ok(values[0].clone())
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_CLEAR => {
                let buffer = self.as_obj(&values[0])?;
                frozen_guard(&buffer)?;
                match &mut buffer.borrow_mut().kind {
                    OKind::Bb(bytes) => bytes.clear(),
                    _ => return Err(Stop::Limit("buffer op on a non-buffer")),
                }
                Ok(values[0].clone())
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_LEN => {
                let buffer = self.as_obj(&values[0])?;
                let len = match &buffer.borrow().kind {
                    OKind::Bb(bytes) => bytes.len(),
                    _ => return Err(Stop::Limit("buffer op on a non-buffer")),
                };
                Ok(OV::Int(len as i64))
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_BUILD => {
                let buffer = self.as_obj(&values[0])?;
                let bytes = match &buffer.borrow().kind {
                    OKind::Bb(bytes) => bytes.clone(),
                    _ => return Err(Stop::Limit("buffer op on a non-buffer")),
                };
                Ok(self.alloc(OKind::Bytes(bytes)))
            }
            _ => Err(Stop::Limit("unknown intrinsic")),
        }
    }

    fn native(&self, op: NativeOp, args: &[HExpr], frame: &mut Frame, depth: u32) -> EResult {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval(arg, frame, depth)?);
        }
        let frozen_guard = |obj: &Rc<RefCell<OObj>>| -> Result<(), Stop> {
            if obj.borrow().frozen {
                Err(Stop::Fault("FrozenWrite"))
            } else {
                Ok(())
            }
        };
        match op {
            NativeOp::Freeze => {
                let root = self.as_obj(&values[0])?;
                self.deep_freeze(root);
                Ok(values[0].clone())
            }
            // The oracle has no heap and no code identity, so it
            // cannot reproduce the canonical digest. A digest program
            // leaves the differential corpus instead of taking a
            // second, weaker encoder.
            NativeOp::Digest => Err(Stop::Limit("digest")),
            NativeOp::ListLen => {
                let obj = self.as_obj(&values[0])?;
                let len = match &obj.borrow().kind {
                    OKind::List(items) => items.len(),
                    _ => return Err(Stop::Limit("list op on a non-list")),
                };
                Ok(OV::Int(len as i64))
            }
            NativeOp::ListAt => {
                let obj = self.as_obj(&values[0])?;
                let idx = self.as_int(&values[1])?;
                let out = match &obj.borrow().kind {
                    OKind::List(items) => {
                        if idx < 0 || idx as usize >= items.len() {
                            return Err(Stop::Fault("IndexOutOfBounds"));
                        }
                        items[idx as usize].clone()
                    }
                    _ => return Err(Stop::Limit("list op on a non-list")),
                };
                Ok(out)
            }
            NativeOp::ListPush => {
                let obj = self.as_obj(&values[0])?;
                frozen_guard(&obj)?;
                match &mut obj.borrow_mut().kind {
                    OKind::List(items) => items.push(values[1].clone()),
                    _ => return Err(Stop::Limit("list op on a non-list")),
                }
                Ok(OV::Unit)
            }
            NativeOp::ListGet => {
                let obj = self.as_obj(&values[0])?;
                let idx = self.as_int(&values[1])?;
                let found = match &obj.borrow().kind {
                    OKind::List(items) => {
                        if idx >= 0 && (idx as usize) < items.len() {
                            Some(items[idx as usize].clone())
                        } else {
                            None
                        }
                    }
                    _ => return Err(Stop::Limit("list op on a non-list")),
                };
                Ok(self.option_value(found))
            }
            NativeOp::MapLen => {
                let obj = self.as_obj(&values[0])?;
                let len = match &obj.borrow().kind {
                    OKind::Map(entries) => entries.len(),
                    _ => return Err(Stop::Limit("map op on a non-map")),
                };
                Ok(OV::Int(len as i64))
            }
            NativeOp::MapHas => {
                let obj = self.as_obj(&values[0])?;
                let found = match &obj.borrow().kind {
                    OKind::Map(entries) => entries.iter().any(|(k, _)| self.key_eq(k, &values[1])),
                    _ => return Err(Stop::Limit("map op on a non-map")),
                };
                Ok(OV::Bool(found))
            }
            NativeOp::MapAt => {
                let obj = self.as_obj(&values[0])?;
                let found = match &obj.borrow().kind {
                    OKind::Map(entries) => entries
                        .iter()
                        .find(|(k, _)| self.key_eq(k, &values[1]))
                        .map(|(_, v)| v.clone()),
                    _ => return Err(Stop::Limit("map op on a non-map")),
                };
                found.ok_or(Stop::Fault("MissingKey"))
            }
            NativeOp::MapGet => {
                let obj = self.as_obj(&values[0])?;
                let found = match &obj.borrow().kind {
                    OKind::Map(entries) => entries
                        .iter()
                        .find(|(k, _)| self.key_eq(k, &values[1]))
                        .map(|(_, v)| v.clone()),
                    _ => return Err(Stop::Limit("map op on a non-map")),
                };
                Ok(self.option_value(found))
            }
            NativeOp::MapPut => {
                let obj = self.as_obj(&values[0])?;
                frozen_guard(&obj)?;
                match &mut obj.borrow_mut().kind {
                    OKind::Map(entries) => {
                        match entries.iter_mut().find(|(k, _)| self.key_eq(k, &values[1])) {
                            Some(entry) => entry.1 = values[2].clone(),
                            None => entries.push((values[1].clone(), values[2].clone())),
                        }
                    }
                    _ => return Err(Stop::Limit("map op on a non-map")),
                }
                Ok(OV::Unit)
            }
            NativeOp::BytesNew => match &values[0] {
                OV::Str(text) => Ok(self.alloc(OKind::Bytes(text.as_bytes().to_vec()))),
                _ => Err(Stop::Limit("bytes construction from a non-string")),
            },
        }
    }

    fn option_value(&self, found: Option<OV>) -> OV {
        match found {
            Some(v) => self.alloc(OKind::Instance {
                class: self.m.core.some_class,
                fields: vec![Some(v)],
            }),
            None => self.alloc(OKind::Instance {
                class: self.m.core.none_class,
                fields: vec![],
            }),
        }
    }

    /// Deeply freeze a graph with an iterative walk that preserves
    /// cycles. Born-frozen containers still pass the walk through to
    /// their children, matching the VM heap.
    fn deep_freeze(&self, root: Rc<RefCell<OObj>>) {
        let mut visited: Vec<*const RefCell<OObj>> = Vec::new();
        let mut work = vec![root];
        while let Some(obj) = work.pop() {
            let ptr = Rc::as_ptr(&obj);
            if visited.contains(&ptr) {
                continue;
            }
            visited.push(ptr);
            obj.borrow_mut().frozen = true;
            let mut push = |v: &OV| {
                if let OV::Obj(o) = v {
                    work.push(o.clone());
                }
            };
            match &obj.borrow().kind {
                OKind::Instance { fields, .. } => fields.iter().flatten().for_each(&mut push),
                OKind::List(items) | OKind::Tuple(items) => items.iter().for_each(&mut push),
                OKind::Map(entries) => {
                    for (k, v) in entries {
                        push(k);
                        push(v);
                    }
                }
                OKind::Closure { captures, .. } => captures.iter().for_each(&mut push),
                OKind::Sb(_) | OKind::Bb(_) | OKind::Bytes(_) => {}
            }
        }
    }

    /// Render one value with the same rules as the VM display.
    fn show(&self, value: &OV) -> String {
        let mut visited: Vec<*const RefCell<OObj>> = Vec::new();
        self.show_inner(value, 0, &mut visited)
    }

    fn show_inner(
        &self,
        value: &OV,
        depth: u32,
        visited: &mut Vec<*const RefCell<OObj>>,
    ) -> String {
        const MAX_SHOW_DEPTH: u32 = 32;
        match value {
            OV::Unit => "()".to_string(),
            OV::Bool(v) => v.to_string(),
            OV::Int(v) => v.to_string(),
            OV::Str(s) => render_string(s),
            OV::Obj(o) => {
                if depth >= MAX_SHOW_DEPTH {
                    return "...".to_string();
                }
                let ptr = Rc::as_ptr(o);
                if visited.contains(&ptr) {
                    return "<cycle>".to_string();
                }
                let kind = &o.borrow().kind;
                match kind {
                    OKind::List(items) => {
                        visited.push(ptr);
                        let parts: Vec<String> = items
                            .iter()
                            .map(|v| self.show_inner(v, depth + 1, visited))
                            .collect();
                        visited.pop();
                        format!("[{}]", parts.join(", "))
                    }
                    OKind::Tuple(items) => {
                        visited.push(ptr);
                        let parts: Vec<String> = items
                            .iter()
                            .map(|v| self.show_inner(v, depth + 1, visited))
                            .collect();
                        visited.pop();
                        if parts.len() == 1 {
                            format!("({},)", parts[0])
                        } else {
                            format!("({})", parts.join(", "))
                        }
                    }
                    OKind::Map(entries) => {
                        visited.push(ptr);
                        let parts: Vec<String> = entries
                            .iter()
                            .map(|(k, v)| {
                                format!(
                                    "{}: {}",
                                    self.show_inner(k, depth + 1, visited),
                                    self.show_inner(v, depth + 1, visited)
                                )
                            })
                            .collect();
                        visited.pop();
                        format!("{{{}}}", parts.join(", "))
                    }
                    OKind::Instance { class, fields } => {
                        visited.push(ptr);
                        let c = &self.m.classes[*class as usize];
                        let text = if c.kind == ClassKind::EnumCase {
                            let short = c.name.rsplit('.').next().unwrap_or(&c.name);
                            if fields.is_empty() {
                                short.to_string()
                            } else {
                                let parts: Vec<String> = fields
                                    .iter()
                                    .map(|v| match v {
                                        Some(v) => self.show_inner(v, depth + 1, visited),
                                        None => "<uninit>".to_string(),
                                    })
                                    .collect();
                                format!("{}({})", short, parts.join(", "))
                            }
                        } else {
                            let parts: Vec<String> = c
                                .field_names
                                .iter()
                                .zip(fields.iter())
                                .map(|(name, v)| {
                                    let v = match v {
                                        Some(v) => self.show_inner(v, depth + 1, visited),
                                        None => "<uninit>".to_string(),
                                    };
                                    format!("{name}: {v}")
                                })
                                .collect();
                            format!("{}{{{}}}", c.name, parts.join(", "))
                        };
                        visited.pop();
                        text
                    }
                    OKind::Closure { func, .. } => {
                        format!("<closure {}>", self.m.funcs[*func as usize].name)
                    }
                    OKind::Sb(buf) => format!("<StringBuilder len {}>", buf.len()),
                    OKind::Bb(bytes) => format!("<ByteBuffer len {}>", bytes.len()),
                    OKind::Bytes(bytes) => format!("<Bytes len {}>", bytes.len()),
                }
            }
        }
    }
}

/// Render a string value with quotation marks and escapes, matching
/// the VM display.
fn render_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
