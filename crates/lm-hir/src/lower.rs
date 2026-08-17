//! Lowering from typed HIR to basic-block bytecode.
//!
//! Each expression leaves exactly one value on the operand stack.
//! Each statement leaves the stack unchanged. Every block ends with a
//! terminator. The pass interns strings, types, selectors, and type
//! applications in first-encounter order, so the output is
//! deterministic. It also synthesizes one construction function for
//! each class, and expands `case` patterns and the non-faulting `get`
//! methods into ordinary instructions with scratch locals.

use crate::hir::*;
use lm_bytecode::{BcClass, BcClassKind, BcRow, BcType, Func, Instr, Module, TypeApp, NO_PARENT};
use lm_source::ast::BinOp;
use lm_types::{
    ClassKind, Row, RowElem, Type, TypeId, TypeStore, BOOL, DIGEST, INT, NEVER, STRING, UNIT,
};
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
    apps: Vec<TypeApp>,
    app_index: HashMap<(Vec<u32>, Vec<Vec<BcRow>>), u32>,
    /// The function index of the first synthesized `<new>` function.
    new_base: u32,
    /// Pinned core indices for the `get` expansions.
    core: CoreIds,
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

    /// Convert a checker row into the bytecode row form. Operation
    /// names intern into the string table.
    fn bc_row(&mut self, row: &Row) -> Vec<BcRow> {
        row.iter()
            .map(|elem| match elem {
                RowElem::Op(name) => {
                    let text = self.store.row_name(*name).to_string();
                    BcRow::Op(self.intern_string(&text))
                }
                RowElem::Var(v) => BcRow::Var(*v),
            })
            .collect()
    }

    fn intern_app(&mut self, types: Vec<u32>, rows: Vec<Vec<BcRow>>) -> u32 {
        let key = (types.clone(), rows.clone());
        if let Some(idx) = self.app_index.get(&key) {
            return *idx;
        }
        let idx = self.apps.len() as u32;
        self.apps.push(TypeApp { types, rows });
        self.app_index.insert(key, idx);
        idx
    }

    /// Build a type application from checker types and rows.
    fn app_of(&mut self, targs: &[TypeId], rowargs: &[Row]) -> u32 {
        let types: Vec<u32> = targs.iter().map(|t| self.bc_ty(*t)).collect();
        let rows: Vec<Vec<BcRow>> = rowargs.iter().map(|r| self.bc_row(r)).collect();
        self.intern_app(types, rows)
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
            Type::Digest => self.intern_type(BcType::Digest),
            Type::Class(c) => self.intern_type(BcType::Class(c.0)),
            Type::Inst(c, args) => {
                let args: Vec<u32> = args.iter().map(|a| self.bc_ty(*a)).collect();
                self.intern_type(BcType::Inst(c.0, args))
            }
            Type::List(e) => {
                let e = self.bc_ty(e);
                self.intern_type(BcType::List(e))
            }
            Type::Map(k, v) => {
                let k = self.bc_ty(k);
                let v = self.bc_ty(v);
                self.intern_type(BcType::Map(k, v))
            }
            Type::Tuple(elems) => {
                let elems: Vec<u32> = elems.iter().map(|e| self.bc_ty(*e)).collect();
                self.intern_type(BcType::Tuple(elems))
            }
            Type::Fn(params, muts, ret, row) => {
                let params: Vec<u32> = params.iter().map(|p| self.bc_ty(*p)).collect();
                let ret = self.bc_ty(ret);
                let row = self.bc_row(&row);
                self.intern_type(BcType::Fn(params, muts, ret, row))
            }
            Type::Var(i) => self.intern_type(BcType::Var(i)),
            Type::Fault => self.intern_type(BcType::Fault),
            Type::Request => self.intern_type(BcType::Request),
            Type::PolicyTable => self.intern_type(BcType::PolicyTable),
            Type::EmptyVm => self.intern_type(BcType::EmptyVm),
            Type::SnapshotImage => self.intern_type(BcType::SnapshotImage),
            Type::Snapshot(t) => {
                let t = self.bc_ty(t);
                self.intern_type(BcType::Snapshot(t))
            }
            Type::Vm(t) => {
                let t = self.bc_ty(t);
                self.intern_type(BcType::Vm(t))
            }
            Type::PendingCall(a, r) => {
                let a = self.bc_ty(a);
                let r = self.bc_ty(r);
                self.intern_type(BcType::PendingCall(a, r))
            }
            Type::Handle(m, r) => {
                let m = self.bc_ty(m);
                let r = self.bc_ty(r);
                self.intern_type(BcType::Handle(m, r))
            }
            Type::Op(op, f) => {
                let f = self.bc_ty(f);
                self.intern_type(BcType::Op(op, f))
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
        apps: Vec::new(),
        app_index: HashMap::new(),
        new_base: hir.funcs.len() as u32,
        core: hir.core,
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
            key: class.key.clone(),
            parent: class.parent.unwrap_or(NO_PARENT),
            parent_args: class.parent_args.iter().map(|t| m.bc_ty(*t)).collect(),
            type_params: class.type_params,
            kind: match class.kind {
                ClassKind::Normal => BcClassKind::Normal,
                ClassKind::EnumParent => BcClassKind::Abstract,
                ClassKind::EnumCase => BcClassKind::Case,
            },
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
    // The construction function of class `c` sits at `new_base + c`.
    let new_base = hir.funcs.len() as u32;
    let imports = hir
        .imports
        .iter()
        .map(|i| lm_bytecode::Import {
            module: i.module.clone(),
            name: i.name.clone(),
            kind: i.kind,
            def: match i.def {
                crate::hir::HirImportDef::Class(c) => c,
                crate::hir::HirImportDef::Func(f) => f,
                crate::hir::HirImportDef::Ctor(c) => new_base + c,
            },
            hash: i.hash,
        })
        .collect();
    let exports = hir
        .exports
        .iter()
        .map(|e| lm_bytecode::Export {
            kind: e.kind,
            name: e.name.clone(),
            def: e.def,
            ctor: if e.kind.is_class() {
                new_base + e.def
            } else {
                lm_bytecode::NO_CTOR
            },
        })
        .collect();
    // The generated constructor of a class takes a binding derived
    // from the qualified key of that class. The class structural hash
    // covers no constructor, because the constructor is a function
    // value of its own. The binding is what makes two providers of one
    // class key with two constructors a rejection instead of a merge.
    let mut bindings = hir.bindings.clone();
    for (cidx, class) in hir.classes.iter().enumerate() {
        if class.imported {
            continue;
        }
        bindings.push(lm_bytecode::FuncBinding {
            key: lm_bytecode::ctor_binding_key(&class.key),
            func: new_base + cidx as u32,
            class: cidx as u32,
        });
    }
    Module {
        strings: m.strings,
        types: m.types,
        selectors: m.selectors,
        apps: m.apps,
        imports,
        core_roles: hir.core_roles,
        classes,
        funcs,
        entry: hir.entry as u32,
        exports,
        bindings,
    }
}

struct Lowerer<'a, 'm> {
    m: &'a mut ModLowerer<'m>,
    blocks: Vec<Vec<Instr>>,
    cur: usize,
    /// Stack of `(continue_target, break_target)` blocks.
    loops: Vec<(u32, u32)>,
    /// The declared type of every local slot so far. The checker
    /// types come first; scratch slots append their true types. The
    /// slot count is the vector length.
    local_types: Vec<u32>,
}

impl<'a, 'm> Lowerer<'a, 'm> {
    fn new(m: &'a mut ModLowerer<'m>, local_types: Vec<u32>) -> Lowerer<'a, 'm> {
        Lowerer {
            m,
            blocks: vec![Vec::new()],
            cur: 0,
            loops: Vec::new(),
            local_types,
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

    /// Allocate one scratch local slot with its declared type.
    fn scratch(&mut self, ty: u32) -> u32 {
        let slot = self.local_types.len() as u32;
        self.local_types.push(ty);
        slot
    }

    /// Allocate one scratch slot for a checker type.
    fn scratch_of(&mut self, ty: TypeId) -> u32 {
        let bc = self.m.bc_ty(ty);
        self.scratch(bc)
    }

    /// Emit a structural comparison of the tuples in the locals `a`
    /// and `b`. The expansion leaves one `Bool` on the operand stack.
    /// Unit elements are always equal, so they emit no test.
    fn lower_tuple_eq(&mut self, a: u32, b: u32, ty: TypeId) {
        let elems = match self.m.store.get(ty) {
            Type::Tuple(elems) => elems.clone(),
            _ => unreachable!("tuple equality on a non-tuple type"),
        };
        let tested: Vec<(usize, TypeId)> = elems
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, e)| *e != UNIT)
            .collect();
        if tested.is_empty() {
            // Every element is unit, so the result is a constant and
            // a failure block would have no predecessor.
            self.emit(Instr::ConstBool(true));
            return;
        }
        let false_b = self.new_block();
        let join_b = self.new_block();
        for (i, elem) in &tested {
            if matches!(self.m.store.get(*elem), Type::Tuple(_)) {
                let sa = self.scratch_of(*elem);
                let sb = self.scratch_of(*elem);
                self.emit(Instr::LoadLocal(a));
                self.emit(Instr::TupleGet(*i as u32));
                self.emit(Instr::StoreLocal(sa));
                self.emit(Instr::LoadLocal(b));
                self.emit(Instr::TupleGet(*i as u32));
                self.emit(Instr::StoreLocal(sb));
                self.lower_tuple_eq(sa, sb, *elem);
            } else {
                self.emit(Instr::LoadLocal(a));
                self.emit(Instr::TupleGet(*i as u32));
                self.emit(Instr::LoadLocal(b));
                self.emit(Instr::TupleGet(*i as u32));
                self.emit(binary_instr(BinOp::Eq, *elem));
            }
            self.emit(Instr::JumpIfFalse(false_b));
        }
        self.emit(Instr::ConstBool(true));
        self.emit(Instr::Jump(join_b));
        self.switch_to(false_b);
        self.emit(Instr::ConstBool(false));
        self.emit(Instr::Jump(join_b));
        self.switch_to(join_b);
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

    /// Emit a direct call, generic when arguments are present.
    fn emit_call(&mut self, func: u32, targs: &[TypeId], rowargs: &[Row]) {
        if targs.is_empty() && rowargs.is_empty() {
            self.emit(Instr::Call(func));
        } else {
            let app = self.m.app_of(targs, rowargs);
            self.emit(Instr::CallG { func, app });
        }
    }

    /// Lower an expression. Exactly one value is pushed unless the
    /// expression cannot complete.
    fn lower_expr(&mut self, expr: &HExpr) {
        match &expr.kind {
            HExprKind::Unit => self.emit(Instr::ConstUnit),
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
                if matches!(op, BinOp::Eq | BinOp::Ne)
                    && matches!(self.m.store.get(*operand_ty), Type::Tuple(_))
                {
                    self.lower_expr(left);
                    let a = self.scratch_of(*operand_ty);
                    self.emit(Instr::StoreLocal(a));
                    self.lower_expr(right);
                    let b = self.scratch_of(*operand_ty);
                    self.emit(Instr::StoreLocal(b));
                    self.lower_tuple_eq(a, b, *operand_ty);
                    if matches!(op, BinOp::Ne) {
                        self.emit(Instr::Not);
                    }
                } else {
                    self.lower_expr(left);
                    self.lower_expr(right);
                    self.emit(binary_instr(*op, *operand_ty));
                }
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
            HExprKind::Call {
                func,
                targs,
                rowargs,
                args,
            } => {
                for arg in args {
                    self.lower_expr(arg);
                }
                self.emit_call(*func, targs, rowargs);
            }
            HExprKind::Construct { class, targs, args } => {
                for arg in args {
                    self.lower_expr(arg);
                }
                let target = self.m.new_base + *class;
                self.emit_call(target, targs, &[]);
            }
            HExprKind::MethodCall {
                recv,
                selector,
                generic_owner,
                own_targs,
                own_rowargs,
                args,
            } => {
                self.lower_expr(recv);
                for arg in args {
                    self.lower_expr(arg);
                }
                let sel = self.m.selector(selector);
                let generic_recv = matches!(self.m.store.get(recv.ty), Type::Inst(_, _));
                if generic_recv
                    || *generic_owner
                    || !own_targs.is_empty()
                    || !own_rowargs.is_empty()
                {
                    let app = self.m.app_of(own_targs, own_rowargs);
                    self.emit(Instr::CallVirtualG {
                        selector: sel,
                        argc: args.len() as u32,
                        app,
                    });
                } else {
                    self.emit(Instr::CallVirtual {
                        selector: sel,
                        argc: args.len() as u32,
                    });
                }
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
                let is_op = matches!(self.m.store.get(callee.ty), Type::Op(_, _));
                self.lower_expr(callee);
                for arg in args {
                    self.lower_expr(arg);
                }
                if is_op {
                    // The instruction carries the reply type, so the
                    // world can check the reply value at a boundary.
                    let reply_ty = self.m.bc_ty(expr.ty);
                    self.emit(Instr::PerformValue {
                        argc: args.len() as u32,
                        reply_ty,
                    });
                } else {
                    self.emit(Instr::CallValue {
                        argc: args.len() as u32,
                    });
                }
            }
            HExprKind::Spawn {
                class,
                body,
                ctor_ty,
                body_ty,
                args,
            } => {
                // The verifier reads the closure type out of the
                // module type table, so both function types must be
                // present before the instruction runs.
                self.m.bc_ty(*ctor_ty);
                self.m.bc_ty(*body_ty);
                // The sugar expands into what a user would write: the
                // construction function, the proc body, and the typed
                // argument tuple, then one `Proc.Spawn` perform.
                self.emit(Instr::MakeClosure {
                    func: self.m.new_base + *class,
                    captures: 0,
                });
                self.emit(Instr::MakeClosure {
                    func: *body,
                    captures: 0,
                });
                if args.is_empty() {
                    self.emit(Instr::ConstUnit);
                } else {
                    for arg in args {
                        self.lower_expr(arg);
                    }
                    let tys: Vec<TypeId> = args.iter().map(|a| a.ty).collect();
                    let tuple = self.m.store.find(&Type::Tuple(tys));
                    let ty = match tuple {
                        Some(id) => self.m.bc_ty(id),
                        None => unreachable!("the checker interned the argument tuple type"),
                    };
                    self.emit(Instr::TupleNew {
                        ty,
                        count: args.len() as u32,
                    });
                }
                // The spawn sugar expands into one perform, so the
                // instruction states the handle type it pushes.
                let reply_ty = self.m.bc_ty(expr.ty);
                self.emit(Instr::Perform {
                    op: lm_abi::OP_PROC_SPAWN,
                    argc: 3,
                    reply_ty,
                });
            }
            HExprKind::TupleLit(items) => {
                for item in items {
                    self.lower_expr(item);
                }
                let ty = self.m.bc_ty(expr.ty);
                self.emit(Instr::TupleNew {
                    ty,
                    count: items.len() as u32,
                });
            }
            HExprKind::TupleGet { tuple, index } => {
                self.lower_expr(tuple);
                self.emit(Instr::TupleGet(*index));
            }
            HExprKind::IsType { value, ty } => {
                self.lower_expr(value);
                let ty = self.m.bc_ty(*ty);
                self.emit(Instr::IsType(ty));
            }
            HExprKind::CastType { value, ty } => {
                self.lower_expr(value);
                let ty = self.m.bc_ty(*ty);
                self.emit(Instr::CastType(ty));
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
            HExprKind::Native {
                op: NativeOp::ListGet,
                args,
            } => self.lower_list_get(expr, args),
            HExprKind::Native {
                op: NativeOp::MapGet,
                args,
            } => self.lower_map_get(expr, args),
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
                    NativeOp::Digest => {
                        // The result type must exist in the module
                        // type table before the verifier reads it.
                        self.m.intern_type(BcType::Digest);
                        Instr::Digest
                    }
                    NativeOp::ListGet | NativeOp::MapGet => unreachable!("handled above"),
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
            HExprKind::Case {
                scrut,
                scrut_slot,
                arms,
            } => self.lower_case(scrut, *scrut_slot, arms),
            HExprKind::Perform { op, args } => {
                for arg in args {
                    self.lower_expr(arg);
                }
                // The verifier reconstructs the perform result type
                // through the module type table, so the entry exists.
                // The instruction states the same index, and the world
                // checks the reply value against it at a boundary.
                let reply_ty = self.m.bc_ty(expr.ty);
                self.emit(Instr::Perform {
                    op: *op,
                    argc: args.len() as u32,
                    reply_ty,
                });
            }
            HExprKind::OpConst(op) => {
                self.m.bc_ty(expr.ty);
                self.emit(Instr::OpConst(*op));
            }
            HExprKind::TableEdit {
                action,
                kind,
                slot,
                table,
                mock,
            } => {
                self.lower_expr(table);
                if let Some(mock) = mock {
                    self.lower_expr(mock);
                }
                let action = match action {
                    TableAction::Pass => 0,
                    TableAction::Block => 1,
                    TableAction::Mock => 2,
                    TableAction::Clear => 3,
                };
                let kind = match kind {
                    TargetKind::Exact => 0,
                    TargetKind::Group => 1,
                };
                self.emit(Instr::TableEdit {
                    action,
                    kind,
                    slot: *slot,
                });
            }
            HExprKind::AsCall { request, op } => {
                self.lower_expr(request);
                self.m.bc_ty(expr.ty);
                self.emit(Instr::AsCall(*op));
            }
            HExprKind::CallArgs { call } => {
                self.lower_expr(call);
                self.m.bc_ty(expr.ty);
                self.emit(Instr::CallArgs);
            }
            HExprKind::FaultCodeGet { fault } => {
                self.lower_expr(fault);
                self.emit(Instr::FaultCode);
            }
        }
    }

    /// Lower `list.get(i)` into a bounds test around `ListAt` plus a
    /// call of the pinned core `Option` constructors.
    fn lower_list_get(&mut self, expr: &HExpr, args: &[HExpr]) {
        let elem = self.option_arg(expr.ty);
        let list_slot = self.scratch_of(args[0].ty);
        let idx_slot = self.scratch_of(INT);
        self.lower_expr(&args[0]);
        self.emit(Instr::StoreLocal(list_slot));
        self.lower_expr(&args[1]);
        self.emit(Instr::StoreLocal(idx_slot));
        let none_b = self.new_block();
        let join_b = self.new_block();
        self.emit(Instr::LoadLocal(idx_slot));
        self.emit(Instr::ConstInt(0));
        self.emit(Instr::GeInt);
        self.emit(Instr::JumpIfFalse(none_b));
        self.emit(Instr::LoadLocal(idx_slot));
        self.emit(Instr::LoadLocal(list_slot));
        self.emit(Instr::ListLen);
        self.emit(Instr::LtInt);
        self.emit(Instr::JumpIfFalse(none_b));
        self.emit(Instr::LoadLocal(list_slot));
        self.emit(Instr::LoadLocal(idx_slot));
        self.emit(Instr::ListAt);
        let some_new = self.m.new_base + self.m.core.some_class;
        self.emit_call(some_new, &[elem], &[]);
        self.emit(Instr::Jump(join_b));
        self.switch_to(none_b);
        let none_new = self.m.new_base + self.m.core.none_class;
        self.emit_call(none_new, &[elem], &[]);
        self.emit(Instr::Jump(join_b));
        self.switch_to(join_b);
    }

    /// Lower `map.get(k)` into `MapHas`/`MapAt` plus a call of the
    /// pinned core `Option` constructors.
    fn lower_map_get(&mut self, expr: &HExpr, args: &[HExpr]) {
        let value_ty = self.option_arg(expr.ty);
        let map_slot = self.scratch_of(args[0].ty);
        let key_slot = self.scratch_of(args[1].ty);
        self.lower_expr(&args[0]);
        self.emit(Instr::StoreLocal(map_slot));
        self.lower_expr(&args[1]);
        self.emit(Instr::StoreLocal(key_slot));
        let none_b = self.new_block();
        let join_b = self.new_block();
        self.emit(Instr::LoadLocal(map_slot));
        self.emit(Instr::LoadLocal(key_slot));
        self.emit(Instr::MapHas);
        self.emit(Instr::JumpIfFalse(none_b));
        self.emit(Instr::LoadLocal(map_slot));
        self.emit(Instr::LoadLocal(key_slot));
        self.emit(Instr::MapAt);
        let some_new = self.m.new_base + self.m.core.some_class;
        self.emit_call(some_new, &[value_ty], &[]);
        self.emit(Instr::Jump(join_b));
        self.switch_to(none_b);
        let none_new = self.m.new_base + self.m.core.none_class;
        self.emit_call(none_new, &[value_ty], &[]);
        self.emit(Instr::Jump(join_b));
        self.switch_to(join_b);
    }

    /// The argument of a core `Option[T]` result type.
    fn option_arg(&self, ty: TypeId) -> TypeId {
        match self.m.store.get(ty) {
            Type::Inst(_, args) => args[0],
            _ => unreachable!("get results are Option instances"),
        }
    }

    /// Lower one `case` expression. The scrutinee is stored first;
    /// each arm tests the pattern, binds, runs its body, and jumps to
    /// the join with one value. The checker proved exhaustiveness, so
    /// the last arm destructures without tests.
    fn lower_case(&mut self, scrut: &HExpr, scrut_slot: u32, arms: &[HArm]) {
        self.lower_expr(scrut);
        self.emit(Instr::StoreLocal(scrut_slot));
        let join_b = self.new_block();
        // The runtime backstop behind the static exhaustiveness
        // proof: the last arm keeps its tests, and a value no arm
        // accepts reaches an `Unreachable` fault instead of falling
        // through silently.
        let unreach_b = self.new_block();
        let last = arms.len() - 1;
        for (aidx, arm) in arms.iter().enumerate() {
            if aidx == last {
                self.lower_pattern(&arm.pattern, scrut_slot, Some(unreach_b));
                let pushed = self.lower_block_value(&arm.body);
                if pushed {
                    self.emit(Instr::Jump(join_b));
                }
            } else {
                let next_b = self.new_block();
                self.lower_pattern(&arm.pattern, scrut_slot, Some(next_b));
                let pushed = self.lower_block_value(&arm.body);
                if pushed {
                    self.emit(Instr::Jump(join_b));
                }
                self.switch_to(next_b);
            }
        }
        self.switch_to(unreach_b);
        self.emit(Instr::Unreachable);
        self.switch_to(join_b);
    }

    /// Lower one pattern over the value in `src`. With `fail` the
    /// tests jump there on a mismatch; without it the pattern only
    /// destructures, because the checker proved it must match.
    fn lower_pattern(&mut self, pattern: &HPattern, src: u32, fail: Option<u32>) {
        match pattern {
            HPattern::Wildcard => {}
            HPattern::Bind(slot) => {
                self.emit(Instr::LoadLocal(src));
                self.emit(Instr::StoreLocal(*slot));
            }
            HPattern::Int(v) => {
                if let Some(fail) = fail {
                    self.emit(Instr::LoadLocal(src));
                    self.emit(Instr::ConstInt(*v));
                    self.emit(Instr::EqInt);
                    self.emit(Instr::JumpIfFalse(fail));
                }
            }
            HPattern::Bool(v) => {
                if let Some(fail) = fail {
                    self.emit(Instr::LoadLocal(src));
                    if *v {
                        self.emit(Instr::JumpIfFalse(fail));
                    } else {
                        self.emit(Instr::JumpIfTrue(fail));
                    }
                }
            }
            HPattern::Str(v) => {
                if let Some(fail) = fail {
                    self.emit(Instr::LoadLocal(src));
                    let idx = self.m.intern_string(v);
                    self.emit(Instr::ConstStr(idx));
                    self.emit(Instr::EqStr);
                    self.emit(Instr::JumpIfFalse(fail));
                }
            }
            HPattern::Ctor {
                ty,
                args,
                field_tys,
                ..
            } => {
                let bc = self.m.bc_ty(*ty);
                if let Some(fail) = fail {
                    self.emit(Instr::LoadLocal(src));
                    self.emit(Instr::IsType(bc));
                    self.emit(Instr::JumpIfFalse(fail));
                }
                let needs_fields = args.iter().any(|a| !matches!(a, HPattern::Wildcard));
                if needs_fields {
                    let cast_slot = self.scratch(bc);
                    self.emit(Instr::LoadLocal(src));
                    self.emit(Instr::CastType(bc));
                    self.emit(Instr::StoreLocal(cast_slot));
                    for (fidx, sub) in args.iter().enumerate() {
                        if matches!(sub, HPattern::Wildcard) {
                            continue;
                        }
                        let field_slot = self.scratch_of(field_tys[fidx]);
                        self.emit(Instr::LoadLocal(cast_slot));
                        self.emit(Instr::LoadField(fidx as u32));
                        self.emit(Instr::StoreLocal(field_slot));
                        self.lower_pattern(sub, field_slot, fail);
                    }
                }
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

/// Shift every local slot reference in a default expression by
/// `base`, and record one past the highest shifted slot in `max`.
fn shift_locals_expr(expr: &HExpr, base: u32, max: &mut u32) -> HExpr {
    let mut out = expr.clone();
    shift_expr_in_place(&mut out, base, max);
    out
}

fn shift_slot(slot: &mut u32, base: u32, max: &mut u32) {
    *max = (*max).max(*slot + 1);
    *slot += base;
}

fn shift_expr_in_place(expr: &mut HExpr, base: u32, max: &mut u32) {
    match &mut expr.kind {
        HExprKind::Local(slot) => shift_slot(slot, base, max),
        HExprKind::Unit
        | HExprKind::Int(_)
        | HExprKind::Str(_)
        | HExprKind::Bool(_)
        | HExprKind::Capture(_) => {}
        HExprKind::Not(inner) | HExprKind::Neg(inner) => shift_expr_in_place(inner, base, max),
        HExprKind::Binary { left, right, .. }
        | HExprKind::And(left, right)
        | HExprKind::Or(left, right) => {
            shift_expr_in_place(left, base, max);
            shift_expr_in_place(right, base, max);
        }
        HExprKind::Call { args, .. } | HExprKind::Construct { args, .. } => {
            for a in args {
                shift_expr_in_place(a, base, max);
            }
        }
        HExprKind::MethodCall { recv, args, .. } => {
            shift_expr_in_place(recv, base, max);
            for a in args {
                shift_expr_in_place(a, base, max);
            }
        }
        HExprKind::FieldGet { recv, .. } => shift_expr_in_place(recv, base, max),
        HExprKind::MakeClosure { captures, .. } => {
            for c in captures {
                shift_expr_in_place(c, base, max);
            }
        }
        HExprKind::Spawn { args, .. } => {
            for a in args {
                shift_expr_in_place(a, base, max);
            }
        }
        HExprKind::CallValue { callee, args } => {
            shift_expr_in_place(callee, base, max);
            for a in args {
                shift_expr_in_place(a, base, max);
            }
        }
        HExprKind::TupleLit(items) | HExprKind::ListLit(items) => {
            for i in items {
                shift_expr_in_place(i, base, max);
            }
        }
        HExprKind::TupleGet { tuple, .. } => shift_expr_in_place(tuple, base, max),
        HExprKind::IsType { value, .. } | HExprKind::CastType { value, .. } => {
            shift_expr_in_place(value, base, max)
        }
        HExprKind::MapLit(entries) => {
            for (k, v) in entries {
                shift_expr_in_place(k, base, max);
                shift_expr_in_place(v, base, max);
            }
        }
        HExprKind::Native { args, .. } => {
            for a in args {
                shift_expr_in_place(a, base, max);
            }
        }
        HExprKind::Interp(parts) => {
            for part in parts {
                if let HInterpPart::Expr(e) = part {
                    shift_expr_in_place(e, base, max);
                }
            }
        }
        HExprKind::If { arms, else_body } => {
            for (cond, body) in arms {
                shift_expr_in_place(cond, base, max);
                for s in body {
                    shift_stmt_in_place(s, base, max);
                }
            }
            if let Some(body) = else_body {
                for s in body {
                    shift_stmt_in_place(s, base, max);
                }
            }
        }
        HExprKind::Case {
            scrut,
            scrut_slot,
            arms,
        } => {
            shift_expr_in_place(scrut, base, max);
            shift_slot(scrut_slot, base, max);
            for arm in arms {
                shift_pattern_in_place(&mut arm.pattern, base, max);
                for s in &mut arm.body {
                    shift_stmt_in_place(s, base, max);
                }
            }
        }
        HExprKind::Perform { args, .. } => {
            for a in args {
                shift_expr_in_place(a, base, max);
            }
        }
        HExprKind::OpConst(_) => {}
        HExprKind::TableEdit { table, mock, .. } => {
            shift_expr_in_place(table, base, max);
            if let Some(mock) = mock {
                shift_expr_in_place(mock, base, max);
            }
        }
        HExprKind::AsCall { request, .. } => shift_expr_in_place(request, base, max),
        HExprKind::CallArgs { call } => shift_expr_in_place(call, base, max),
        HExprKind::FaultCodeGet { fault } => shift_expr_in_place(fault, base, max),
    }
}

fn shift_pattern_in_place(pattern: &mut HPattern, base: u32, max: &mut u32) {
    match pattern {
        HPattern::Bind(slot) => shift_slot(slot, base, max),
        HPattern::Ctor { args, .. } => {
            for a in args {
                shift_pattern_in_place(a, base, max);
            }
        }
        _ => {}
    }
}

fn shift_stmt_in_place(stmt: &mut HStmt, base: u32, max: &mut u32) {
    match stmt {
        HStmt::Assign { slot, value } => {
            shift_slot(slot, base, max);
            shift_expr_in_place(value, base, max);
        }
        HStmt::AssignField { recv, value, .. } => {
            shift_expr_in_place(recv, base, max);
            shift_expr_in_place(value, base, max);
        }
        HStmt::While { cond, body } => {
            shift_expr_in_place(cond, base, max);
            for s in body {
                shift_stmt_in_place(s, base, max);
            }
        }
        HStmt::Return { value } => {
            if let Some(v) = value {
                shift_expr_in_place(v, base, max);
            }
        }
        HStmt::Break | HStmt::Continue => {}
        HStmt::Expr(e) => shift_expr_in_place(e, base, max),
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
            DIGEST => Instr::EqDigest,
            INT | NEVER | UNIT => Instr::EqInt,
            _ => Instr::EqRef,
        },
        BinOp::Ne => match operand_ty {
            BOOL => Instr::NeBool,
            STRING => Instr::NeStr,
            DIGEST => Instr::NeDigest,
            INT | NEVER | UNIT => Instr::NeInt,
            _ => Instr::NeRef,
        },
    }
}

fn lower_func(m: &mut ModLowerer<'_>, func: &HirFunc) -> Func {
    let params: Vec<u32> = func.params.iter().map(|t| m.bc_ty(*t)).collect();
    let ret = m.bc_ty(func.ret);
    let row = m.bc_row(&func.row);
    if func.imported {
        // An imported function is a declaration: the signature only.
        // The linker replaces it with the provider definition.
        return Func {
            name: func.name.clone(),
            type_params: func.type_params,
            effect_params: func.effect_params,
            params: params.clone(),
            param_muts: func.param_muts.clone(),
            ret,
            row,
            captures: vec![],
            local_types: params,
            blocks: vec![],
        };
    }
    let captures: Vec<u32> = func.captures.iter().map(|t| m.bc_ty(*t)).collect();
    // The declared checker types of every local slot seed the table;
    // scratch slots append their true types during lowering.
    let base_types: Vec<u32> = func.locals.iter().map(|t| m.bc_ty(*t)).collect();
    let mut lowerer = Lowerer::new(m, base_types);
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
    let local_types = lowerer.local_types.clone();
    let blocks = lowerer.finish(pushed);
    Func {
        name: func.name.clone(),
        type_params: func.type_params,
        effect_params: func.effect_params,
        params,
        param_muts: func.param_muts.clone(),
        ret,
        row,
        captures,
        local_types,
        blocks,
    }
}

/// Synthesize the `<new>` construction function for one class:
/// allocate, evaluate defaults, run `init` or store the case fields,
/// and return the instance.
fn lower_new_func(m: &mut ModLowerer<'_>, class: &HirClass, cidx: u32) -> Func {
    if class.imported {
        // An imported class declares its construction function and
        // carries no body. The provider evaluates its own defaults.
        let params: Vec<u32> = class.ctor_params.iter().map(|t| m.bc_ty(*t)).collect();
        let self_bc = if class.type_params == 0 {
            m.intern_type(BcType::Class(cidx))
        } else {
            let var_tys: Vec<u32> = (0..class.type_params)
                .map(|i| m.intern_type(BcType::Var(i)))
                .collect();
            m.intern_type(BcType::Inst(cidx, var_tys))
        };
        let row = m.bc_row(&class.ctor_row);
        return Func {
            name: format!("<new {}>", class.name),
            type_params: class.type_params,
            effect_params: 0,
            params: params.clone(),
            param_muts: class.ctor_param_muts.clone(),
            ret: self_bc,
            row,
            captures: vec![],
            local_types: params,
            blocks: vec![],
        };
    }
    if class.kind == ClassKind::EnumParent {
        // An abstract enum parent is never constructed. Its `<new>`
        // slot only keeps the index arithmetic dense.
        return Func {
            name: format!("<new {}>", class.name),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: m.intern_type(BcType::Unit),
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![Instr::ConstUnit, Instr::Return]],
        };
    }
    let params: Vec<u32> = class.ctor_params.iter().map(|t| m.bc_ty(*t)).collect();
    let type_params = class.type_params;
    let vars: Vec<TypeId> = Vec::new();
    let _ = vars;
    let (self_bc, app) = if type_params == 0 {
        (m.intern_type(BcType::Class(cidx)), None)
    } else {
        let var_tys: Vec<u32> = (0..type_params)
            .map(|i| m.intern_type(BcType::Var(i)))
            .collect();
        let inst = m.intern_type(BcType::Inst(cidx, var_tys.clone()));
        let app = m.intern_app(var_tys, vec![]);
        (inst, Some(app))
    };
    let row = m.bc_row(&class.ctor_row);
    let self_slot = params.len() as u32;
    // The slot table starts with the constructor parameters and the
    // `self` scratch slot.
    let mut base_types = params.clone();
    base_types.push(self_bc);
    let mut lowerer = Lowerer::new(m, base_types);
    match app {
        None => lowerer.emit(Instr::New(cidx)),
        Some(app) => lowerer.emit(Instr::NewG { class: cidx, app }),
    }
    lowerer.emit(Instr::StoreLocal(self_slot));
    if class.ctor_kind == CtorKind::CaseFields {
        for fidx in 0..class.ctor_params.len() {
            lowerer.emit(Instr::LoadLocal(self_slot));
            lowerer.emit(Instr::LoadLocal(fidx as u32));
            lowerer.emit(Instr::StoreField(fidx as u32));
        }
    } else {
        for (fidx, default) in class.defaults.iter().enumerate() {
            if let Some(expr) = default {
                // A default was checked in its own local space. Move
                // its temporary slots into fresh scratch slots of the
                // `<new>` function, with their checker-declared types.
                let base = lowerer.local_types.len() as u32;
                let mut max_slot = 0;
                let shifted = shift_locals_expr(expr, base, &mut max_slot);
                // The shifted temporaries occupy `base .. base + max_slot`,
                // because `max_slot` counts in the pre-shift space.
                let default_types = &class.default_locals[fidx];
                for ty in default_types.iter().take(max_slot as usize) {
                    lowerer.scratch_of(*ty);
                }
                lowerer.emit(Instr::LoadLocal(self_slot));
                lowerer.lower_expr(&shifted);
                lowerer.emit(Instr::StoreField(fidx as u32));
            }
        }
        if let Some(init) = class.init {
            lowerer.emit(Instr::LoadLocal(self_slot));
            for i in 0..self_slot {
                lowerer.emit(Instr::LoadLocal(i));
            }
            match app {
                None => lowerer.emit(Instr::Call(init)),
                Some(app) => lowerer.emit(Instr::CallG { func: init, app }),
            }
            lowerer.emit(Instr::Pop);
        }
    }
    lowerer.emit(Instr::LoadLocal(self_slot));
    let local_types = lowerer.local_types.clone();
    let blocks = lowerer.finish(true);
    Func {
        name: format!("<new {}>", class.name),
        type_params,
        effect_params: 0,
        params,
        param_muts: class.ctor_param_muts.clone(),
        ret: self_bc,
        row,
        captures: vec![],
        local_types,
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
        | Instr::NewG { .. }
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
        | Instr::TupleGet(_)
        | Instr::IsType(_)
        | Instr::CastType(_)
        | Instr::ListLen
        | Instr::MapLen
        | Instr::SbBuild
        | Instr::BbLen
        | Instr::BbBuild
        | Instr::Freeze
        | Instr::Digest => (1, 1),
        Instr::EqDigest | Instr::NeDigest => (2, 1),
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
        Instr::ListNew { count, .. } | Instr::TupleNew { count, .. } => (*count as usize, 1),
        Instr::MapNew { count, .. } => (2 * *count as usize, 1),
        Instr::MakeClosure { captures, .. } => (*captures as usize, 1),
        Instr::Call(idx) | Instr::CallG { func: idx, .. } => {
            let argc = module
                .funcs
                .get(*idx as usize)
                .map(|f| f.params.len())
                .unwrap_or(0);
            (argc, 1)
        }
        Instr::CallVirtual { argc, .. } | Instr::CallVirtualG { argc, .. } => {
            (*argc as usize + 1, 1)
        }
        Instr::CallValue { argc } => (*argc as usize + 1, 1),
        Instr::Jump(_) => (0, 0),
        Instr::JumpIfFalse(_) | Instr::JumpIfTrue(_) => (1, 0),
        Instr::Return => (1, 0),
        Instr::Perform { argc, .. } => (*argc as usize, 1),
        Instr::PerformValue { argc, .. } => (*argc as usize + 1, 1),
        Instr::OpConst(_) => (0, 1),
        Instr::TableEdit { action, .. } => {
            // A mock edit also pops the handler closure.
            if *action == 2 {
                (2, 1)
            } else {
                (1, 1)
            }
        }
        Instr::AsCall(_) => (1, 1),
        Instr::CallArgs => (1, 1),
        Instr::FaultCode => (1, 1),
        Instr::Unreachable => (0, 0),
    }
}

/// The display name of one operation slot, safe for out-of-range
/// slots in hand-built modules.
fn op_text(slot: u32) -> String {
    if slot < lm_abi::OP_COUNT {
        lm_abi::op_name(slot)
    } else {
        format!("op{slot}")
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
        Instr::CallG { func, app } => format!("CallG fn{func} app{app}"),
        Instr::CallVirtual { selector, argc } => {
            format!("CallVirtual sel{selector} argc {argc}")
        }
        Instr::CallVirtualG {
            selector,
            argc,
            app,
        } => format!("CallVirtualG sel{selector} argc {argc} app{app}"),
        Instr::CallValue { argc } => format!("CallValue argc {argc}"),
        Instr::MakeClosure { func, captures } => {
            format!("MakeClosure fn{func} captures {captures}")
        }
        Instr::LoadCapture(idx) => format!("LoadCapture {idx}"),
        Instr::New(class) => format!("New class{class}"),
        Instr::NewG { class, app } => format!("NewG class{class} app{app}"),
        Instr::LoadField(field) => format!("LoadField {field}"),
        Instr::StoreField(field) => format!("StoreField {field}"),
        Instr::TupleNew { ty, count } => format!("TupleNew ty{ty} count {count}"),
        Instr::TupleGet(index) => format!("TupleGet {index}"),
        Instr::IsType(ty) => format!("IsType ty{ty}"),
        Instr::CastType(ty) => format!("CastType ty{ty}"),
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
        Instr::Digest => "Digest".to_string(),
        Instr::EqDigest => "EqDigest".to_string(),
        Instr::NeDigest => "NeDigest".to_string(),
        Instr::Jump(b) => format!("Jump -> b{b}"),
        Instr::JumpIfFalse(b) => format!("JumpIfFalse -> b{b}"),
        Instr::JumpIfTrue(b) => format!("JumpIfTrue -> b{b}"),
        Instr::Return => "Return".to_string(),
        Instr::Perform { op, argc, .. } => {
            format!("Perform {} argc {argc}", op_text(*op))
        }
        Instr::PerformValue { argc, .. } => format!("PerformValue argc {argc}"),
        Instr::OpConst(op) => format!("OpConst {}", op_text(*op)),
        Instr::TableEdit { action, kind, slot } => {
            let action_text = match action {
                0 => "pass",
                1 => "block",
                2 => "mock",
                _ => "clear",
            };
            let target = match kind {
                0 => op_text(*slot),
                _ => lm_abi::GROUPS
                    .get(*slot as usize)
                    .map(|g| g.to_string())
                    .unwrap_or_else(|| format!("group{slot}")),
            };
            format!("TableEdit {action_text} {target}")
        }
        Instr::AsCall(op) => format!("AsCall {}", op_text(*op)),
        Instr::CallArgs => "CallArgs".to_string(),
        Instr::FaultCode => "FaultCode".to_string(),
        Instr::Unreachable => "Unreachable".to_string(),
    }
}

fn row_text(module: &Module, row: &[BcRow]) -> String {
    let parts: Vec<String> = row
        .iter()
        .map(|elem| match elem {
            BcRow::Op(idx) => module
                .strings
                .get(*idx as usize)
                .cloned()
                .unwrap_or_else(|| format!("s{idx}")),
            BcRow::Var(v) => format!("e{v}"),
        })
        .collect();
    parts.join(", ")
}

fn type_text(module: &Module, idx: u32) -> String {
    match &module.types[idx as usize] {
        BcType::Unit => "()".to_string(),
        BcType::Bool => "Bool".to_string(),
        BcType::Int => "Int".to_string(),
        BcType::Str => "String".to_string(),
        BcType::StringBuilder => "StringBuilder".to_string(),
        BcType::ByteBuffer => "ByteBuffer".to_string(),
        BcType::Digest => "Digest".to_string(),
        BcType::Class(c) => module
            .classes
            .get(*c as usize)
            .map(|cl| cl.name.clone())
            .unwrap_or_else(|| format!("class{c}")),
        BcType::Inst(c, args) => {
            let name = module
                .classes
                .get(*c as usize)
                .map(|cl| cl.name.clone())
                .unwrap_or_else(|| format!("class{c}"));
            let parts: Vec<String> = args.iter().map(|a| type_text(module, *a)).collect();
            format!("{}[{}]", name, parts.join(", "))
        }
        BcType::List(e) => format!("[{}]", type_text(module, *e)),
        BcType::Map(k, v) => {
            format!("{{{}: {}}}", type_text(module, *k), type_text(module, *v))
        }
        BcType::Tuple(elems) => {
            let parts: Vec<String> = elems.iter().map(|e| type_text(module, *e)).collect();
            if parts.len() == 1 {
                format!("({},)", parts[0])
            } else {
                format!("({})", parts.join(", "))
            }
        }
        BcType::Fn(params, muts, ret, row) => {
            let parts: Vec<String> = params
                .iter()
                .zip(muts.iter())
                .map(|(p, m)| {
                    if *m {
                        format!("mut {}", type_text(module, *p))
                    } else {
                        type_text(module, *p)
                    }
                })
                .collect();
            let mut out = format!("({}) -> {}", parts.join(", "), type_text(module, *ret));
            if !row.is_empty() {
                out.push_str(" with ");
                out.push_str(&row_text(module, row));
            }
            out
        }
        BcType::Var(i) => format!("${i}"),
        BcType::Fault => "Fault".to_string(),
        BcType::Request => "Request".to_string(),
        BcType::PolicyTable => "PolicyTable".to_string(),
        BcType::EmptyVm => "EmptyVm".to_string(),
        BcType::SnapshotImage => "SnapshotImage".to_string(),
        BcType::Vm(t) => format!("Vm[{}]", type_text(module, *t)),
        BcType::Snapshot(t) => format!("Snapshot[{}]", type_text(module, *t)),
        BcType::PendingCall(a, r) => format!(
            "PendingCall[{}, {}]",
            type_text(module, *a),
            type_text(module, *r)
        ),
        BcType::Handle(m, r) => format!(
            "Handle[{}, {}]",
            type_text(module, *m),
            type_text(module, *r)
        ),
        BcType::Op(op, f) => format!("Op[{}, {}]", op_text(*op), type_text(module, *f)),
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
    for (aidx, app) in module.apps.iter().enumerate() {
        let types: Vec<String> = app.types.iter().map(|t| type_text(module, *t)).collect();
        let rows: Vec<String> = app
            .rows
            .iter()
            .map(|r| format!("{{{}}}", row_text(module, r)))
            .collect();
        let _ = writeln!(
            out,
            "app app{aidx} = [{}] rows [{}]",
            types.join(", "),
            rows.join(", ")
        );
    }
    for (cidx, class) in module.classes.iter().enumerate() {
        // A generic parent carries its type arguments, so the listing
        // shows the instantiation the class table records.
        let parent = class
            .parent()
            .map(|p| {
                let args = if class.parent_args.is_empty() {
                    String::new()
                } else {
                    let parts: Vec<String> = class
                        .parent_args
                        .iter()
                        .map(|t| type_text(module, *t))
                        .collect();
                    format!("[{}]", parts.join(", "))
                };
                format!(" < {}{args}", module.classes[p as usize].name)
            })
            .unwrap_or_default();
        let kind = match class.kind {
            BcClassKind::Normal => "",
            BcClassKind::Abstract => " abstract",
            BcClassKind::Case => " case",
        };
        let generics = if class.type_params > 0 {
            format!(" params {}", class.type_params)
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "class class{cidx} {}{kind}{generics}{parent}",
            class.name
        );
        for (fidx, (name, ty)) in class.fields.iter().enumerate() {
            let _ = writeln!(out, "  field {fidx} {name}: {}", type_text(module, *ty));
        }
        for (sel, func) in &class.methods {
            let _ = writeln!(out, "  method sel{sel} -> fn{func}");
        }
    }
    // One function-to-names index for the whole dump.
    let mut binding_index: std::collections::HashMap<u32, Vec<&str>> =
        std::collections::HashMap::new();
    for binding in &module.bindings {
        binding_index
            .entry(binding.func)
            .or_default()
            .push(binding.key.as_str());
    }
    for (fidx, func) in module.funcs.iter().enumerate() {
        let params: Vec<String> = func.params.iter().map(|p| type_text(module, *p)).collect();
        let generics = if func.type_params > 0 || func.effect_params > 0 {
            format!(" generics {}+{}", func.type_params, func.effect_params)
        } else {
            String::new()
        };
        let row = if func.row.is_empty() {
            String::new()
        } else {
            format!(" with {}", row_text(module, &func.row))
        };
        let _ = writeln!(
            out,
            "\nfn{} {}({}) -> {}{}{}",
            fidx,
            func.name,
            params.join(", "),
            type_text(module, func.ret),
            row,
            generics
        );
        // Every name that points at this function value. Two modules
        // with equal bodies share one code object and keep two names,
        // so the listing must print them all. The index is built once,
        // because a scan per function makes the dump quadratic.
        if let Some(keys) = binding_index.get(&(fidx as u32)) {
            for key in keys {
                let _ = writeln!(out, "  binding {key}");
            }
        }
        if !func.captures.is_empty() {
            let caps: Vec<String> = func
                .captures
                .iter()
                .map(|c| type_text(module, *c))
                .collect();
            let _ = writeln!(out, "  captures {}", caps.join(", "));
        }
        let _ = writeln!(out, "  locals {}", func.local_count());
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
