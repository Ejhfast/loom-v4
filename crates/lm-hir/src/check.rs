//! Bidirectional type checking from AST to typed HIR.
//!
//! The checker synthesizes a type for each expression, or checks the
//! expression against an expected type. It resolves names to local
//! slots and function indices. It stops at the first error and returns
//! one precise diagnostic.

use crate::hir::*;
use lm_source::ast::{self, BinOp, ExprKind, StmtKind, TypeExprKind};
use lm_source::diag::Diagnostic;
use lm_source::span::Span;
use lm_types::{TypeId, TypeStore, BOOL, INT, NEVER, STRING, UNIT};
use std::collections::HashMap;

/// Check a parsed module and produce typed HIR.
pub fn check_module(module: &ast::Module) -> Result<HirModule, Diagnostic> {
    let mut store = TypeStore::new();
    // Predeclare all function signatures. This permits mutual recursion.
    let mut func_index: HashMap<String, u32> = HashMap::new();
    let mut sigs: Vec<(Vec<TypeId>, TypeId)> = Vec::new();
    for (idx, func) in module.funcs.iter().enumerate() {
        if func_index.contains_key(&func.name) {
            return Err(Diagnostic::new(
                "E1010",
                format!("the name `{}` has more than one definition", func.name),
                func.name_span,
            ));
        }
        let mut params = Vec::new();
        let mut seen = HashMap::new();
        for param in &func.params {
            if seen.insert(param.name.clone(), ()).is_some() {
                return Err(Diagnostic::new(
                    "E1014",
                    format!("duplicate parameter name `{}`", param.name),
                    param.span,
                ));
            }
            params.push(resolve_type(&store, &param.ty)?);
        }
        let ret = match &func.ret {
            Some(ty) => resolve_type(&store, ty)?,
            None => UNIT,
        };
        func_index.insert(func.name.clone(), idx as u32);
        sigs.push((params, ret));
    }

    let mut funcs = Vec::new();
    for (idx, func) in module.funcs.iter().enumerate() {
        let (params, ret) = sigs[idx].clone();
        let mut ctx = FnChecker {
            store: &mut store,
            func_index: &func_index,
            sigs: &sigs,
            locals: params.clone(),
            scopes: vec![HashMap::new()],
            loop_depth: 0,
            ret: Some(ret),
        };
        for (slot, param) in func.params.iter().enumerate() {
            ctx.scopes[0].insert(param.name.clone(), slot as u32);
        }
        let mode = if ret == UNIT {
            BlockMode::Stmt
        } else {
            BlockMode::Value(ret)
        };
        let (body, _) = ctx.check_block(&func.body, mode, func.span)?;
        funcs.push(HirFunc {
            name: func.name.clone(),
            params,
            ret,
            locals: ctx.locals,
            body,
        });
    }

    // The entry statements form one synthesized function.
    let entry_span = module
        .entry
        .last()
        .map(|s| s.span)
        .unwrap_or(Span::new(0, 0));
    let mut ctx = FnChecker {
        store: &mut store,
        func_index: &func_index,
        sigs: &sigs,
        locals: Vec::new(),
        scopes: vec![HashMap::new()],
        loop_depth: 0,
        ret: None,
    };
    let (body, entry_ty) = ctx.check_block(&module.entry, BlockMode::Synth, entry_span)?;
    let entry_ty = if entry_ty == NEVER { UNIT } else { entry_ty };
    let locals = ctx.locals;
    funcs.push(HirFunc {
        name: "<entry>".to_string(),
        params: Vec::new(),
        ret: entry_ty,
        locals,
        body,
    });
    let entry = funcs.len() - 1;
    Ok(HirModule {
        store,
        funcs,
        entry,
    })
}

fn resolve_type(store: &TypeStore, ty: &ast::TypeExpr) -> Result<TypeId, Diagnostic> {
    match &ty.kind {
        TypeExprKind::Unit => Ok(UNIT),
        TypeExprKind::Name(name) => store.by_name(name).ok_or_else(|| {
            Diagnostic::new("E1013", format!("unknown type name `{name}`"), ty.span)
        }),
    }
}

/// How a block result is used.
#[derive(Clone, Copy, PartialEq)]
enum BlockMode {
    /// The block value is discarded. The block type is `()`.
    Stmt,
    /// The block must produce a value of the expected type.
    Value(TypeId),
    /// Synthesize the block type from its final expression.
    Synth,
}

struct FnChecker<'a> {
    store: &'a mut TypeStore,
    func_index: &'a HashMap<String, u32>,
    sigs: &'a [(Vec<TypeId>, TypeId)],
    locals: Vec<TypeId>,
    scopes: Vec<HashMap<String, u32>>,
    loop_depth: u32,
    /// The declared result type. `None` marks the module entry.
    ret: Option<TypeId>,
}

impl<'a> FnChecker<'a> {
    fn lookup(&self, name: &str) -> Option<u32> {
        for scope in self.scopes.iter().rev() {
            if let Some(slot) = scope.get(name) {
                return Some(*slot);
            }
        }
        None
    }

    fn mismatch(&self, expected: TypeId, found: TypeId, span: Span) -> Diagnostic {
        Diagnostic::new(
            "E1004",
            format!(
                "expected {}, found {}",
                self.store.display(expected),
                self.store.display(found)
            ),
            span,
        )
    }

    /// Check a statement list. Return the statements and the block type.
    fn check_block(
        &mut self,
        stmts: &[ast::Stmt],
        mode: BlockMode,
        block_span: Span,
    ) -> Result<(Vec<HStmt>, TypeId), Diagnostic> {
        let mut out = Vec::new();
        for (idx, stmt) in stmts.iter().enumerate() {
            if let Some(prev) = out.last() {
                let prev: &HStmt = prev;
                if prev.diverges() {
                    return Err(Diagnostic::new(
                        "E1021",
                        "this statement is unreachable",
                        stmt.span,
                    ));
                }
            }
            let is_last = idx + 1 == stmts.len();
            if is_last {
                if let BlockMode::Value(expected) = mode {
                    let checked = self.check_tail(stmt, expected)?;
                    out.push(checked);
                    return Ok((out, expected));
                }
            }
            out.push(self.check_stmt(stmt)?);
        }
        let block_ty = match mode {
            BlockMode::Stmt => UNIT,
            BlockMode::Value(expected) => {
                // The list is empty, so the block value is `()`.
                if expected != UNIT {
                    return Err(self.mismatch(expected, UNIT, block_span));
                }
                UNIT
            }
            BlockMode::Synth => match out.last() {
                Some(HStmt::Expr(e)) => e.ty,
                Some(stmt) if stmt.diverges() => NEVER,
                _ => UNIT,
            },
        };
        Ok((out, block_ty))
    }

    /// Check the final statement of a value block against an expected type.
    fn check_tail(&mut self, stmt: &ast::Stmt, expected: TypeId) -> Result<HStmt, Diagnostic> {
        match &stmt.kind {
            StmtKind::Expr(expr) => {
                let value = self.check_expr(expr, expected)?;
                Ok(HStmt::Expr(value))
            }
            // A diverging tail satisfies any expected type.
            StmtKind::Return { .. } | StmtKind::Break | StmtKind::Continue => self.check_stmt(stmt),
            _ => Err(self.mismatch(expected, UNIT, stmt.span)),
        }
    }

    fn check_stmt(&mut self, stmt: &ast::Stmt) -> Result<HStmt, Diagnostic> {
        match &stmt.kind {
            StmtKind::Assign {
                name,
                name_span,
                ty,
                value,
            } => {
                if let Some(slot) = self.lookup(name) {
                    if ty.is_some() {
                        return Err(Diagnostic::new(
                            "E1020",
                            format!("the name `{name}` already has a declaration"),
                            *name_span,
                        ));
                    }
                    let expected = self.locals[slot as usize];
                    let value = self.check_expr(value, expected)?;
                    return Ok(HStmt::Assign { slot, value });
                }
                if self.func_index.contains_key(name) {
                    return Err(Diagnostic::new(
                        "E1019",
                        format!("cannot assign to the function `{name}`"),
                        *name_span,
                    ));
                }
                // The first assignment declares a new local.
                let (value, local_ty) = match ty {
                    Some(annotation) => {
                        let annotated = resolve_type(self.store, annotation)?;
                        let value = self.check_expr(value, annotated)?;
                        (value, annotated)
                    }
                    None => {
                        let value = self.synth_expr(value)?;
                        let ty = value.ty;
                        (value, ty)
                    }
                };
                let slot = self.locals.len() as u32;
                self.locals.push(local_ty);
                self.scopes
                    .last_mut()
                    .expect("a scope is always open")
                    .insert(name.clone(), slot);
                Ok(HStmt::Assign { slot, value })
            }
            StmtKind::While { cond, body } => {
                let cond = self.check_expr(cond, BOOL)?;
                self.scopes.push(HashMap::new());
                self.loop_depth += 1;
                let result = self.check_block(body, BlockMode::Stmt, stmt.span);
                self.loop_depth -= 1;
                self.scopes.pop();
                let (body, _) = result?;
                Ok(HStmt::While { cond, body })
            }
            StmtKind::Return { value } => {
                let ret = self.ret.ok_or_else(|| {
                    Diagnostic::new(
                        "E1016",
                        "`return` is not valid at the top level of a module",
                        stmt.span,
                    )
                })?;
                let value = match value {
                    Some(expr) => Some(self.check_expr(expr, ret)?),
                    None => {
                        if ret != UNIT {
                            return Err(self.mismatch(ret, UNIT, stmt.span));
                        }
                        None
                    }
                };
                Ok(HStmt::Return { value })
            }
            StmtKind::Break => {
                if self.loop_depth == 0 {
                    return Err(Diagnostic::new(
                        "E1008",
                        "`break` is only valid inside a loop",
                        stmt.span,
                    ));
                }
                Ok(HStmt::Break)
            }
            StmtKind::Continue => {
                if self.loop_depth == 0 {
                    return Err(Diagnostic::new(
                        "E1008",
                        "`continue` is only valid inside a loop",
                        stmt.span,
                    ));
                }
                Ok(HStmt::Continue)
            }
            StmtKind::Expr(expr) => Ok(HStmt::Expr(self.synth_expr(expr)?)),
        }
    }

    /// Check an expression against an expected type.
    fn check_expr(&mut self, expr: &ast::Expr, expected: TypeId) -> Result<HExpr, Diagnostic> {
        if let ExprKind::If { arms, else_body } = &expr.kind {
            return self.check_if(arms, else_body, Some(expected), expr.span);
        }
        let found = self.synth_expr(expr)?;
        if !self.store.compatible(expected, found.ty) {
            return Err(self.mismatch(expected, found.ty, expr.span));
        }
        Ok(found)
    }

    /// Synthesize an expression type.
    fn synth_expr(&mut self, expr: &ast::Expr) -> Result<HExpr, Diagnostic> {
        match &expr.kind {
            ExprKind::Int(v) => Ok(HExpr {
                ty: INT,
                kind: HExprKind::Int(*v),
            }),
            ExprKind::Str(v) => Ok(HExpr {
                ty: STRING,
                kind: HExprKind::Str(v.clone()),
            }),
            ExprKind::Bool(v) => Ok(HExpr {
                ty: BOOL,
                kind: HExprKind::Bool(*v),
            }),
            ExprKind::Name(name) => {
                if let Some(slot) = self.lookup(name) {
                    return Ok(HExpr {
                        ty: self.locals[slot as usize],
                        kind: HExprKind::Local(slot),
                    });
                }
                if self.func_index.contains_key(name) {
                    return Err(Diagnostic::new(
                        "E1018",
                        format!("the function `{name}` is not a value in this language slice"),
                        expr.span,
                    ));
                }
                Err(Diagnostic::new(
                    "E1005",
                    format!("cannot find `{name}` in this scope"),
                    expr.span,
                ))
            }
            ExprKind::Not(inner) => {
                let inner = self.check_expr(inner, BOOL)?;
                Ok(HExpr {
                    ty: BOOL,
                    kind: HExprKind::Not(Box::new(inner)),
                })
            }
            ExprKind::Neg(inner) => {
                let inner = self.check_expr(inner, INT)?;
                Ok(HExpr {
                    ty: INT,
                    kind: HExprKind::Neg(Box::new(inner)),
                })
            }
            ExprKind::Binary { op, left, right } => self.synth_binary(*op, left, right),
            ExprKind::And(left, right) => {
                let left = self.check_expr(left, BOOL)?;
                let right = self.check_expr(right, BOOL)?;
                Ok(HExpr {
                    ty: BOOL,
                    kind: HExprKind::And(Box::new(left), Box::new(right)),
                })
            }
            ExprKind::Or(left, right) => {
                let left = self.check_expr(left, BOOL)?;
                let right = self.check_expr(right, BOOL)?;
                Ok(HExpr {
                    ty: BOOL,
                    kind: HExprKind::Or(Box::new(left), Box::new(right)),
                })
            }
            ExprKind::Call {
                name,
                name_span,
                args,
            } => {
                if self.lookup(name).is_some() {
                    return Err(Diagnostic::new(
                        "E1007",
                        format!("`{name}` is not a function"),
                        *name_span,
                    ));
                }
                let func = *self.func_index.get(name).ok_or_else(|| {
                    Diagnostic::new(
                        "E1005",
                        format!("cannot find a function named `{name}`"),
                        *name_span,
                    )
                })?;
                let (params, ret) = self.sigs[func as usize].clone();
                if args.len() != params.len() {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!(
                            "the function `{name}` expects {} argument(s), found {}",
                            params.len(),
                            args.len()
                        ),
                        expr.span,
                    ));
                }
                let mut checked = Vec::new();
                for (arg, param) in args.iter().zip(params.iter()) {
                    checked.push(self.check_expr(arg, *param)?);
                }
                Ok(HExpr {
                    ty: ret,
                    kind: HExprKind::Call {
                        func,
                        args: checked,
                    },
                })
            }
            ExprKind::If { arms, else_body } => self.check_if(arms, else_body, None, expr.span),
        }
    }

    fn synth_binary(
        &mut self,
        op: BinOp,
        left: &ast::Expr,
        right: &ast::Expr,
    ) -> Result<HExpr, Diagnostic> {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                let l = self.check_expr(left, INT)?;
                let r = self.check_expr(right, INT)?;
                Ok(HExpr {
                    ty: INT,
                    kind: HExprKind::Binary {
                        op,
                        operand_ty: INT,
                        left: Box::new(l),
                        right: Box::new(r),
                    },
                })
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let l = self.check_expr(left, INT)?;
                let r = self.check_expr(right, INT)?;
                Ok(HExpr {
                    ty: BOOL,
                    kind: HExprKind::Binary {
                        op,
                        operand_ty: INT,
                        left: Box::new(l),
                        right: Box::new(r),
                    },
                })
            }
            BinOp::Eq | BinOp::Ne => {
                let l = self.synth_expr(left)?;
                let operand_ty = l.ty;
                if !matches!(operand_ty, t if t == INT || t == BOOL || t == STRING) {
                    return Err(Diagnostic::new(
                        "E1017",
                        format!(
                            "cannot compare {} values with `{}`",
                            self.store.display(operand_ty),
                            op.text()
                        ),
                        left.span,
                    ));
                }
                let r = self.check_expr(right, operand_ty)?;
                Ok(HExpr {
                    ty: BOOL,
                    kind: HExprKind::Binary {
                        op,
                        operand_ty,
                        left: Box::new(l),
                        right: Box::new(r),
                    },
                })
            }
        }
    }

    /// Check an `if` expression. `expected` is `Some` in check mode.
    fn check_if(
        &mut self,
        arms: &[(ast::Expr, Vec<ast::Stmt>)],
        else_body: &Option<Vec<ast::Stmt>>,
        expected: Option<TypeId>,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if let Some(expected) = expected {
            if else_body.is_none() && expected != UNIT {
                return Err(self.mismatch(expected, UNIT, span));
            }
        }
        let branch_mode = match expected {
            Some(t) if t != UNIT => BlockMode::Value(t),
            Some(_) => BlockMode::Stmt,
            None => match else_body {
                Some(_) => BlockMode::Synth,
                // Without `else` the value is `()`, so branch values
                // are discarded.
                None => BlockMode::Stmt,
            },
        };
        let mut checked_arms = Vec::new();
        let mut branch_types: Vec<(TypeId, Span)> = Vec::new();
        for (cond, body) in arms {
            let cond = self.check_expr(cond, BOOL)?;
            self.scopes.push(HashMap::new());
            let result = self.check_block(body, branch_mode, span);
            self.scopes.pop();
            let (body_h, ty) = result?;
            let branch_span = body.last().map(|s| s.span).unwrap_or(span);
            branch_types.push((ty, branch_span));
            checked_arms.push((cond, body_h));
        }
        let else_h = match else_body {
            Some(body) => {
                self.scopes.push(HashMap::new());
                let result = self.check_block(body, branch_mode, span);
                self.scopes.pop();
                let (body_h, ty) = result?;
                let branch_span = body.last().map(|s| s.span).unwrap_or(span);
                branch_types.push((ty, branch_span));
                Some(body_h)
            }
            None => None,
        };
        let ty = match expected {
            Some(t) => t,
            None => {
                if else_h.is_none() {
                    UNIT
                } else {
                    self.join_branches(&branch_types)?
                }
            }
        };
        Ok(HExpr {
            ty,
            kind: HExprKind::If {
                arms: checked_arms,
                else_body: else_h,
            },
        })
    }

    /// Join branch types. `Never` branches do not contribute.
    fn join_branches(&self, branches: &[(TypeId, Span)]) -> Result<TypeId, Diagnostic> {
        let mut joined: Option<TypeId> = None;
        for (ty, span) in branches {
            if *ty == NEVER {
                continue;
            }
            match joined {
                None => joined = Some(*ty),
                Some(first) => {
                    if first != *ty {
                        return Err(self.mismatch(first, *ty, *span));
                    }
                }
            }
        }
        Ok(joined.unwrap_or(NEVER))
    }
}
