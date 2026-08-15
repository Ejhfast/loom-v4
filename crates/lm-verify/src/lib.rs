//! Independent bytecode verifier.
//!
//! The verifier receives a decoded module and rejects it unless every
//! function is well formed. It reconstructs the operand-stack types and
//! the local-slot types at each block entry with a worklist, and it
//! checks jumps, calls, and returns. It shares no code with the source
//! checker.

use lm_bytecode::{Func, Instr, Module, PrimTy};
use std::collections::VecDeque;
use std::fmt;

/// The largest operand-stack depth the verifier accepts for one function.
const MAX_STATIC_STACK: usize = 4096;

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

/// The abstract state at one program point.
#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    /// `None` marks a local slot without a known value.
    locals: Vec<Option<PrimTy>>,
    stack: Vec<PrimTy>,
}

/// Verify a full module. Every function must pass.
pub fn verify_module(module: &Module) -> Result<(), VerifyError> {
    let entry = module.entry as usize;
    if entry >= module.funcs.len() {
        return Err(VerifyError {
            func: module.entry,
            message: format!(
                "entry index {} is not inside the function table of length {}",
                module.entry,
                module.funcs.len()
            ),
        });
    }
    if !module.funcs[entry].params.is_empty() {
        return Err(VerifyError {
            func: module.entry,
            message: "the entry function must not have parameters".to_string(),
        });
    }
    for (idx, func) in module.funcs.iter().enumerate() {
        verify_func(module, func, idx as u32)?;
    }
    Ok(())
}

fn err(func: u32, message: impl Into<String>) -> VerifyError {
    VerifyError {
        func,
        message: message.into(),
    }
}

fn verify_func(module: &Module, func: &Func, fidx: u32) -> Result<(), VerifyError> {
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
                module,
                func,
                fidx,
                bidx,
                iidx,
                instr,
                &mut state,
                |target, edge_state| merge(fidx, target, edge_state, &mut states, &mut worklist),
            )?;
        }
    }
    Ok(())
}

/// Merge an edge state into a target block. Queue the block again when
/// its entry state changes.
fn merge(
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
            if existing.stack != edge.stack {
                return Err(err(
                    fidx,
                    format!("block {target} entry stack shapes do not agree"),
                ));
            }
            let mut changed = false;
            for (have, new) in existing.locals.iter_mut().zip(edge.locals.iter()) {
                if *have != *new && have.is_some() {
                    *have = None;
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
    module: &Module,
    func: &Func,
    fidx: u32,
    bidx: usize,
    iidx: usize,
    instr: &Instr,
    state: &mut State,
    mut edge: impl FnMut(usize, State) -> Result<(), VerifyError>,
) -> Result<(), VerifyError> {
    let fail = |what: &str| err(fidx, format!("block {bidx}, instruction {iidx}: {what}"));
    let pop = |state: &mut State, want: Option<PrimTy>| -> Result<PrimTy, VerifyError> {
        let ty = state
            .stack
            .pop()
            .ok_or_else(|| fail("pop from an empty stack"))?;
        if let Some(want) = want {
            if ty != want {
                return Err(fail(&format!("expected {want} on the stack, found {ty}")));
            }
        }
        Ok(ty)
    };
    let push = |state: &mut State, ty: PrimTy| -> Result<(), VerifyError> {
        if state.stack.len() >= MAX_STATIC_STACK {
            return Err(fail("static stack depth limit exceeded"));
        }
        state.stack.push(ty);
        Ok(())
    };
    match instr {
        Instr::ConstUnit => push(state, PrimTy::Unit)?,
        Instr::ConstBool(_) => push(state, PrimTy::Bool)?,
        Instr::ConstInt(_) => push(state, PrimTy::Int)?,
        Instr::ConstStr(_) => push(state, PrimTy::Str)?,
        Instr::LoadLocal(slot) => {
            let ty = state.locals[*slot as usize]
                .ok_or_else(|| fail("load from a local without a value"))?;
            push(state, ty)?;
        }
        Instr::StoreLocal(slot) => {
            let ty = pop(state, None)?;
            state.locals[*slot as usize] = Some(ty);
        }
        Instr::Pop => {
            pop(state, None)?;
        }
        Instr::Add | Instr::Sub | Instr::Mul | Instr::Div | Instr::Rem => {
            pop(state, Some(PrimTy::Int))?;
            pop(state, Some(PrimTy::Int))?;
            push(state, PrimTy::Int)?;
        }
        Instr::Neg => {
            pop(state, Some(PrimTy::Int))?;
            push(state, PrimTy::Int)?;
        }
        Instr::Not => {
            pop(state, Some(PrimTy::Bool))?;
            push(state, PrimTy::Bool)?;
        }
        Instr::LtInt | Instr::LeInt | Instr::GtInt | Instr::GeInt | Instr::EqInt | Instr::NeInt => {
            pop(state, Some(PrimTy::Int))?;
            pop(state, Some(PrimTy::Int))?;
            push(state, PrimTy::Bool)?;
        }
        Instr::EqBool | Instr::NeBool => {
            pop(state, Some(PrimTy::Bool))?;
            pop(state, Some(PrimTy::Bool))?;
            push(state, PrimTy::Bool)?;
        }
        Instr::EqStr | Instr::NeStr => {
            pop(state, Some(PrimTy::Str))?;
            pop(state, Some(PrimTy::Str))?;
            push(state, PrimTy::Bool)?;
        }
        Instr::Call(callee) => {
            let sig = &module.funcs[*callee as usize];
            for param in sig.params.iter().rev() {
                pop(state, Some(*param))?;
            }
            push(state, sig.ret)?;
        }
        Instr::Jump(target) => {
            edge(*target as usize, state.clone())?;
        }
        Instr::JumpIfFalse(target) | Instr::JumpIfTrue(target) => {
            pop(state, Some(PrimTy::Bool))?;
            edge(*target as usize, state.clone())?;
        }
        Instr::Return => {
            pop(state, Some(func.ret))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lm_bytecode::{Func, Instr::*, Module, PrimTy};

    fn module_with(blocks: Vec<Vec<Instr>>) -> Module {
        Module {
            strings: vec!["s".to_string()],
            funcs: vec![Func {
                name: "main".to_string(),
                params: vec![],
                ret: PrimTy::Int,
                local_count: 1,
                blocks,
            }],
            entry: 0,
        }
    }

    #[test]
    fn accepts_simple_function() {
        let m = module_with(vec![vec![ConstInt(1), ConstInt(2), Add, Return]]);
        assert!(verify_module(&m).is_ok());
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
        assert!(e.message.contains("expected Int"), "{e}");
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
    fn rejects_wrong_return_type() {
        let m = module_with(vec![vec![ConstBool(true), Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("expected Int"), "{e}");
    }

    #[test]
    fn rejects_disagreeing_join_stacks() {
        // Block 1 receives Int from block 0 and Bool from itself.
        let m = module_with(vec![
            vec![ConstBool(true), JumpIfTrue(1), ConstInt(1), Jump(1)],
            vec![Pop, ConstBool(false), Jump(1)],
        ]);
        // Block 0 reaches block 1 with [] (true edge) and [Int] (jump).
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("stack shapes"), "{e}");
    }

    #[test]
    fn rejects_entry_with_parameters() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.funcs[0].params = vec![PrimTy::Int];
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("entry function"), "{e}");
    }

    #[test]
    fn rejects_call_argument_mismatch() {
        let mut m = module_with(vec![vec![ConstBool(true), Call(1), Return]]);
        m.funcs.push(Func {
            name: "f".to_string(),
            params: vec![PrimTy::Int],
            ret: PrimTy::Int,
            local_count: 1,
            blocks: vec![vec![LoadLocal(0), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("expected Int"), "{e}");
    }

    #[test]
    fn merges_conditional_local_state() {
        // Store to local 0 only on one path. A later load must fail.
        let m = module_with(vec![
            vec![ConstBool(true), JumpIfTrue(2), Jump(1)],
            vec![ConstInt(1), StoreLocal(0), Jump(2)],
            vec![LoadLocal(0), Return],
        ]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("without a value"), "{e}");
    }
}
