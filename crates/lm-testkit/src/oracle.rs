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
    Float(u64),
    Char(char),
    Str(Rc<String>),
    Substring(Rc<String>),
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
    Sb(Option<String>),
    Bb(Option<Vec<u8>>),
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
            HStmt::For {
                source,
                bindings,
                kind,
                body,
            } => self.run_for(source, bindings, kind, body, frame, depth),
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

    fn run_for(
        &self,
        source: &HExpr,
        bindings: &[u32],
        kind: &HForKind,
        body: &[HStmt],
        frame: &mut Frame,
        depth: u32,
    ) -> Result<(), Stop> {
        let source_value = self.eval(source, frame, depth)?;
        let source_slot = match kind {
            HForKind::List { source_slot, .. }
            | HForKind::Map { source_slot, .. }
            | HForKind::Text { source_slot, .. }
            | HForKind::Range { source_slot, .. }
            | HForKind::Generic { source_slot, .. } => *source_slot,
        };
        frame.set(source_slot, source_value.clone());

        match kind {
            HForKind::List {
                index_slot,
                epoch_slot,
                ..
            } => {
                frame.set(*epoch_slot, OV::Int(0));
                let object = self.as_obj(&source_value)?;
                let length = match &object.borrow().kind {
                    OKind::List(items) => items.len(),
                    _ => return Err(Stop::Limit("list loop on a non-list")),
                };
                for index in 0..length {
                    let value = match &object.borrow().kind {
                        OKind::List(items) if items.len() == length => items[index].clone(),
                        OKind::List(_) => return Err(Stop::Fault("CollectionModified")),
                        _ => return Err(Stop::Limit("list loop on a non-list")),
                    };
                    frame.set(*index_slot, OV::Int(index as i64));
                    frame.set(bindings[0], value);
                    match self.run_block(body, frame, depth, false) {
                        Ok(_) | Err(Stop::Continue) => {}
                        Err(Stop::Break) => break,
                        Err(other) => return Err(other),
                    }
                }
            }
            HForKind::Map {
                index_slot,
                epoch_slot,
                ..
            } => {
                frame.set(*epoch_slot, OV::Int(0));
                let object = self.as_obj(&source_value)?;
                let length = match &object.borrow().kind {
                    OKind::Map(entries) => entries.len(),
                    _ => return Err(Stop::Limit("map loop on a non-map")),
                };
                for index in 0..length {
                    let (key, value) = match &object.borrow().kind {
                        OKind::Map(entries) if entries.len() == length => entries[index].clone(),
                        OKind::Map(_) => return Err(Stop::Fault("CollectionModified")),
                        _ => return Err(Stop::Limit("map loop on a non-map")),
                    };
                    frame.set(*index_slot, OV::Int(index as i64));
                    if bindings.len() == 1 {
                        frame.set(bindings[0], self.alloc(OKind::Tuple(vec![key, value])));
                    } else {
                        frame.set(bindings[0], key);
                        frame.set(bindings[1], value);
                    }
                    match self.run_block(body, frame, depth, false) {
                        Ok(_) | Err(Stop::Continue) => {}
                        Err(Stop::Break) => break,
                        Err(other) => return Err(other),
                    }
                }
            }
            HForKind::Text { cursor_slot, .. } => {
                let text = self.as_text(&source_value)?.to_string();
                let mut cursor = 0;
                for value in text.chars() {
                    frame.set(*cursor_slot, OV::Int(cursor));
                    frame.set(bindings[0], OV::Char(value));
                    cursor += value.len_utf8() as i64;
                    match self.run_block(body, frame, depth, false) {
                        Ok(_) | Err(Stop::Continue) => {}
                        Err(Stop::Break) => break,
                        Err(other) => return Err(other),
                    }
                }
            }
            HForKind::Range {
                cursor_slot,
                stop_slot,
                ..
            } => {
                let object = self.as_obj(&source_value)?;
                let (start, stop) = match &object.borrow().kind {
                    OKind::Instance { fields, .. } => {
                        let start = fields[0]
                            .as_ref()
                            .ok_or(Stop::Fault("UninitializedField"))?;
                        let stop = fields[1]
                            .as_ref()
                            .ok_or(Stop::Fault("UninitializedField"))?;
                        (self.as_int(start)?, self.as_int(stop)?)
                    }
                    _ => return Err(Stop::Limit("range loop on a non-range")),
                };
                frame.set(*stop_slot, OV::Int(stop));
                for value in start..stop {
                    frame.set(*cursor_slot, OV::Int(value));
                    frame.set(bindings[0], OV::Int(value));
                    match self.run_block(body, frame, depth, false) {
                        Ok(_) | Err(Stop::Continue) => {}
                        Err(Stop::Break) => break,
                        Err(other) => return Err(other),
                    }
                }
            }
            HForKind::Generic {
                iterator_slot,
                option_slot,
                item_slot,
                iterator,
                next,
                some_ty,
                ..
            } => {
                let iterator_value = self.eval(iterator, frame, depth)?;
                frame.set(*iterator_slot, iterator_value);
                loop {
                    let option = self.eval(next, frame, depth)?;
                    frame.set(*option_slot, option.clone());
                    if !self.instance_matches(&option, *some_ty)? {
                        break;
                    }
                    let object = self.as_obj(&option)?;
                    let item = match &object.borrow().kind {
                        OKind::Instance { fields, .. } => {
                            fields[0].clone().ok_or(Stop::Fault("UninitializedField"))?
                        }
                        _ => return Err(Stop::Limit("iterator returned an invalid option")),
                    };
                    if let Some(item_slot) = item_slot {
                        frame.set(*item_slot, item.clone());
                        let tuple = self.as_obj(&item)?;
                        let values = match &tuple.borrow().kind {
                            OKind::Tuple(values) if values.len() == 2 => values.clone(),
                            _ => return Err(Stop::Limit("iterator item is not a pair")),
                        };
                        frame.set(bindings[0], values[0].clone());
                        frame.set(bindings[1], values[1].clone());
                    } else {
                        frame.set(bindings[0], item);
                    }
                    match self.run_block(body, frame, depth, false) {
                        Ok(_) | Err(Stop::Continue) => {}
                        Err(Stop::Break) => break,
                        Err(other) => return Err(other),
                    }
                }
            }
        }
        Ok(())
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

    fn as_float(&self, value: &OV) -> Result<f64, Stop> {
        match value {
            OV::Float(bits) => Ok(f64::from_bits(*bits)),
            _ => Err(Stop::Limit("expected a Float value")),
        }
    }

    fn as_char(&self, value: &OV) -> Result<char, Stop> {
        match value {
            OV::Char(value) => Ok(*value),
            _ => Err(Stop::Limit("expected a Char value")),
        }
    }

    fn as_text<'a>(&self, value: &'a OV) -> Result<&'a str, Stop> {
        match value {
            OV::Str(text) | OV::Substring(text) => Ok(text.as_str()),
            _ => Err(Stop::Limit("expected a Text value")),
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
            Some(NativeRepr::Unit) => return Ok(OV::Unit),
            Some(NativeRepr::Int) => return Ok(OV::Int(0)),
            Some(NativeRepr::Float) => return Ok(OV::Float(0)),
            Some(NativeRepr::Bool) => return Ok(OV::Bool(false)),
            Some(NativeRepr::String) => return Ok(OV::Str(Rc::new(String::new()))),
            Some(NativeRepr::Bytes) => return Ok(self.alloc(OKind::Bytes(Vec::new()))),
            Some(NativeRepr::StringBuilder) => {
                return Ok(self.alloc(OKind::Sb(Some(String::new()))))
            }
            Some(NativeRepr::ByteBuffer) => return Ok(self.alloc(OKind::Bb(Some(Vec::new())))),
            Some(NativeRepr::List) => return Ok(self.alloc(OKind::List(Vec::new()))),
            Some(NativeRepr::Map) => return Ok(self.alloc(OKind::Map(Vec::new()))),
            Some(NativeRepr::Tuple(arity)) => {
                if args.len() != arity as usize {
                    return Err(Stop::Limit("the tuple constructor has the wrong arity"));
                }
                return Ok(self.alloc(OKind::Tuple(args)));
            }
            Some(
                NativeRepr::Text
                | NativeRepr::Substring
                | NativeRepr::Char
                | NativeRepr::FileHandle
                | NativeRepr::TcpResource
                | NativeRepr::TcpStream
                | NativeRepr::TcpListener
                | NativeRepr::TlsStream
                | NativeRepr::UdpSocket
                | NativeRepr::Artifact
                | NativeRepr::VerifiedModule
                | NativeRepr::FunctionCode
                | NativeRepr::ClassCode
                | NativeRepr::SlotSpec
                | NativeRepr::CodeInstance
                | NativeRepr::Slot
                | NativeRepr::FunctionDef
                | NativeRepr::ClassDef
                | NativeRepr::FunctionBinding
                | NativeRepr::ClassBinding
                | NativeRepr::DynValue,
            ) => return Err(Stop::Limit("this native class has no direct constructor")),
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
            HExprKind::Float(bits) => Ok(OV::Float(*bits)),
            HExprKind::Bool(v) => Ok(OV::Bool(*v)),
            HExprKind::Str(v) => Ok(OV::Str(Rc::new(v.clone()))),
            HExprKind::Bytes(v) => Ok(self.alloc(OKind::Bytes(v.clone()))),
            HExprKind::Local(slot) => frame.get(*slot),
            HExprKind::Capture(idx) => Ok(frame.captures[*idx as usize].clone()),
            HExprKind::Not(inner) => match self.eval(inner, frame, depth)? {
                OV::Bool(v) => Ok(OV::Bool(!v)),
                _ => Err(Stop::Limit("non-Bool operand")),
            },
            HExprKind::Neg(inner) => match self.eval(inner, frame, depth)? {
                OV::Int(value) => value
                    .checked_neg()
                    .map(OV::Int)
                    .ok_or(Stop::Fault("IntegerOverflow")),
                OV::Float(bits) => Ok(OV::Float(lm_value::canonical_float_bits(
                    (-f64::from_bits(bits)).to_bits(),
                ))),
                _ => Err(Stop::Limit("invalid negation operand")),
            },
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
            }
            | HExprKind::InterfaceCall {
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
                let class = match self.native_class(&recv_v) {
                    Some(class) => class,
                    None => {
                        let obj = self.as_obj(&recv_v)?;
                        let class = match &obj.borrow().kind {
                            OKind::Instance { class, .. } => *class,
                            _ => return Err(Stop::Limit("method call on a non-instance")),
                        };
                        class
                    }
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
            HExprKind::MakeClosure { func, captures }
            | HExprKind::MakeCallback { func, captures } => {
                let mut values = Vec::with_capacity(captures.len());
                for capture in captures {
                    values.push(self.eval(capture, frame, depth)?);
                }
                Ok(self.alloc(OKind::Closure {
                    func: *func,
                    captures: values,
                }))
            }
            HExprKind::AsCallback(value) => self.eval(value, frame, depth),
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
                let builder = self.alloc(OKind::Sb(Some(String::new())));
                for part in parts {
                    match part {
                        HInterpPart::Lit(text) => {
                            let object = self.as_obj(&builder)?;
                            match &mut object.borrow_mut().kind {
                                OKind::Sb(Some(buffer)) => buffer.push_str(text),
                                _ => return Err(Stop::Limit("invalid interpolation builder")),
                            };
                        }
                        HInterpPart::Native { value, kind } => {
                            let value = self.eval(value, frame, depth)?;
                            let text = match (kind, value) {
                                (HInterpNative::Int, OV::Int(value)) => value.to_string(),
                                (HInterpNative::Float, OV::Float(bits)) => {
                                    f64::from_bits(bits).to_string()
                                }
                                (HInterpNative::Bool, OV::Bool(true)) => "true".to_string(),
                                (HInterpNative::Bool, OV::Bool(false)) => "false".to_string(),
                                (HInterpNative::Char, OV::Char(value)) => value.to_string(),
                                (HInterpNative::Text, OV::Str(value))
                                | (HInterpNative::Text, OV::Substring(value)) => {
                                    value.as_ref().clone()
                                }
                                _ => return Err(Stop::Limit("invalid native interpolation")),
                            };
                            let object = self.as_obj(&builder)?;
                            match &mut object.borrow_mut().kind {
                                OKind::Sb(Some(buffer)) => buffer.push_str(&text),
                                _ => return Err(Stop::Limit("invalid interpolation builder")),
                            };
                        }
                        HInterpPart::Display {
                            value, selector, ..
                        } => {
                            let receiver = self.eval(value, frame, depth)?;
                            let class = match self.native_class(&receiver) {
                                Some(class) => class,
                                None => {
                                    let object = self.as_obj(&receiver)?;
                                    let class = match &object.borrow().kind {
                                        OKind::Instance { class, .. } => *class,
                                        _ => {
                                            return Err(Stop::Limit(
                                                "display call on a non-instance",
                                            ))
                                        }
                                    };
                                    class
                                }
                            };
                            let func = self
                                .find_method(class, selector)
                                .ok_or(Stop::Limit("unknown display selector"))?;
                            self.call(func, vec![receiver, builder.clone()], vec![], depth + 1)?;
                        }
                    }
                }
                let object = self.as_obj(&builder)?;
                let out = match &object.borrow().kind {
                    OKind::Sb(Some(buffer)) => buffer.clone(),
                    _ => return Err(Stop::Limit("invalid interpolation builder")),
                };
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
            | HExprKind::PrepareWait { .. }
            | HExprKind::Spawn { .. }
            | HExprKind::FunctionCode { .. }
            | HExprKind::ClassCode { .. }
            | HExprKind::CodeSource { .. }
            | HExprKind::CodeDefinition { .. }
            | HExprKind::OpConst(_)
            | HExprKind::TableEdit { .. }
            | HExprKind::CallArgs { .. }
            | HExprKind::FaultCodeGet { .. }
            | HExprKind::FaultSiteGet { .. }
            | HExprKind::FaultTraceGet { .. }
            | HExprKind::FaultDenied { .. }
            | HExprKind::RequestOpName { .. } => Err(Stop::Limit(
                "the oracle models the pure subset only; programs with performs \
                 are outside the oracle",
            )),
        }
    }

    /// The class of one value with a native representation.
    ///
    /// A native text or scalar value carries no instance object, so a
    /// virtual call on a `Text` receiver needs the class from the
    /// value form. The VM reads the same relation from its heap tag.
    fn native_class(&self, value: &OV) -> Option<u32> {
        let want = match value {
            OV::Unit => NativeRepr::Unit,
            OV::Bool(_) => NativeRepr::Bool,
            OV::Int(_) => NativeRepr::Int,
            OV::Float(_) => NativeRepr::Float,
            OV::Str(_) => NativeRepr::String,
            OV::Substring(_) => NativeRepr::Substring,
            OV::Char(_) => NativeRepr::Char,
            OV::Obj(object) => match &object.borrow().kind {
                OKind::List(_) => NativeRepr::List,
                OKind::Map(_) => NativeRepr::Map,
                OKind::Tuple(values) => NativeRepr::Tuple(values.len().try_into().ok()?),
                OKind::Sb(_) => NativeRepr::StringBuilder,
                OKind::Bb(_) => NativeRepr::ByteBuffer,
                OKind::Bytes(_) => NativeRepr::Bytes,
                OKind::Instance { .. } | OKind::Closure { .. } => return None,
            },
        };
        self.m
            .classes
            .iter()
            .position(|class| class.native_repr == Some(want))
            .map(|index| index as u32)
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
            (OV::Float(x), OV::Float(y)) => oracle_float_eq(*x, *y),
            (OV::Bool(x), OV::Bool(y)) => x == y,
            (OV::Str(x), OV::Str(y))
            | (OV::Str(x), OV::Substring(y))
            | (OV::Substring(x), OV::Str(y))
            | (OV::Substring(x), OV::Substring(y)) => x == y,
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

    /// Test whether one type names an enum family or one of its arms.
    fn is_enum_family(&self, ty: lm_types::TypeId) -> bool {
        let Some((class, _)) = self.m.store.nominal_class(ty) else {
            return false;
        };
        matches!(
            self.m.store.class_meta(class).kind,
            lm_types::ClassKind::EnumParent | lm_types::ClassKind::EnumCase
        )
    }

    /// Structural equality of two enum values: the same case and
    /// equal fields. Each field takes the rule of its own form, and
    /// the walk keeps its own stack.
    fn value_eq(&self, a: &OV, b: &OV) -> bool {
        let mut work: Vec<(OV, OV)> = vec![(a.clone(), b.clone())];
        while let Some((left, right)) = work.pop() {
            let equal = match (&left, &right) {
                (OV::Unit, OV::Unit) => true,
                (OV::Int(x), OV::Int(y)) => x == y,
                (OV::Float(x), OV::Float(y)) => oracle_float_eq(*x, *y),
                (OV::Bool(x), OV::Bool(y)) => x == y,
                (OV::Char(x), OV::Char(y)) => x == y,
                (OV::Str(x), OV::Str(y))
                | (OV::Str(x), OV::Substring(y))
                | (OV::Substring(x), OV::Str(y))
                | (OV::Substring(x), OV::Substring(y)) => x == y,
                (OV::Obj(x), OV::Obj(y)) => {
                    if Rc::ptr_eq(x, y) {
                        continue;
                    }
                    let xb = x.borrow();
                    let yb = y.borrow();
                    match (&xb.kind, &yb.kind) {
                        (OKind::Bytes(p), OKind::Bytes(q)) => p == q,
                        (
                            OKind::Instance {
                                class: ac,
                                fields: af,
                            },
                            OKind::Instance {
                                class: bc,
                                fields: bf,
                            },
                        ) => {
                            let is_case = self
                                .m
                                .classes
                                .get(*ac as usize)
                                .map(|c| c.kind == lm_types::ClassKind::EnumCase)
                                .unwrap_or(false);
                            if !is_case || ac != bc || af.len() != bf.len() {
                                false
                            } else {
                                for (p, q) in af.iter().zip(bf.iter()) {
                                    match (p, q) {
                                        (Some(p), Some(q)) => work.push((p.clone(), q.clone())),
                                        _ => return false,
                                    }
                                }
                                continue;
                            }
                        }
                        (OKind::Tuple(ai), OKind::Tuple(bi)) => {
                            if ai.len() != bi.len() {
                                false
                            } else {
                                for (p, q) in ai.iter().zip(bi.iter()) {
                                    work.push((p.clone(), q.clone()));
                                }
                                continue;
                            }
                        }
                        _ => false,
                    }
                }
                _ => false,
            };
            if !equal {
                return false;
            }
        }
        true
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
                (OV::Float(x), OV::Float(y)) => oracle_float_eq(*x, *y),
                (OV::Bool(x), OV::Bool(y)) => x == y,
                (OV::Str(x), OV::Str(y)) if *e == lm_types::STRING => x == y,
                _ => self.ref_eq(&ia[i], &ib[i]),
            }
        })
    }

    fn binary(&self, op: BinOp, operand_ty: lm_types::TypeId, l: OV, r: OV) -> EResult {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                if operand_ty == lm_types::FLOAT {
                    let a = self.as_float(&l)?;
                    let b = self.as_float(&r)?;
                    let out = match op {
                        BinOp::Add => a + b,
                        BinOp::Sub => a - b,
                        BinOp::Mul => a * b,
                        BinOp::Div => a / b,
                        BinOp::Rem => return Err(Stop::Limit("Float has no remainder")),
                        _ => unreachable!(),
                    };
                    return Ok(OV::Float(lm_value::canonical_float_bits(out.to_bits())));
                }
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
                if operand_ty == lm_types::FLOAT {
                    let a = self.as_float(&l)?;
                    let b = self.as_float(&r)?;
                    return Ok(OV::Bool(match op {
                        BinOp::Lt => a < b,
                        BinOp::Le => a <= b,
                        BinOp::Gt => a > b,
                        _ => a >= b,
                    }));
                }
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
                } else if self.is_enum_family(operand_ty) {
                    self.value_eq(&l, &r)
                } else {
                    match (&l, &r) {
                        (OV::Int(a), OV::Int(b)) => a == b,
                        (OV::Float(a), OV::Float(b)) => oracle_float_eq(*a, *b),
                        (OV::Bool(a), OV::Bool(b)) => a == b,
                        (OV::Str(a), OV::Str(b)) if operand_ty == lm_types::STRING => a == b,
                        _ => self.ref_eq(&l, &r),
                    }
                };
                Ok(OV::Bool(if op == BinOp::Eq { equal } else { !equal }))
            }
            BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::Shl
            | BinOp::Shr
            | BinOp::Ushr => {
                let left = self.as_int(&l)?;
                let right = self.as_int(&r)?;
                let value = match op {
                    BinOp::BitAnd => left & right,
                    BinOp::BitOr => left | right,
                    BinOp::BitXor => left ^ right,
                    BinOp::Shl => ((left as u64) << oracle_shift(right)?) as i64,
                    BinOp::Shr => left >> oracle_shift(right)?,
                    BinOp::Ushr => ((left as u64) >> oracle_shift(right)?) as i64,
                    _ => unreachable!(),
                };
                Ok(OV::Int(value))
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
            lm_abi::INTRINSIC_INT_BIT_AND => Some((BinOp::BitAnd, lm_types::INT)),
            lm_abi::INTRINSIC_INT_BIT_OR => Some((BinOp::BitOr, lm_types::INT)),
            lm_abi::INTRINSIC_INT_BIT_XOR => Some((BinOp::BitXor, lm_types::INT)),
            lm_abi::INTRINSIC_INT_SHL => Some((BinOp::Shl, lm_types::INT)),
            lm_abi::INTRINSIC_INT_SHR => Some((BinOp::Shr, lm_types::INT)),
            lm_abi::INTRINSIC_INT_USHR => Some((BinOp::Ushr, lm_types::INT)),
            lm_abi::INTRINSIC_INT_EQ => Some((BinOp::Eq, lm_types::INT)),
            lm_abi::INTRINSIC_INT_NE => Some((BinOp::Ne, lm_types::INT)),
            lm_abi::INTRINSIC_INT_LT => Some((BinOp::Lt, lm_types::INT)),
            lm_abi::INTRINSIC_INT_LE => Some((BinOp::Le, lm_types::INT)),
            lm_abi::INTRINSIC_INT_GT => Some((BinOp::Gt, lm_types::INT)),
            lm_abi::INTRINSIC_INT_GE => Some((BinOp::Ge, lm_types::INT)),
            lm_abi::INTRINSIC_FLOAT_ADD => Some((BinOp::Add, lm_types::FLOAT)),
            lm_abi::INTRINSIC_FLOAT_SUB => Some((BinOp::Sub, lm_types::FLOAT)),
            lm_abi::INTRINSIC_FLOAT_MUL => Some((BinOp::Mul, lm_types::FLOAT)),
            lm_abi::INTRINSIC_FLOAT_DIV => Some((BinOp::Div, lm_types::FLOAT)),
            lm_abi::INTRINSIC_FLOAT_EQ => Some((BinOp::Eq, lm_types::FLOAT)),
            lm_abi::INTRINSIC_FLOAT_NE => Some((BinOp::Ne, lm_types::FLOAT)),
            lm_abi::INTRINSIC_FLOAT_LT => Some((BinOp::Lt, lm_types::FLOAT)),
            lm_abi::INTRINSIC_FLOAT_LE => Some((BinOp::Le, lm_types::FLOAT)),
            lm_abi::INTRINSIC_FLOAT_GT => Some((BinOp::Gt, lm_types::FLOAT)),
            lm_abi::INTRINSIC_FLOAT_GE => Some((BinOp::Ge, lm_types::FLOAT)),
            lm_abi::INTRINSIC_BOOL_EQ => Some((BinOp::Eq, lm_types::BOOL)),
            lm_abi::INTRINSIC_BOOL_NE => Some((BinOp::Ne, lm_types::BOOL)),
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
            lm_abi::INTRINSIC_INT_BIT_NOT => Ok(OV::Int(!self.as_int(&values[0])?)),
            lm_abi::INTRINSIC_INT_WRAPPING_ADD
            | lm_abi::INTRINSIC_INT_WRAPPING_SUB
            | lm_abi::INTRINSIC_INT_WRAPPING_MUL => {
                let left = self.as_int(&values[0])?;
                let right = self.as_int(&values[1])?;
                Ok(OV::Int(match intrinsic {
                    lm_abi::INTRINSIC_INT_WRAPPING_ADD => left.wrapping_add(right),
                    lm_abi::INTRINSIC_INT_WRAPPING_SUB => left.wrapping_sub(right),
                    _ => left.wrapping_mul(right),
                }))
            }
            lm_abi::INTRINSIC_INT_ROTATE_LEFT | lm_abi::INTRINSIC_INT_ROTATE_RIGHT => {
                let value = self.as_int(&values[0])? as u64;
                let amount = oracle_shift(self.as_int(&values[1])?)?;
                let result = if intrinsic == lm_abi::INTRINSIC_INT_ROTATE_LEFT {
                    value.rotate_left(amount)
                } else {
                    value.rotate_right(amount)
                };
                Ok(OV::Int(result as i64))
            }
            lm_abi::INTRINSIC_INT_TO_FLOAT => {
                Ok(OV::Float((self.as_int(&values[0])? as f64).to_bits()))
            }
            lm_abi::INTRINSIC_FLOAT_NEG => Ok(OV::Float(lm_value::canonical_float_bits(
                (-self.as_float(&values[0])?).to_bits(),
            ))),
            lm_abi::INTRINSIC_FLOAT_IS_NAN => Ok(OV::Bool(self.as_float(&values[0])?.is_nan())),
            lm_abi::INTRINSIC_FLOAT_HASH => Ok(OV::Int(oracle_float_hash(match values[0] {
                OV::Float(bits) => bits,
                _ => return Err(Stop::Limit("expected a Float value")),
            }))),
            lm_abi::INTRINSIC_FLOAT_BITS => match values[0] {
                OV::Float(bits) => Ok(OV::Int(lm_value::canonical_float_bits(bits) as i64)),
                _ => Err(Stop::Limit("expected a Float value")),
            },
            lm_abi::INTRINSIC_FLOAT_FROM_BITS => Ok(OV::Float(lm_value::canonical_float_bits(
                self.as_int(&values[0])? as u64,
            ))),
            lm_abi::INTRINSIC_FLOAT_TO_INT_STATUS => {
                let value = self.as_float(&values[0])?;
                Ok(OV::Int(if !value.is_finite() {
                    1
                } else if !oracle_float_fits_int(value) {
                    2
                } else {
                    0
                }))
            }
            lm_abi::INTRINSIC_FLOAT_TO_INT_VALUE => {
                let value = self.as_float(&values[0])?;
                if !value.is_finite() {
                    return Err(Stop::Fault("BadCast"));
                }
                if !oracle_float_fits_int(value) {
                    return Err(Stop::Fault("IntegerOverflow"));
                }
                Ok(OV::Int(value.trunc() as i64))
            }
            lm_abi::INTRINSIC_FLOAT_FIXED => {
                let value = self.as_float(&values[0])?;
                let digits = self.as_int(&values[1])?;
                if digits < 0 {
                    return Err(Stop::Fault("InvalidPrecision"));
                }
                let digits = usize::try_from(digits).map_err(|_| Stop::Fault("HeapLimit"))?;
                Ok(OV::Str(Rc::new(format!("{value:.digits$}"))))
            }
            lm_abi::INTRINSIC_BOOL_NOT => match values[0] {
                OV::Bool(value) => Ok(OV::Bool(!value)),
                _ => Err(Stop::Limit("non-Bool operand")),
            },
            lm_abi::INTRINSIC_STRING_BYTE_LEN => {
                Ok(OV::Int(self.as_text(&values[0])?.len() as i64))
            }
            lm_abi::INTRINSIC_STRING_CHAR_COUNT => {
                Ok(OV::Int(self.as_text(&values[0])?.chars().count() as i64))
            }
            lm_abi::INTRINSIC_STRING_CONCAT => {
                let left = self.as_text(&values[0])?;
                let right = self.as_text(&values[1])?;
                let mut text = String::with_capacity(left.len() + right.len());
                text.push_str(left);
                text.push_str(right);
                Ok(OV::Str(Rc::new(text)))
            }
            lm_abi::INTRINSIC_STRING_STARTS_WITH => Ok(OV::Bool(
                self.as_text(&values[0])?
                    .starts_with(self.as_text(&values[1])?),
            )),
            lm_abi::INTRINSIC_STRING_ENDS_WITH => Ok(OV::Bool(
                self.as_text(&values[0])?
                    .ends_with(self.as_text(&values[1])?),
            )),
            lm_abi::INTRINSIC_STRING_CONTAINS => Ok(OV::Bool(
                self.as_text(&values[0])?
                    .contains(self.as_text(&values[1])?),
            )),
            lm_abi::INTRINSIC_STRING_FIND_INDEX => {
                let text = self.as_text(&values[0])?;
                let needle = self.as_text(&values[1])?;
                let found = text
                    .find(needle)
                    .map(|byte| text[..byte].chars().count() as i64)
                    .unwrap_or(-1);
                Ok(OV::Int(found))
            }
            lm_abi::INTRINSIC_TEXT_FIND_BYTE_INDEX => {
                let text = self.as_text(&values[0])?;
                let needle = self.as_text(&values[1])?;
                Ok(OV::Int(
                    text.find(needle).map(|byte| byte as i64).unwrap_or(-1),
                ))
            }
            lm_abi::INTRINSIC_TEXT_TRIM => {
                let text = self.as_text(&values[0])?;
                Ok(OV::Substring(Rc::new(text.trim().to_string())))
            }
            lm_abi::INTRINSIC_TEXT_TRIM_START => {
                let text = self.as_text(&values[0])?;
                Ok(OV::Substring(Rc::new(text.trim_start().to_string())))
            }
            lm_abi::INTRINSIC_TEXT_TRIM_END => {
                let text = self.as_text(&values[0])?;
                Ok(OV::Substring(Rc::new(text.trim_end().to_string())))
            }
            lm_abi::INTRINSIC_TEXT_TO_LOWER_ASCII => {
                let text = self.as_text(&values[0])?;
                Ok(OV::Str(Rc::new(text.to_ascii_lowercase())))
            }
            lm_abi::INTRINSIC_TEXT_TO_UPPER_ASCII => {
                let text = self.as_text(&values[0])?;
                Ok(OV::Str(Rc::new(text.to_ascii_uppercase())))
            }
            lm_abi::INTRINSIC_TEXT_REPLACE => {
                let text = self.as_text(&values[0])?;
                let needle = self.as_text(&values[1])?;
                let replacement = self.as_text(&values[2])?;
                Ok(OV::Str(Rc::new(text.replace(needle, replacement))))
            }
            lm_abi::INTRINSIC_TEXT_PARSE_INT_STATUS | lm_abi::INTRINSIC_TEXT_PARSE_INT_VALUE => {
                let text = self.as_text(&values[0])?.to_string();
                let value = intrinsic == lm_abi::INTRINSIC_TEXT_PARSE_INT_VALUE;
                let radix = u32::try_from(self.as_int(&values[1])?)
                    .ok()
                    .filter(|radix| (2..=36).contains(radix));
                let Some(radix) = radix else {
                    return Ok(OV::Int(if value { 0 } else { 3 }));
                };
                let parsed = i64::from_str_radix(&text, radix);
                if value {
                    return Ok(OV::Int(parsed.unwrap_or(0)));
                }
                Ok(OV::Int(match parsed {
                    Ok(_) => 0,
                    Err(error) => match error.kind() {
                        std::num::IntErrorKind::PosOverflow
                        | std::num::IntErrorKind::NegOverflow => 2,
                        _ => 1,
                    },
                }))
            }
            lm_abi::INTRINSIC_TEXT_PAD_START | lm_abi::INTRINSIC_TEXT_PAD_END => {
                let text = self.as_text(&values[0])?;
                let width = self.as_int(&values[1])?;
                let length = i64::try_from(text.chars().count())
                    .map_err(|_| Stop::Fault("IntegerOverflow"))?;
                let padding = usize::try_from(width.saturating_sub(length).max(0))
                    .map_err(|_| Stop::Fault("HeapLimit"))?;
                let spaces = " ".repeat(padding);
                let output = if intrinsic == lm_abi::INTRINSIC_TEXT_PAD_START {
                    format!("{spaces}{text}")
                } else {
                    format!("{text}{spaces}")
                };
                Ok(OV::Str(Rc::new(output)))
            }
            lm_abi::INTRINSIC_TEXT_PARSE_FLOAT_STATUS
            | lm_abi::INTRINSIC_TEXT_PARSE_FLOAT_VALUE => {
                let parsed = oracle_parse_float_text(self.as_text(&values[0])?);
                if intrinsic == lm_abi::INTRINSIC_TEXT_PARSE_FLOAT_STATUS {
                    return Ok(OV::Int(parsed.err().unwrap_or(0)));
                }
                let value = parsed.unwrap_or(0.0);
                Ok(OV::Float(lm_value::canonical_float_bits(value.to_bits())))
            }
            lm_abi::INTRINSIC_STRING_EQ | lm_abi::INTRINSIC_STRING_NE => {
                let equal = self.as_text(&values[0])? == self.as_text(&values[1])?;
                Ok(OV::Bool(
                    equal == (intrinsic == lm_abi::INTRINSIC_STRING_EQ),
                ))
            }
            lm_abi::INTRINSIC_TEXT_LT
            | lm_abi::INTRINSIC_TEXT_LE
            | lm_abi::INTRINSIC_TEXT_GT
            | lm_abi::INTRINSIC_TEXT_GE => {
                let left = self.as_text(&values[0])?;
                let right = self.as_text(&values[1])?;
                Ok(OV::Bool(match intrinsic {
                    lm_abi::INTRINSIC_TEXT_LT => left < right,
                    lm_abi::INTRINSIC_TEXT_LE => left <= right,
                    lm_abi::INTRINSIC_TEXT_GT => left > right,
                    _ => left >= right,
                }))
            }
            lm_abi::INTRINSIC_TEXT_AT => {
                let text = self.as_text(&values[0])?;
                let index = usize::try_from(self.as_int(&values[1])?)
                    .map_err(|_| Stop::Fault("IndexOutOfBounds"))?;
                text.chars()
                    .nth(index)
                    .map(OV::Char)
                    .ok_or(Stop::Fault("IndexOutOfBounds"))
            }
            lm_abi::INTRINSIC_TEXT_AT_BYTE => {
                let text = self.as_text(&values[0])?;
                let index = usize::try_from(self.as_int(&values[1])?)
                    .map_err(|_| Stop::Fault("IndexOutOfBounds"))?;
                text.get(index..)
                    .and_then(|suffix| suffix.chars().next())
                    .map(OV::Char)
                    .ok_or(Stop::Fault("IndexOutOfBounds"))
            }
            lm_abi::INTRINSIC_TEXT_SLICE => {
                let text = self.as_text(&values[0])?;
                let start = usize::try_from(self.as_int(&values[1])?)
                    .map_err(|_| Stop::Fault("IndexOutOfBounds"))?;
                let length = usize::try_from(self.as_int(&values[2])?)
                    .map_err(|_| Stop::Fault("IndexOutOfBounds"))?;
                let visible: String = text.chars().skip(start).take(length).collect();
                if visible.chars().count() != length || start > text.chars().count() {
                    return Err(Stop::Fault("IndexOutOfBounds"));
                }
                Ok(OV::Substring(Rc::new(visible)))
            }
            lm_abi::INTRINSIC_TEXT_IS_BOUNDARY => {
                let text = self.as_text(&values[0])?;
                let index = usize::try_from(self.as_int(&values[1])?)
                    .map_err(|_| Stop::Fault("IndexOutOfBounds"))?;
                Ok(OV::Bool(text.is_char_boundary(index)))
            }
            lm_abi::INTRINSIC_TEXT_SLICE_BYTES => {
                let text = self.as_text(&values[0])?;
                let start = usize::try_from(self.as_int(&values[1])?)
                    .map_err(|_| Stop::Fault("IndexOutOfBounds"))?;
                let length = usize::try_from(self.as_int(&values[2])?)
                    .map_err(|_| Stop::Fault("IndexOutOfBounds"))?;
                let end = start
                    .checked_add(length)
                    .ok_or(Stop::Fault("IndexOutOfBounds"))?;
                let visible = text
                    .get(start..end)
                    .ok_or(Stop::Fault("IndexOutOfBounds"))?;
                Ok(OV::Substring(Rc::new(visible.to_string())))
            }
            lm_abi::INTRINSIC_TEXT_BYTES => {
                Ok(self.alloc(OKind::Bytes(self.as_text(&values[0])?.as_bytes().to_vec())))
            }
            lm_abi::INTRINSIC_SUBSTRING_TO_STRING => match &values[0] {
                OV::Substring(text) => Ok(OV::Str(text.clone())),
                _ => Err(Stop::Limit("String conversion needs a Substring")),
            },
            lm_abi::INTRINSIC_CHAR_CODEPOINT => {
                Ok(OV::Int(i64::from(u32::from(self.as_char(&values[0])?))))
            }
            lm_abi::INTRINSIC_CHAR_UTF8_LEN => {
                Ok(OV::Int(self.as_char(&values[0])?.len_utf8() as i64))
            }
            lm_abi::INTRINSIC_CHAR_EQ
            | lm_abi::INTRINSIC_CHAR_NE
            | lm_abi::INTRINSIC_CHAR_LT
            | lm_abi::INTRINSIC_CHAR_LE
            | lm_abi::INTRINSIC_CHAR_GT
            | lm_abi::INTRINSIC_CHAR_GE => {
                let left = self.as_char(&values[0])?;
                let right = self.as_char(&values[1])?;
                Ok(OV::Bool(match intrinsic {
                    lm_abi::INTRINSIC_CHAR_EQ => left == right,
                    lm_abi::INTRINSIC_CHAR_NE => left != right,
                    lm_abi::INTRINSIC_CHAR_LT => left < right,
                    lm_abi::INTRINSIC_CHAR_LE => left <= right,
                    lm_abi::INTRINSIC_CHAR_GT => left > right,
                    _ => left >= right,
                }))
            }
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
            lm_abi::INTRINSIC_TEXT_SPLIT => {
                let text = self.as_text(&values[0])?;
                let needle = self.as_text(&values[1])?;
                let pieces: Vec<OV> = text
                    .split(needle)
                    .map(|piece| OV::Substring(Rc::new(piece.to_string())))
                    .collect();
                Ok(self.alloc(OKind::List(pieces)))
            }
            lm_abi::INTRINSIC_TEXT_LINES => {
                let text = self.as_text(&values[0])?;
                let pieces: Vec<OV> = text
                    .lines()
                    .map(|piece| OV::Substring(Rc::new(piece.to_string())))
                    .collect();
                Ok(self.alloc(OKind::List(pieces)))
            }
            lm_abi::INTRINSIC_BYTES_ENDS_WITH => {
                let bytes = self.as_obj(&values[0])?;
                let suffix = self.as_obj(&values[1])?;
                let found = match (&bytes.borrow().kind, &suffix.borrow().kind) {
                    (OKind::Bytes(bytes), OKind::Bytes(suffix)) => bytes.ends_with(suffix),
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                Ok(OV::Bool(found))
            }
            lm_abi::INTRINSIC_BYTES_CONTAINS => {
                let bytes = self.as_obj(&values[0])?;
                let needle = self.as_obj(&values[1])?;
                let found = match (&bytes.borrow().kind, &needle.borrow().kind) {
                    (OKind::Bytes(bytes), OKind::Bytes(needle)) => {
                        needle.is_empty()
                            || bytes.windows(needle.len()).any(|window| window == needle)
                    }
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
            lm_abi::INTRINSIC_BYTES_TEXT_VIEW => {
                let bytes = self.as_obj(&values[0])?;
                let bytes = match &bytes.borrow().kind {
                    OKind::Bytes(bytes) => bytes.clone(),
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                String::from_utf8(bytes)
                    .map(|text| OV::Substring(Rc::new(text)))
                    .map_err(|_| Stop::Fault("BadCast"))
            }
            lm_abi::INTRINSIC_BYTES_COMPACT => {
                let bytes = self.as_obj(&values[0])?;
                let bytes = match &bytes.borrow().kind {
                    OKind::Bytes(bytes) => bytes.clone(),
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                Ok(self.alloc(OKind::Bytes(bytes)))
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
            lm_abi::INTRINSIC_BYTES_LT
            | lm_abi::INTRINSIC_BYTES_LE
            | lm_abi::INTRINSIC_BYTES_GT
            | lm_abi::INTRINSIC_BYTES_GE => {
                let left = self.as_obj(&values[0])?;
                let right = self.as_obj(&values[1])?;
                let result = match (&left.borrow().kind, &right.borrow().kind) {
                    (OKind::Bytes(left), OKind::Bytes(right)) => match intrinsic {
                        lm_abi::INTRINSIC_BYTES_LT => left < right,
                        lm_abi::INTRINSIC_BYTES_LE => left <= right,
                        lm_abi::INTRINSIC_BYTES_GT => left > right,
                        _ => left >= right,
                    },
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                Ok(OV::Bool(result))
            }
            lm_abi::INTRINSIC_BYTES_BIT_AND
            | lm_abi::INTRINSIC_BYTES_BIT_OR
            | lm_abi::INTRINSIC_BYTES_BIT_XOR => {
                let left = self.as_obj(&values[0])?;
                let right = self.as_obj(&values[1])?;
                let left = match &left.borrow().kind {
                    OKind::Bytes(bytes) => bytes.clone(),
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                let right = match &right.borrow().kind {
                    OKind::Bytes(bytes) => bytes.clone(),
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                if left.len() != right.len() {
                    return Err(Stop::Fault("LengthMismatch"));
                }
                let bytes = left
                    .iter()
                    .zip(right)
                    .map(|(left, right)| match intrinsic {
                        lm_abi::INTRINSIC_BYTES_BIT_AND => left & right,
                        lm_abi::INTRINSIC_BYTES_BIT_OR => left | right,
                        _ => left ^ right,
                    })
                    .collect();
                Ok(self.alloc(OKind::Bytes(bytes)))
            }
            lm_abi::INTRINSIC_BYTES_BIT_NOT => {
                let bytes = self.as_obj(&values[0])?;
                let bytes = match &bytes.borrow().kind {
                    OKind::Bytes(bytes) => bytes.iter().map(|value| !value).collect(),
                    _ => return Err(Stop::Limit("bytes op on a non-bytes value")),
                };
                Ok(self.alloc(OKind::Bytes(bytes)))
            }
            lm_abi::INTRINSIC_STRING_BUILDER_APPEND => {
                let builder = self.as_obj(&values[0])?;
                frozen_guard(&builder)?;
                let text = self.as_text(&values[1])?.to_string();
                match &mut builder.borrow_mut().kind {
                    OKind::Sb(Some(buffer)) => buffer.push_str(&text),
                    OKind::Sb(None) => return Err(Stop::Fault("InvalidVmState")),
                    _ => return Err(Stop::Limit("builder op on a non-builder")),
                }
                Ok(values[0].clone())
            }
            lm_abi::INTRINSIC_STRING_BUILDER_APPEND_INT
            | lm_abi::INTRINSIC_STRING_BUILDER_APPEND_BOOL
            | lm_abi::INTRINSIC_STRING_BUILDER_APPEND_FLOAT => {
                let builder = self.as_obj(&values[0])?;
                frozen_guard(&builder)?;
                let text = if intrinsic == lm_abi::INTRINSIC_STRING_BUILDER_APPEND_INT {
                    self.as_int(&values[1])?.to_string()
                } else if intrinsic == lm_abi::INTRINSIC_STRING_BUILDER_APPEND_BOOL {
                    match &values[1] {
                        OV::Bool(true) => "true".to_string(),
                        OV::Bool(false) => "false".to_string(),
                        _ => return Err(Stop::Limit("expected a Bool value")),
                    }
                } else {
                    self.as_float(&values[1])?.to_string()
                };
                match &mut builder.borrow_mut().kind {
                    OKind::Sb(Some(buffer)) => buffer.push_str(&text),
                    OKind::Sb(None) => return Err(Stop::Fault("InvalidVmState")),
                    _ => return Err(Stop::Limit("builder op on a non-builder")),
                }
                Ok(values[0].clone())
            }
            lm_abi::INTRINSIC_STRING_BUILDER_LEN => {
                let builder = self.as_obj(&values[0])?;
                let len = match &builder.borrow().kind {
                    OKind::Sb(Some(buffer)) => buffer.chars().count(),
                    OKind::Sb(None) => return Err(Stop::Fault("InvalidVmState")),
                    _ => return Err(Stop::Limit("builder op on a non-builder")),
                };
                Ok(OV::Int(len as i64))
            }
            lm_abi::INTRINSIC_STRING_BUILDER_CLEAR => {
                let builder = self.as_obj(&values[0])?;
                frozen_guard(&builder)?;
                match &mut builder.borrow_mut().kind {
                    OKind::Sb(Some(buffer)) => buffer.clear(),
                    OKind::Sb(None) => return Err(Stop::Fault("InvalidVmState")),
                    _ => return Err(Stop::Limit("builder op on a non-builder")),
                }
                Ok(values[0].clone())
            }
            lm_abi::INTRINSIC_STRING_BUILDER_BUILD => {
                let builder = self.as_obj(&values[0])?;
                let text = match &builder.borrow().kind {
                    OKind::Sb(Some(buffer)) => buffer.clone(),
                    OKind::Sb(None) => return Err(Stop::Fault("InvalidVmState")),
                    _ => return Err(Stop::Limit("builder op on a non-builder")),
                };
                Ok(OV::Str(Rc::new(text)))
            }
            lm_abi::INTRINSIC_STRING_BUILDER_PUSH_CHAR => {
                let builder = self.as_obj(&values[0])?;
                frozen_guard(&builder)?;
                let value = self.as_char(&values[1])?;
                match &mut builder.borrow_mut().kind {
                    OKind::Sb(Some(buffer)) => buffer.push(value),
                    OKind::Sb(None) => return Err(Stop::Fault("InvalidVmState")),
                    _ => return Err(Stop::Limit("builder op on a non-builder")),
                }
                Ok(values[0].clone())
            }
            lm_abi::INTRINSIC_STRING_BUILDER_BYTE_LEN => {
                let builder = self.as_obj(&values[0])?;
                let len = match &builder.borrow().kind {
                    OKind::Sb(Some(buffer)) => buffer.len(),
                    OKind::Sb(None) => return Err(Stop::Fault("InvalidVmState")),
                    _ => return Err(Stop::Limit("builder op on a non-builder")),
                };
                Ok(OV::Int(len as i64))
            }
            lm_abi::INTRINSIC_STRING_BUILDER_FINISH => {
                let builder = self.as_obj(&values[0])?;
                frozen_guard(&builder)?;
                let text = match &mut builder.borrow_mut().kind {
                    OKind::Sb(buffer) => buffer.take().ok_or(Stop::Fault("InvalidVmState"))?,
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
                    OKind::Bb(Some(bytes)) => bytes.push(byte),
                    OKind::Bb(None) => return Err(Stop::Fault("InvalidVmState")),
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
                    OKind::Bb(Some(bytes)) => bytes.extend_from_slice(&source),
                    OKind::Bb(None) => return Err(Stop::Fault("InvalidVmState")),
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
                    OKind::Bb(Some(bytes)) => bytes
                        .try_reserve(additional)
                        .map_err(|_| Stop::Fault("HeapLimit"))?,
                    OKind::Bb(None) => return Err(Stop::Fault("InvalidVmState")),
                    _ => return Err(Stop::Limit("buffer op on a non-buffer")),
                }
                Ok(values[0].clone())
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_CLEAR => {
                let buffer = self.as_obj(&values[0])?;
                frozen_guard(&buffer)?;
                match &mut buffer.borrow_mut().kind {
                    OKind::Bb(Some(bytes)) => bytes.clear(),
                    OKind::Bb(None) => return Err(Stop::Fault("InvalidVmState")),
                    _ => return Err(Stop::Limit("buffer op on a non-buffer")),
                }
                Ok(values[0].clone())
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_LEN => {
                let buffer = self.as_obj(&values[0])?;
                let len = match &buffer.borrow().kind {
                    OKind::Bb(Some(bytes)) => bytes.len(),
                    OKind::Bb(None) => return Err(Stop::Fault("InvalidVmState")),
                    _ => return Err(Stop::Limit("buffer op on a non-buffer")),
                };
                Ok(OV::Int(len as i64))
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_BUILD => {
                let buffer = self.as_obj(&values[0])?;
                let bytes = match &buffer.borrow().kind {
                    OKind::Bb(Some(bytes)) => bytes.clone(),
                    OKind::Bb(None) => return Err(Stop::Fault("InvalidVmState")),
                    _ => return Err(Stop::Limit("buffer op on a non-buffer")),
                };
                Ok(self.alloc(OKind::Bytes(bytes)))
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_FINISH => {
                let buffer = self.as_obj(&values[0])?;
                frozen_guard(&buffer)?;
                let bytes = match &mut buffer.borrow_mut().kind {
                    OKind::Bb(bytes) => bytes.take().ok_or(Stop::Fault("InvalidVmState"))?,
                    _ => return Err(Stop::Limit("buffer op on a non-buffer")),
                };
                Ok(self.alloc(OKind::Bytes(bytes)))
            }
            lm_abi::INTRINSIC_LIST_LEN
            | lm_abi::INTRINSIC_LIST_CAPACITY
            | lm_abi::INTRINSIC_LIST_EPOCH => {
                let list = self.as_obj(&values[0])?;
                let value = match &list.borrow().kind {
                    OKind::List(items) if intrinsic == lm_abi::INTRINSIC_LIST_CAPACITY => {
                        items.capacity()
                    }
                    OKind::List(items) if intrinsic == lm_abi::INTRINSIC_LIST_LEN => items.len(),
                    OKind::List(_) => 0,
                    _ => return Err(Stop::Limit("list op on a non-list")),
                };
                Ok(OV::Int(value as i64))
            }
            lm_abi::INTRINSIC_LIST_AT => {
                let list = self.as_obj(&values[0])?;
                let index = self.as_int(&values[1])?;
                let value = match &list.borrow().kind {
                    OKind::List(items) if index >= 0 => items.get(index as usize).cloned(),
                    OKind::List(_) => None,
                    _ => return Err(Stop::Limit("list op on a non-list")),
                };
                value.ok_or(Stop::Fault("IndexOutOfBounds"))
            }
            lm_abi::INTRINSIC_LIST_GET => {
                let list = self.as_obj(&values[0])?;
                let index = self.as_int(&values[1])?;
                let value = match &list.borrow().kind {
                    OKind::List(items) if index >= 0 => items.get(index as usize).cloned(),
                    OKind::List(_) => None,
                    _ => return Err(Stop::Limit("list op on a non-list")),
                };
                Ok(self.option_value(value))
            }
            lm_abi::INTRINSIC_LIST_PUSH => {
                let list = self.as_obj(&values[0])?;
                frozen_guard(&list)?;
                match &mut list.borrow_mut().kind {
                    OKind::List(items) => items.push(values[1].clone()),
                    _ => return Err(Stop::Limit("list op on a non-list")),
                }
                Ok(OV::Unit)
            }
            lm_abi::INTRINSIC_LIST_ITER_LEN => {
                let list = self.as_obj(&values[0])?;
                let length = match &list.borrow().kind {
                    OKind::List(items) => items.len(),
                    _ => return Err(Stop::Limit("list op on a non-list")),
                };
                Ok(OV::Int(length as i64))
            }
            lm_abi::INTRINSIC_LIST_SET => {
                let list = self.as_obj(&values[0])?;
                frozen_guard(&list)?;
                let index = self.as_int(&values[1])?;
                let mut borrow = list.borrow_mut();
                let item = match &mut borrow.kind {
                    OKind::List(items) if index >= 0 => items.get_mut(index as usize),
                    OKind::List(_) => None,
                    _ => return Err(Stop::Limit("list op on a non-list")),
                }
                .ok_or(Stop::Fault("IndexOutOfBounds"))?;
                *item = values[2].clone();
                Ok(OV::Unit)
            }
            lm_abi::INTRINSIC_LIST_POP => {
                let list = self.as_obj(&values[0])?;
                frozen_guard(&list)?;
                let value = match &mut list.borrow_mut().kind {
                    OKind::List(items) => items.pop(),
                    _ => return Err(Stop::Limit("list op on a non-list")),
                };
                Ok(self.option_value(value))
            }
            lm_abi::INTRINSIC_LIST_INSERT => {
                let list = self.as_obj(&values[0])?;
                frozen_guard(&list)?;
                let index = self.as_int(&values[1])?;
                let mut borrow = list.borrow_mut();
                let items = match &mut borrow.kind {
                    OKind::List(items) => items,
                    _ => return Err(Stop::Limit("list op on a non-list")),
                };
                if index < 0 || index as usize > items.len() {
                    return Err(Stop::Fault("IndexOutOfBounds"));
                }
                items.insert(index as usize, values[2].clone());
                Ok(OV::Unit)
            }
            lm_abi::INTRINSIC_LIST_REMOVE | lm_abi::INTRINSIC_LIST_SWAP_REMOVE => {
                let list = self.as_obj(&values[0])?;
                frozen_guard(&list)?;
                let index = self.as_int(&values[1])?;
                let mut borrow = list.borrow_mut();
                let items = match &mut borrow.kind {
                    OKind::List(items) => items,
                    _ => return Err(Stop::Limit("list op on a non-list")),
                };
                if index < 0 || index as usize >= items.len() {
                    return Err(Stop::Fault("IndexOutOfBounds"));
                }
                if intrinsic == lm_abi::INTRINSIC_LIST_REMOVE {
                    Ok(items.remove(index as usize))
                } else {
                    Ok(items.swap_remove(index as usize))
                }
            }
            lm_abi::INTRINSIC_LIST_RESERVE => {
                let list = self.as_obj(&values[0])?;
                frozen_guard(&list)?;
                let additional = usize::try_from(self.as_int(&values[1])?)
                    .map_err(|_| Stop::Fault("IndexOutOfBounds"))?;
                match &mut list.borrow_mut().kind {
                    OKind::List(items) => items.reserve(additional),
                    _ => return Err(Stop::Limit("list op on a non-list")),
                }
                Ok(OV::Unit)
            }
            lm_abi::INTRINSIC_LIST_TRUNCATE => {
                let list = self.as_obj(&values[0])?;
                frozen_guard(&list)?;
                let length = usize::try_from(self.as_int(&values[1])?)
                    .map_err(|_| Stop::Fault("IndexOutOfBounds"))?;
                match &mut list.borrow_mut().kind {
                    OKind::List(items) => items.truncate(length),
                    _ => return Err(Stop::Limit("list op on a non-list")),
                }
                Ok(OV::Unit)
            }
            lm_abi::INTRINSIC_LIST_CONTAINS => {
                let list = self.as_obj(&values[0])?;
                let found = match &list.borrow().kind {
                    OKind::List(items) => items.iter().any(|item| self.value_eq(item, &values[1])),
                    _ => return Err(Stop::Limit("list op on a non-list")),
                };
                Ok(OV::Bool(found))
            }
            lm_abi::INTRINSIC_MAP_LEN | lm_abi::INTRINSIC_MAP_EPOCH => {
                let map = self.as_obj(&values[0])?;
                let value = match &map.borrow().kind {
                    OKind::Map(entries) if intrinsic == lm_abi::INTRINSIC_MAP_LEN => entries.len(),
                    OKind::Map(_) => 0,
                    _ => return Err(Stop::Limit("map op on a non-map")),
                };
                Ok(OV::Int(value as i64))
            }
            lm_abi::INTRINSIC_MAP_HAS | lm_abi::INTRINSIC_MAP_AT | lm_abi::INTRINSIC_MAP_GET => {
                let map = self.as_obj(&values[0])?;
                let found = match &map.borrow().kind {
                    OKind::Map(entries) => entries
                        .iter()
                        .find(|(key, _)| self.key_eq(key, &values[1]))
                        .map(|(_, value)| value.clone()),
                    _ => return Err(Stop::Limit("map op on a non-map")),
                };
                match intrinsic {
                    lm_abi::INTRINSIC_MAP_HAS => Ok(OV::Bool(found.is_some())),
                    lm_abi::INTRINSIC_MAP_AT => found.ok_or(Stop::Fault("MissingKey")),
                    _ => Ok(self.option_value(found)),
                }
            }
            lm_abi::INTRINSIC_MAP_PUT => {
                let map = self.as_obj(&values[0])?;
                frozen_guard(&map)?;
                let previous = match &mut map.borrow_mut().kind {
                    OKind::Map(entries) => {
                        match entries
                            .iter_mut()
                            .find(|(key, _)| self.key_eq(key, &values[1]))
                        {
                            Some(entry) => Some(std::mem::replace(&mut entry.1, values[2].clone())),
                            None => {
                                entries.push((values[1].clone(), values[2].clone()));
                                None
                            }
                        }
                    }
                    _ => return Err(Stop::Limit("map op on a non-map")),
                };
                Ok(self.option_value(previous))
            }
            lm_abi::INTRINSIC_MAP_ITER_LEN => {
                let map = self.as_obj(&values[0])?;
                let length = match &map.borrow().kind {
                    OKind::Map(entries) => entries.len(),
                    _ => return Err(Stop::Limit("map op on a non-map")),
                };
                Ok(OV::Int(length as i64))
            }
            lm_abi::INTRINSIC_MAP_KEY_AT | lm_abi::INTRINSIC_MAP_VALUE_AT => {
                let map = self.as_obj(&values[0])?;
                let index = self.as_int(&values[1])?;
                let value = match &map.borrow().kind {
                    OKind::Map(entries) if index >= 0 => entries.get(index as usize).map(|entry| {
                        if intrinsic == lm_abi::INTRINSIC_MAP_KEY_AT {
                            entry.0.clone()
                        } else {
                            entry.1.clone()
                        }
                    }),
                    OKind::Map(_) => None,
                    _ => return Err(Stop::Limit("map op on a non-map")),
                };
                value.ok_or(Stop::Fault("IndexOutOfBounds"))
            }
            lm_abi::INTRINSIC_MAP_REMOVE => {
                let map = self.as_obj(&values[0])?;
                frozen_guard(&map)?;
                let value = match &mut map.borrow_mut().kind {
                    OKind::Map(entries) => entries
                        .iter()
                        .position(|(key, _)| self.key_eq(key, &values[1]))
                        .map(|index| entries.remove(index).1),
                    _ => return Err(Stop::Limit("map op on a non-map")),
                };
                Ok(self.option_value(value))
            }
            lm_abi::INTRINSIC_MAP_CLEAR => {
                let map = self.as_obj(&values[0])?;
                frozen_guard(&map)?;
                match &mut map.borrow_mut().kind {
                    OKind::Map(entries) => entries.clear(),
                    _ => return Err(Stop::Limit("map op on a non-map")),
                }
                Ok(OV::Unit)
            }
            lm_abi::INTRINSIC_MAP_RESERVE => {
                let map = self.as_obj(&values[0])?;
                frozen_guard(&map)?;
                let additional = usize::try_from(self.as_int(&values[1])?)
                    .map_err(|_| Stop::Fault("IndexOutOfBounds"))?;
                match &mut map.borrow_mut().kind {
                    OKind::Map(entries) => entries.reserve(additional),
                    _ => return Err(Stop::Limit("map op on a non-map")),
                }
                Ok(OV::Unit)
            }
            lm_abi::INTRINSIC_PANIC => Err(Stop::Fault("UserPanic")),
            lm_abi::INTRINSIC_ASSERT_FAIL => Err(Stop::Fault("AssertionFailed")),
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
                let previous = match &mut obj.borrow_mut().kind {
                    OKind::Map(entries) => {
                        match entries.iter_mut().find(|(k, _)| self.key_eq(k, &values[1])) {
                            Some(entry) => Some(std::mem::replace(&mut entry.1, values[2].clone())),
                            None => {
                                entries.push((values[1].clone(), values[2].clone()));
                                None
                            }
                        }
                    }
                    _ => return Err(Stop::Limit("map op on a non-map")),
                };
                Ok(self.option_value(previous))
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
            OV::Float(bits) => f64::from_bits(*bits).to_string(),
            OV::Char(value) => format!("{value:?}"),
            OV::Str(s) => render_string(s),
            OV::Substring(s) => render_string(s),
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
                    OKind::Sb(Some(buf)) => {
                        format!("<StringBuilder length {}>", buf.len())
                    }
                    OKind::Sb(None) => "<finished StringBuilder>".to_string(),
                    OKind::Bb(Some(bytes)) => {
                        format!("<ByteBuffer length {}>", bytes.len())
                    }
                    OKind::Bb(None) => "<finished ByteBuffer>".to_string(),
                    OKind::Bytes(bytes) => format!("<Bytes len {}>", bytes.len()),
                }
            }
        }
    }
}

fn oracle_float_eq(left: u64, right: u64) -> bool {
    let left = f64::from_bits(left);
    let right = f64::from_bits(right);
    left == right || (left.is_nan() && right.is_nan())
}

fn oracle_float_hash(bits: u64) -> i64 {
    let bits = lm_value::canonical_float_bits(bits);
    if bits << 1 == 0 {
        0
    } else {
        bits as i64
    }
}

fn oracle_float_fits_int(value: f64) -> bool {
    value >= i64::MIN as f64 && value < 9_223_372_036_854_775_808.0
}

fn oracle_parse_float_text(text: &str) -> Result<f64, i64> {
    match text {
        "NaN" => return Ok(f64::NAN),
        "inf" | "+inf" => return Ok(f64::INFINITY),
        "-inf" => return Ok(f64::NEG_INFINITY),
        _ => {}
    }
    if !oracle_is_decimal_float_text(text) {
        return Err(1);
    }
    let value = text.parse::<f64>().map_err(|_| 1)?;
    if value.is_infinite() {
        Err(2)
    } else {
        Ok(value)
    }
}

fn oracle_is_decimal_float_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut at = usize::from(matches!(bytes.first(), Some(b'+') | Some(b'-')));
    let mut digits = 0usize;
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
        digits += 1;
    }
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return false;
    }
    if matches!(bytes.get(at), Some(b'e') | Some(b'E')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+') | Some(b'-')) {
            at += 1;
        }
        let exponent = at;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == exponent {
            return false;
        }
    }
    at == bytes.len()
}

fn oracle_shift(value: i64) -> Result<u32, Stop> {
    let value = u32::try_from(value).map_err(|_| Stop::Fault("ShiftOutOfRange"))?;
    if value > 63 {
        return Err(Stop::Fault("ShiftOutOfRange"));
    }
    Ok(value)
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
