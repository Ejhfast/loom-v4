//! Lowering from typed HIR to basic-block bytecode.
//!
//! Each expression leaves exactly one value on the operand stack.
//! Each statement leaves the stack unchanged. Every block ends with a
//! terminator. The pass interns strings, types, and selectors in
//! first-encounter order, so the output is deterministic. It also
//! synthesizes one construction function for each class.

use crate::hir::*;
use lm_bytecode::{BcClass, BcType, Func, Instr, Module, NO_PARENT};
use lm_source::ast::BinOp;
use lm_types::{Type, TypeId, TypeStore, BOOL, INT, NEVER, STRING, UNIT};
use std::collections::HashMap;
use std::fmt::Write as _;

/// Module-wide interning state during lowering.
struct ModLowerer<'m> {
    store: &'m TypeStore,
    strings: Vec<String>,
    string_index: HashMap<String, u32>,
    types: Vec<BcType>,
    type_index: HashMap<BcType, u32>,
    selectors: Vec<String>,
    selector_index: HashMap<String, u32>,
    /// The function index of the first synthesized `<new>` function.
    new_base: u32,
}

impl<'m> ModLowerer<'m> {
    fn intern_string(&mut self, value: &str) -> u32 {
        if let Some(idx) = self.string_index.get(value) {
            return *idx;
        }
        let idx = self.strings.len() as u32;
        self.strings.push(value.to_string());
        self.string_index.insert(value.to_string(), idx);
        idx
    }

    fn intern_type(&mut self, ty: BcType) -> u32 {
        if let Some(idx) = self.type_index.get(&ty) {
            return *idx;
        }
        let idx = self.types.len() as u32;
        self.types.push(ty.clone());
        self.type_index.insert(ty, idx);
        idx
    }

    /// Convert an interned checker type to a type-table index.
    /// `Never` types occupy unreachable value positions only, so they
    /// share the unit entry.
    fn bc_ty(&mut self, id: TypeId) -> u32 {
        match self.store.get(id).clone() {
            Type::Unit | Type::Never => self.intern_type(BcType::Unit),
            Type::Bool => self.intern_type(BcType::Bool),
            Type::Int => self.intern_type(BcType::Int),
            Type::String => self.intern_type(BcType::Str),
            Type::StringBuilder => self.intern_type(BcType::StringBuilder),
            Type::ByteBuffer => self.intern_type(BcType::ByteBuffer),
            Type::Class(c) => self.intern_type(BcType::Class(c.0)),
            Type::List(e) => {
                let e = self.bc_ty(e);
                self.intern_type(BcType::List(e))
            }
            Type::Map(k, v) => {
                let k = self.bc_ty(k);
                let v = self.bc_ty(v);
                self.intern_type(BcType::Map(k, v))
            }
            Type::Fn(params, ret) => {
                let params: Vec<u32> = params.iter().map(|p| self.bc_ty(*p)).collect();
                let ret = self.bc_ty(ret);
                self.intern_type(BcType::Fn(params, ret))
            }
        }
    }

    fn selector(&mut self, name: &str) -> u32 {
        if let Some(idx) = self.selector_index.get(name) {
            return *idx;
        }
        let idx = self.selectors.len() as u32;
        self.selectors.push(name.to_string());
        self.selector_index.insert(name.to_string(), idx);
        idx
    }
}

/// Lower a checked module to decoded bytecode.
pub fn lower_module(hir: &HirModule) -> Module {
    let mut m = ModLowerer {
        store: &hir.store,
        strings: Vec::new(),
        string_index: HashMap::new(),
        types: Vec::new(),
        type_index: HashMap::new(),
        selectors: Vec::new(),
        selector_index: HashMap::new(),
        new_base: hir.funcs.len() as u32,
    };
    // The canonical primitive prefix required by the verifier.
    m.intern_type(BcType::Unit);
    m.intern_type(BcType::Bool);
    m.intern_type(BcType::Int);
    m.intern_type(BcType::Str);
    // Selectors in class-declaration order.
    for class in &hir.classes {
        for (name, _) in &class.methods {
            m.selector(name);
        }
    }
    let mut funcs = Vec::new();
    for func in &hir.funcs {
        funcs.push(lower_func(&mut m, func));
    }
    for (cidx, class) in hir.classes.iter().enumerate() {
        funcs.push(lower_new_func(&mut m, class, cidx as u32));
    }
    let classes: Vec<BcClass> = hir
        .classes
        .iter()
        .map(|class| BcClass {
            name: class.name.clone(),
            parent: class.parent.unwrap_or(NO_PARENT),
            fields: class
                .field_names
                .iter()
                .zip(class.field_tys.iter())
                .map(|(name, ty)| (name.clone(), m.bc_ty(*ty)))
                .collect(),
            methods: class
                .methods
                .iter()
                .map(|(name, func)| (m.selector(name), *func))
                .collect(),
        })
        .collect();
    Module {
        strings: m.strings,
        types: m.types,
        selectors: m.selectors,
        classes,
        funcs,
        entry: hir.entry as u32,
    }
}

struct Lowerer<'a, 'm> {
    m: &'a mut ModLowerer<'m>,
    blocks: Vec<Vec<Instr>>,
    cur: usize,
    /// Stack of `(continue_target, break_target)` blocks.
    loops: Vec<(u32, u32)>,
}

impl<'a, 'm> Lowerer<'a, 'm> {
    fn new(m: &'a mut ModLowerer<'m>) -> Lowerer<'a, 'm> {
        Lowerer {
            m,
            blocks: vec![Vec::new()],
            cur: 0,
            loops: Vec::new(),
        }
    }

    fn emit(&mut self, instr: Instr) {
        self.blocks[self.cur].push(instr);
    }

    fn new_block(&mut self) -> u32 {
        self.blocks.push(Vec::new());
        (self.blocks.len() - 1) as u32
    }

    fn switch_to(&mut self, block: u32) {
        self.cur = block as usize;
    }

    /// Close every open block and return the block list.
    fn finish(mut self, pushed: bool) -> Vec<Vec<Instr>> {
        if pushed {
            self.emit(Instr::Return);
        }
        // Close every open block. Only dead continuation blocks stay
        // open here. They receive an explicit return, so the structure
        // is valid.
        for block in &mut self.blocks {
            let terminated = block.last().map(Instr::is_terminator).unwrap_or(false);
            if !terminated {
                block.push(Instr::ConstUnit);
                block.push(Instr::Return);
            }
        }
        self.blocks
    }

    /// Lower a statement. The operand stack is unchanged.
    fn lower_stmt(&mut self, stmt: &HStmt) {
        match stmt {
            HStmt::Assign { slot, value } => {
                self.lower_expr(value);
                self.emit(Instr::StoreLocal(*slot));
            }
            HStmt::AssignField { recv, field, value } => {
                self.lower_expr(recv);
                self.lower_expr(value);
                self.emit(Instr::StoreField(*field));
            }
            HStmt::While { cond, body } => {
                let cond_b = self.new_block();
                self.emit(Instr::Jump(cond_b));
                self.switch_to(cond_b);
                self.lower_expr(cond);
                let body_b = self.new_block();
                let exit_b = self.new_block();
                self.emit(Instr::JumpIfFalse(exit_b));
                self.emit(Instr::Jump(body_b));
                self.switch_to(body_b);
                self.loops.push((cond_b, exit_b));
                self.lower_block_stmt(body);
                self.loops.pop();
                self.emit(Instr::Jump(cond_b));
                self.switch_to(exit_b);
            }
            HStmt::Return { value } => {
                match value {
                    Some(value) => self.lower_expr(value),
                    None => self.emit(Instr::ConstUnit),
                }
                self.emit(Instr::Return);
                let dead = self.new_block();
                self.switch_to(dead);
            }
            HStmt::Break => {
                let (_, exit_b) = *self.loops.last().expect("checked loop context");
                self.emit(Instr::Jump(exit_b));
                let dead = self.new_block();
                self.switch_to(dead);
            }
            HStmt::Continue => {
                let (cond_b, _) = *self.loops.last().expect("checked loop context");
                self.emit(Instr::Jump(cond_b));
                let dead = self.new_block();
                self.switch_to(dead);
            }
            HStmt::Expr(expr) => {
                self.lower_expr(expr);
                self.emit(Instr::Pop);
            }
        }
    }

    /// Lower a statement list without a value. Return true when the
    /// list ends with a diverging statement.
    fn lower_block_stmt(&mut self, stmts: &[HStmt]) -> bool {
        for stmt in stmts {
            self.lower_stmt(stmt);
        }
        stmts.last().map(HStmt::diverges).unwrap_or(false)
    }

    /// Lower a statement list that produces one value. Return false
    /// when the list ends with a diverging statement and pushes nothing.
    fn lower_block_value(&mut self, stmts: &[HStmt]) -> bool {
        let Some((last, init)) = stmts.split_last() else {
            self.emit(Instr::ConstUnit);
            return true;
        };
        for stmt in init {
            self.lower_stmt(stmt);
        }
        match last {
            HStmt::Expr(expr) => {
                self.lower_expr(expr);
                true
            }
            stmt if stmt.diverges() => {
                self.lower_stmt(stmt);
                false
            }
            stmt => {
                self.lower_stmt(stmt);
                self.emit(Instr::ConstUnit);
                true
            }
        }
    }

    /// Lower an expression. Exactly one value is pushed unless the
    /// expression cannot complete.
    fn lower_expr(&mut self, expr: &HExpr) {
        match &expr.kind {
            HExprKind::Int(v) => self.emit(Instr::ConstInt(*v)),
            HExprKind::Bool(v) => self.emit(Instr::ConstBool(*v)),
            HExprKind::Str(v) => {
                let idx = self.m.intern_string(v);
                self.emit(Instr::ConstStr(idx));
            }
            HExprKind::Local(slot) => self.emit(Instr::LoadLocal(*slot)),
            HExprKind::Capture(idx) => self.emit(Instr::LoadCapture(*idx)),
            HExprKind::Not(inner) => {
                self.lower_expr(inner);
                self.emit(Instr::Not);
            }
            HExprKind::Neg(inner) => {
                self.lower_expr(inner);
                self.emit(Instr::Neg);
            }
            HExprKind::Binary {
                op,
                operand_ty,
                left,
                right,
            } => {
                self.lower_expr(left);
                self.lower_expr(right);
                self.emit(binary_instr(*op, *operand_ty));
            }
            HExprKind::And(left, right) => {
                self.lower_expr(left);
                let false_b = self.new_block();
                let join_b = self.new_block();
                self.emit(Instr::JumpIfFalse(false_b));
                self.lower_expr(right);
                self.emit(Instr::Jump(join_b));
                self.switch_to(false_b);
                self.emit(Instr::ConstBool(false));
                self.emit(Instr::Jump(join_b));
                self.switch_to(join_b);
            }
            HExprKind::Or(left, right) => {
                self.lower_expr(left);
                let true_b = self.new_block();
                let join_b = self.new_block();
                self.emit(Instr::JumpIfTrue(true_b));
                self.lower_expr(right);
                self.emit(Instr::Jump(join_b));
                self.switch_to(true_b);
                self.emit(Instr::ConstBool(true));
                self.emit(Instr::Jump(join_b));
                self.switch_to(join_b);
            }
            HExprKind::Call { func, args } => {
                for arg in args {
                    self.lower_expr(arg);
                }
                self.emit(Instr::Call(*func));
            }
            HExprKind::Construct { class, args } => {
                for arg in args {
                    self.lower_expr(arg);
                }
                let target = self.m.new_base + *class;
                self.emit(Instr::Call(target));
            }
            HExprKind::MethodCall {
                recv,
                selector,
                args,
            } => {
                self.lower_expr(recv);
                for arg in args {
                    self.lower_expr(arg);
                }
                let sel = self.m.selector(selector);
                self.emit(Instr::CallVirtual {
                    selector: sel,
                    argc: args.len() as u32,
                });
            }
            HExprKind::FieldGet { recv, field } => {
                self.lower_expr(recv);
                self.emit(Instr::LoadField(*field));
            }
            HExprKind::MakeClosure { func, captures } => {
                for capture in captures {
                    self.lower_expr(capture);
                }
                // The verifier resolves the closure type through the
                // type table, so the entry must exist.
                self.m.bc_ty(expr.ty);
                self.emit(Instr::MakeClosure {
                    func: *func,
                    captures: captures.len() as u32,
                });
            }
            HExprKind::CallValue { callee, args } => {
                self.lower_expr(callee);
                for arg in args {
                    self.lower_expr(arg);
                }
                self.emit(Instr::CallValue {
                    argc: args.len() as u32,
                });
            }
            HExprKind::ListLit(items) => {
                for item in items {
                    self.lower_expr(item);
                }
                let ty = self.m.bc_ty(expr.ty);
                self.emit(Instr::ListNew {
                    ty,
                    count: items.len() as u32,
                });
            }
            HExprKind::MapLit(entries) => {
                for (key, value) in entries {
                    self.lower_expr(key);
                    self.lower_expr(value);
                }
                let ty = self.m.bc_ty(expr.ty);
                self.emit(Instr::MapNew {
                    ty,
                    count: entries.len() as u32,
                });
            }
            HExprKind::Native { op, args } => {
                for arg in args {
                    self.lower_expr(arg);
                }
                let instr = match op {
                    NativeOp::ListLen => Instr::ListLen,
                    NativeOp::ListAt => Instr::ListAt,
                    NativeOp::ListPush => Instr::ListPush,
                    NativeOp::MapLen => Instr::MapLen,
                    NativeOp::MapHas => Instr::MapHas,
                    NativeOp::MapAt => Instr::MapAt,
                    NativeOp::MapPut => Instr::MapPut,
                    NativeOp::SbNew => {
                        self.m.intern_type(BcType::StringBuilder);
                        Instr::SbNew
                    }
                    NativeOp::SbAppend => Instr::SbAppendStr,
                    NativeOp::SbBuild => Instr::SbBuild,
                    NativeOp::BbNew => {
                        self.m.intern_type(BcType::ByteBuffer);
                        Instr::BbNew
                    }
                    NativeOp::BbAppend => Instr::BbAppend,
                    NativeOp::BbLen => Instr::BbLen,
                    NativeOp::BbBuild => Instr::BbBuild,
                    NativeOp::Freeze => Instr::Freeze,
                };
                self.emit(instr);
            }
            HExprKind::Interp(parts) => {
                self.m.intern_type(BcType::StringBuilder);
                self.emit(Instr::SbNew);
                for part in parts {
                    match part {
                        HInterpPart::Lit(text) => {
                            let idx = self.m.intern_string(text);
                            self.emit(Instr::ConstStr(idx));
                            self.emit(Instr::SbAppendStr);
                        }
                        HInterpPart::Expr(e) => {
                            self.lower_expr(e);
                            let instr = match e.ty {
                                INT => Instr::SbAppendInt,
                                BOOL => Instr::SbAppendBool,
                                _ => Instr::SbAppendStr,
                            };
                            self.emit(instr);
                        }
                    }
                }
                self.emit(Instr::SbBuild);
            }
            HExprKind::If { arms, else_body } => {
                let join_b = self.new_block();
                let unit_valued = expr.ty == UNIT;
                for (cond, body) in arms {
                    self.lower_expr(cond);
                    let next_b = self.new_block();
                    self.emit(Instr::JumpIfFalse(next_b));
                    self.lower_branch(body, unit_valued, join_b);
                    self.switch_to(next_b);
                }
                match else_body {
                    Some(body) => self.lower_branch(body, unit_valued, join_b),
                    None => {
                        self.emit(Instr::ConstUnit);
                        self.emit(Instr::Jump(join_b));
                    }
                }
                self.switch_to(join_b);
            }
        }
    }

    /// Lower one `if` branch body and jump to the join block with one
    /// value on the stack.
    fn lower_branch(&mut self, body: &[HStmt], unit_valued: bool, join_b: u32) {
        let pushed = if unit_valued {
            let diverged = self.lower_block_stmt(body);
            if !diverged {
                self.emit(Instr::ConstUnit);
            }
            !diverged
        } else {
            self.lower_block_value(body)
        };
        if pushed {
            self.emit(Instr::Jump(join_b));
        }
    }
}

fn binary_instr(op: BinOp, operand_ty: TypeId) -> Instr {
    match op {
        BinOp::Add => Instr::Add,
        BinOp::Sub => Instr::Sub,
        BinOp::Mul => Instr::Mul,
        BinOp::Div => Instr::Div,
        BinOp::Rem => Instr::Rem,
        BinOp::Lt => Instr::LtInt,
        BinOp::Le => Instr::LeInt,
        BinOp::Gt => Instr::GtInt,
        BinOp::Ge => Instr::GeInt,
        BinOp::Eq => match operand_ty {
            BOOL => Instr::EqBool,
            STRING => Instr::EqStr,
            INT | NEVER | UNIT => Instr::EqInt,
            _ => Instr::EqRef,
        },
        BinOp::Ne => match operand_ty {
            BOOL => Instr::NeBool,
            STRING => Instr::NeStr,
            INT | NEVER | UNIT => Instr::NeInt,
            _ => Instr::NeRef,
        },
    }
}

fn lower_func(m: &mut ModLowerer<'_>, func: &HirFunc) -> Func {
    let params: Vec<u32> = func.params.iter().map(|t| m.bc_ty(*t)).collect();
    let ret = m.bc_ty(func.ret);
    let captures: Vec<u32> = func.captures.iter().map(|t| m.bc_ty(*t)).collect();
    let mut lowerer = Lowerer::new(m);
    let unit_ret = func.ret == UNIT;
    let pushed = if unit_ret {
        let diverged = lowerer.lower_block_stmt(&func.body);
        if !diverged {
            lowerer.emit(Instr::ConstUnit);
        }
        !diverged
    } else {
        lowerer.lower_block_value(&func.body)
    };
    let blocks = lowerer.finish(pushed);
    Func {
        name: func.name.clone(),
        params,
        ret,
        captures,
        local_count: func.locals.len() as u32,
        blocks,
    }
}

/// Synthesize the `<new>` construction function for one class:
/// allocate, evaluate defaults, run `init`, and return the instance.
fn lower_new_func(m: &mut ModLowerer<'_>, class: &HirClass, cidx: u32) -> Func {
    let params: Vec<u32> = class.ctor_params.iter().map(|t| m.bc_ty(*t)).collect();
    let ret = m.intern_type(BcType::Class(cidx));
    let self_slot = params.len() as u32;
    let mut lowerer = Lowerer::new(m);
    lowerer.emit(Instr::New(cidx));
    lowerer.emit(Instr::StoreLocal(self_slot));
    for (fidx, default) in class.defaults.iter().enumerate() {
        if let Some(expr) = default {
            lowerer.emit(Instr::LoadLocal(self_slot));
            lowerer.lower_expr(expr);
            lowerer.emit(Instr::StoreField(fidx as u32));
        }
    }
    if let Some(init) = class.init {
        lowerer.emit(Instr::LoadLocal(self_slot));
        for i in 0..self_slot {
            lowerer.emit(Instr::LoadLocal(i));
        }
        lowerer.emit(Instr::Call(init));
        lowerer.emit(Instr::Pop);
    }
    lowerer.emit(Instr::LoadLocal(self_slot));
    let blocks = lowerer.finish(true);
    Func {
        name: format!("<new {}>", class.name),
        params,
        ret,
        captures: vec![],
        local_count: self_slot + 1,
        blocks,
    }
}

/// Count the values an instruction pops and pushes.
fn stack_effect(module: &Module, instr: &Instr) -> (usize, usize) {
    match instr {
        Instr::ConstUnit
        | Instr::ConstBool(_)
        | Instr::ConstInt(_)
        | Instr::ConstStr(_)
        | Instr::LoadLocal(_)
        | Instr::LoadCapture(_)
        | Instr::New(_)
        | Instr::SbNew
        | Instr::BbNew => (0, 1),
        Instr::StoreLocal(_) | Instr::Pop => (1, 0),
        Instr::Add
        | Instr::Sub
        | Instr::Mul
        | Instr::Div
        | Instr::Rem
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
        | Instr::NeRef => (2, 1),
        Instr::Neg
        | Instr::Not
        | Instr::LoadField(_)
        | Instr::ListLen
        | Instr::MapLen
        | Instr::SbBuild
        | Instr::BbLen
        | Instr::BbBuild
        | Instr::Freeze => (1, 1),
        Instr::StoreField(_) => (2, 0),
        Instr::ListAt
        | Instr::ListPush
        | Instr::MapHas
        | Instr::MapAt
        | Instr::SbAppendStr
        | Instr::SbAppendInt
        | Instr::SbAppendBool
        | Instr::BbAppend => (2, 1),
        Instr::MapPut => (3, 1),
        Instr::ListNew { count, .. } => (*count as usize, 1),
        Instr::MapNew { count, .. } => (2 * *count as usize, 1),
        Instr::MakeClosure { captures, .. } => (*captures as usize, 1),
        Instr::Call(idx) => {
            let argc = module
                .funcs
                .get(*idx as usize)
                .map(|f| f.params.len())
                .unwrap_or(0);
            (argc, 1)
        }
        Instr::CallVirtual { argc, .. } => (*argc as usize + 1, 1),
        Instr::CallValue { argc } => (*argc as usize + 1, 1),
        Instr::Jump(_) => (0, 0),
        Instr::JumpIfFalse(_) | Instr::JumpIfTrue(_) => (1, 0),
        Instr::Return => (1, 0),
    }
}

fn instr_text(instr: &Instr) -> String {
    match instr {
        Instr::ConstUnit => "ConstUnit".to_string(),
        Instr::ConstBool(v) => format!("ConstBool {v}"),
        Instr::ConstInt(v) => format!("ConstInt {v}"),
        Instr::ConstStr(idx) => format!("ConstStr s{idx}"),
        Instr::LoadLocal(slot) => format!("LoadLocal {slot}"),
        Instr::StoreLocal(slot) => format!("StoreLocal {slot}"),
        Instr::Pop => "Pop".to_string(),
        Instr::Add => "Add".to_string(),
        Instr::Sub => "Sub".to_string(),
        Instr::Mul => "Mul".to_string(),
        Instr::Div => "Div".to_string(),
        Instr::Rem => "Rem".to_string(),
        Instr::Neg => "Neg".to_string(),
        Instr::Not => "Not".to_string(),
        Instr::LtInt => "LtInt".to_string(),
        Instr::LeInt => "LeInt".to_string(),
        Instr::GtInt => "GtInt".to_string(),
        Instr::GeInt => "GeInt".to_string(),
        Instr::EqInt => "EqInt".to_string(),
        Instr::NeInt => "NeInt".to_string(),
        Instr::EqBool => "EqBool".to_string(),
        Instr::NeBool => "NeBool".to_string(),
        Instr::EqStr => "EqStr".to_string(),
        Instr::NeStr => "NeStr".to_string(),
        Instr::EqRef => "EqRef".to_string(),
        Instr::NeRef => "NeRef".to_string(),
        Instr::Call(idx) => format!("Call fn{idx}"),
        Instr::CallVirtual { selector, argc } => {
            format!("CallVirtual sel{selector} argc {argc}")
        }
        Instr::CallValue { argc } => format!("CallValue argc {argc}"),
        Instr::MakeClosure { func, captures } => {
            format!("MakeClosure fn{func} captures {captures}")
        }
        Instr::LoadCapture(idx) => format!("LoadCapture {idx}"),
        Instr::New(class) => format!("New class{class}"),
        Instr::LoadField(field) => format!("LoadField {field}"),
        Instr::StoreField(field) => format!("StoreField {field}"),
        Instr::ListNew { ty, count } => format!("ListNew ty{ty} count {count}"),
        Instr::ListLen => "ListLen".to_string(),
        Instr::ListAt => "ListAt".to_string(),
        Instr::ListPush => "ListPush".to_string(),
        Instr::MapNew { ty, count } => format!("MapNew ty{ty} count {count}"),
        Instr::MapLen => "MapLen".to_string(),
        Instr::MapHas => "MapHas".to_string(),
        Instr::MapAt => "MapAt".to_string(),
        Instr::MapPut => "MapPut".to_string(),
        Instr::SbNew => "SbNew".to_string(),
        Instr::SbAppendStr => "SbAppendStr".to_string(),
        Instr::SbAppendInt => "SbAppendInt".to_string(),
        Instr::SbAppendBool => "SbAppendBool".to_string(),
        Instr::SbBuild => "SbBuild".to_string(),
        Instr::BbNew => "BbNew".to_string(),
        Instr::BbAppend => "BbAppend".to_string(),
        Instr::BbLen => "BbLen".to_string(),
        Instr::BbBuild => "BbBuild".to_string(),
        Instr::Freeze => "Freeze".to_string(),
        Instr::Jump(b) => format!("Jump -> b{b}"),
        Instr::JumpIfFalse(b) => format!("JumpIfFalse -> b{b}"),
        Instr::JumpIfTrue(b) => format!("JumpIfTrue -> b{b}"),
        Instr::Return => "Return".to_string(),
    }
}

fn type_text(module: &Module, idx: u32) -> String {
    match &module.types[idx as usize] {
        BcType::Unit => "()".to_string(),
        BcType::Bool => "Bool".to_string(),
        BcType::Int => "Int".to_string(),
        BcType::Str => "String".to_string(),
        BcType::StringBuilder => "StringBuilder".to_string(),
        BcType::ByteBuffer => "ByteBuffer".to_string(),
        BcType::Class(c) => module
            .classes
            .get(*c as usize)
            .map(|cl| cl.name.clone())
            .unwrap_or_else(|| format!("class{c}")),
        BcType::List(e) => format!("[{}]", type_text(module, *e)),
        BcType::Map(k, v) => {
            format!("{{{}: {}}}", type_text(module, *k), type_text(module, *v))
        }
        BcType::Fn(params, ret) => {
            let parts: Vec<String> = params.iter().map(|p| type_text(module, *p)).collect();
            format!("({}) -> {}", parts.join(", "), type_text(module, *ret))
        }
    }
}

/// Render a module as a readable control-flow listing with tables,
/// function signatures, block boundaries, stack effects, and resolved
/// jump targets.
pub fn dump_cfg(module: &Module) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "entry fn{}", module.entry);
    for (sidx, s) in module.strings.iter().enumerate() {
        let _ = writeln!(out, "string s{sidx} = {s:?}");
    }
    for (tidx, _) in module.types.iter().enumerate() {
        let _ = writeln!(out, "type ty{tidx} = {}", type_text(module, tidx as u32));
    }
    for (sidx, s) in module.selectors.iter().enumerate() {
        let _ = writeln!(out, "selector sel{sidx} = {s}");
    }
    for (cidx, class) in module.classes.iter().enumerate() {
        let parent = class
            .parent()
            .map(|p| format!(" < {}", module.classes[p as usize].name))
            .unwrap_or_default();
        let _ = writeln!(out, "class class{cidx} {}{parent}", class.name);
        for (fidx, (name, ty)) in class.fields.iter().enumerate() {
            let _ = writeln!(out, "  field {fidx} {name}: {}", type_text(module, *ty));
        }
        for (sel, func) in &class.methods {
            let _ = writeln!(out, "  method sel{sel} -> fn{func}");
        }
    }
    for (fidx, func) in module.funcs.iter().enumerate() {
        let params: Vec<String> = func.params.iter().map(|p| type_text(module, *p)).collect();
        let _ = writeln!(
            out,
            "\nfn{} {}({}) -> {}",
            fidx,
            func.name,
            params.join(", "),
            type_text(module, func.ret)
        );
        if !func.captures.is_empty() {
            let caps: Vec<String> = func
                .captures
                .iter()
                .map(|c| type_text(module, *c))
                .collect();
            let _ = writeln!(out, "  captures {}", caps.join(", "));
        }
        let _ = writeln!(out, "  locals {}", func.local_count);
        for (bidx, block) in func.blocks.iter().enumerate() {
            let _ = writeln!(out, "  b{bidx}:");
            for instr in block {
                let (pops, pushes) = stack_effect(module, instr);
                let _ = writeln!(
                    out,
                    "    {:<24} ; pop {pops} push {pushes}",
                    instr_text(instr)
                );
            }
        }
    }
    out
}
