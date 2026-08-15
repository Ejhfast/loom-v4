//! Lowering from typed HIR to basic-block bytecode.
//!
//! Each expression leaves exactly one value on the operand stack.
//! Each statement leaves the stack unchanged. Every block ends with a
//! terminator.

use crate::hir::*;
use lm_bytecode::{Func, Instr, Module, PrimTy};
use lm_source::ast::BinOp;
use lm_types::{TypeId, BOOL, INT, STRING, UNIT};
use std::collections::HashMap;
use std::fmt::Write as _;

/// Lower a checked module to decoded bytecode.
pub fn lower_module(hir: &HirModule) -> Module {
    let mut strings = Vec::new();
    let mut string_index: HashMap<String, u32> = HashMap::new();
    let mut funcs = Vec::new();
    for func in &hir.funcs {
        funcs.push(lower_func(func, &mut strings, &mut string_index));
    }
    Module {
        strings,
        funcs,
        entry: hir.entry as u32,
    }
}

fn prim(ty: TypeId) -> PrimTy {
    match ty {
        BOOL => PrimTy::Bool,
        INT => PrimTy::Int,
        STRING => PrimTy::Str,
        // `()` and unreachable `Never` slots use the unit representation.
        _ => PrimTy::Unit,
    }
}

struct Lowerer<'a> {
    blocks: Vec<Vec<Instr>>,
    cur: usize,
    strings: &'a mut Vec<String>,
    string_index: &'a mut HashMap<String, u32>,
    /// Stack of `(continue_target, break_target)` blocks.
    loops: Vec<(u32, u32)>,
}

impl<'a> Lowerer<'a> {
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

    fn intern_string(&mut self, value: &str) -> u32 {
        if let Some(idx) = self.string_index.get(value) {
            return *idx;
        }
        let idx = self.strings.len() as u32;
        self.strings.push(value.to_string());
        self.string_index.insert(value.to_string(), idx);
        idx
    }

    /// Lower a statement. The operand stack is unchanged.
    fn lower_stmt(&mut self, stmt: &HStmt) {
        match stmt {
            HStmt::Assign { slot, value } => {
                self.lower_expr(value);
                self.emit(Instr::StoreLocal(*slot));
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
                let idx = self.intern_string(v);
                self.emit(Instr::ConstStr(idx));
            }
            HExprKind::Local(slot) => self.emit(Instr::LoadLocal(*slot)),
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
            _ => Instr::EqInt,
        },
        BinOp::Ne => match operand_ty {
            BOOL => Instr::NeBool,
            STRING => Instr::NeStr,
            _ => Instr::NeInt,
        },
    }
}

fn lower_func(
    func: &HirFunc,
    strings: &mut Vec<String>,
    string_index: &mut HashMap<String, u32>,
) -> Func {
    let mut lowerer = Lowerer {
        blocks: vec![Vec::new()],
        cur: 0,
        strings,
        string_index,
        loops: Vec::new(),
    };
    let pushed = if func.ret == UNIT {
        let diverged = lowerer.lower_block_stmt(&func.body);
        if !diverged {
            lowerer.emit(Instr::ConstUnit);
        }
        !diverged
    } else {
        lowerer.lower_block_value(&func.body)
    };
    if pushed {
        lowerer.emit(Instr::Return);
    }
    // Close every open block. Only dead continuation blocks stay open
    // here. They receive an explicit return, so the structure is valid.
    for block in &mut lowerer.blocks {
        let terminated = block.last().map(Instr::is_terminator).unwrap_or(false);
        if !terminated {
            block.push(Instr::ConstUnit);
            block.push(Instr::Return);
        }
    }
    Func {
        name: func.name.clone(),
        params: func.params.iter().map(|t| prim(*t)).collect(),
        ret: prim(func.ret),
        local_count: func.locals.len() as u32,
        blocks: lowerer.blocks,
    }
}

/// Count the values an instruction pops and pushes.
fn stack_effect(module: &Module, instr: &Instr) -> (usize, usize) {
    match instr {
        Instr::ConstUnit
        | Instr::ConstBool(_)
        | Instr::ConstInt(_)
        | Instr::ConstStr(_)
        | Instr::LoadLocal(_) => (0, 1),
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
        | Instr::NeStr => (2, 1),
        Instr::Neg | Instr::Not => (1, 1),
        Instr::Call(idx) => {
            let argc = module
                .funcs
                .get(*idx as usize)
                .map(|f| f.params.len())
                .unwrap_or(0);
            (argc, 1)
        }
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
        Instr::Call(idx) => format!("Call fn{idx}"),
        Instr::Jump(b) => format!("Jump -> b{b}"),
        Instr::JumpIfFalse(b) => format!("JumpIfFalse -> b{b}"),
        Instr::JumpIfTrue(b) => format!("JumpIfTrue -> b{b}"),
        Instr::Return => "Return".to_string(),
    }
}

/// Render a module as a readable control-flow listing with function
/// signatures, block boundaries, stack effects, and jump targets.
pub fn dump_cfg(module: &Module) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "entry fn{}", module.entry);
    for (sidx, s) in module.strings.iter().enumerate() {
        let _ = writeln!(out, "string s{sidx} = {s:?}");
    }
    for (fidx, func) in module.funcs.iter().enumerate() {
        let params: Vec<String> = func.params.iter().map(|p| p.to_string()).collect();
        let _ = writeln!(
            out,
            "\nfn{} {}({}) -> {}",
            fidx,
            func.name,
            params.join(", "),
            func.ret
        );
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
