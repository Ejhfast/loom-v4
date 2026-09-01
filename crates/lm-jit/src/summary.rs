//! Conservative behavior summaries for verified functions.

use crate::{instruction_treatment, ExitBehavior, TreatmentClass};
use lm_bytecode::{ExtendedInstr, Func, Instr};
use std::collections::VecDeque;
use std::sync::Arc;

/// Transitive behavior facts for one verified function.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FunctionBehavior {
    may_suspend_or_perform: bool,
    may_allocate: bool,
    may_collect: bool,
    may_grow_list: bool,
    may_mutate: bool,
    may_fault_or_replay: bool,
    has_dynamic_call: bool,
}

impl FunctionBehavior {
    const CONSERVATIVE: FunctionBehavior = FunctionBehavior {
        may_suspend_or_perform: true,
        may_allocate: true,
        may_collect: true,
        may_grow_list: true,
        may_mutate: true,
        may_fault_or_replay: true,
        has_dynamic_call: true,
    };

    pub(crate) const fn conservative() -> FunctionBehavior {
        FunctionBehavior::CONSERVATIVE
    }

    /// Return true when the function can suspend or perform an effect.
    pub fn may_suspend_or_perform(self) -> bool {
        self.may_suspend_or_perform
    }

    /// Return true when the function can allocate a guest object.
    pub fn may_allocate(self) -> bool {
        self.may_allocate
    }

    /// Return true when the function can request guest collection.
    pub fn may_collect(self) -> bool {
        self.may_collect
    }

    /// Return true when the function can grow an existing list.
    pub fn may_grow_list(self) -> bool {
        self.may_grow_list
    }

    /// Return true when the function can mutate guest state.
    pub fn may_mutate(self) -> bool {
        self.may_mutate
    }

    /// Return true when the function can fault or replay an instruction.
    pub fn may_fault_or_replay(self) -> bool {
        self.may_fault_or_replay
    }

    /// Return true when the function calls a runtime-selected target.
    pub fn has_dynamic_call(self) -> bool {
        self.has_dynamic_call
    }

    fn include(&mut self, other: FunctionBehavior) -> bool {
        let prior = *self;
        self.may_suspend_or_perform |= other.may_suspend_or_perform;
        self.may_allocate |= other.may_allocate;
        self.may_collect |= other.may_collect;
        self.may_grow_list |= other.may_grow_list;
        self.may_mutate |= other.may_mutate;
        self.may_fault_or_replay |= other.may_fault_or_replay;
        self.has_dynamic_call |= other.has_dynamic_call;
        *self != prior
    }
}

/// One immutable behavior table for a namespace revision.
#[derive(Debug, Clone, Default)]
pub struct FunctionBehaviors(Arc<[FunctionBehavior]>);

impl FunctionBehaviors {
    /// Analyze every verified function in one runtime table.
    pub fn analyze(functions: &lm_bytecode::CodeTable<Func>) -> FunctionBehaviors {
        let count = functions.len();
        let mut summaries = vec![FunctionBehavior::default(); count];
        let mut callers = vec![Vec::new(); count];

        for (function, definition) in functions.into_iter().enumerate() {
            let mut local = FunctionBehavior::default();
            for instruction in definition.blocks.iter().flatten() {
                let treatment = instruction_treatment(instruction);
                local.may_suspend_or_perform |= matches!(
                    treatment.exit(),
                    ExitBehavior::Effect | ExitBehavior::Boundary
                );
                local.may_allocate |= treatment.exit() == ExitBehavior::Allocation;
                local.may_collect |= treatment.exit() == ExitBehavior::Allocation
                    || treatment.class() == TreatmentClass::FastPath
                    || (treatment.class() == TreatmentClass::Helper
                        && treatment.is_replay_barrier());
                local.may_grow_list |= grows_existing_list(instruction);
                local.may_mutate |= treatment.is_replay_barrier();
                local.may_fault_or_replay |= treatment.replays()
                    || treatment.fault_stack() != crate::FaultStack::None
                    || treatment.exit() == ExitBehavior::Fault;

                if let Some(target) = direct_target(instruction) {
                    if let Some(target_callers) = callers.get_mut(target as usize) {
                        target_callers.push(function);
                    } else {
                        local.include(FunctionBehavior::CONSERVATIVE);
                    }
                } else if treatment.class() == TreatmentClass::Call {
                    local.include(FunctionBehavior::CONSERVATIVE);
                }
            }
            summaries[function] = local;
        }

        let mut pending = VecDeque::with_capacity(count);
        let mut queued = vec![true; count];
        pending.extend(0..count);
        while let Some(callee) = pending.pop_front() {
            queued[callee] = false;
            let callee_summary = summaries[callee];
            for caller in callers[callee].iter().copied() {
                if summaries[caller].include(callee_summary) && !queued[caller] {
                    queued[caller] = true;
                    pending.push_back(caller);
                }
            }
        }

        FunctionBehaviors(Arc::from(summaries))
    }

    /// Return one function summary.
    pub fn get(&self, function: u32) -> FunctionBehavior {
        self.0
            .get(function as usize)
            .copied()
            .unwrap_or(FunctionBehavior::CONSERVATIVE)
    }
}

fn direct_target(instruction: &Instr) -> Option<u32> {
    match instruction {
        Instr::Call(target) | Instr::CallG { func: target, .. } => Some(*target),
        _ => None,
    }
}

fn grows_existing_list(instruction: &Instr) -> bool {
    matches!(
        instruction,
        Instr::ListPush
            | Instr::Extended(
                ExtendedInstr::ListInsert | ExtendedInstr::ListReserve | ExtendedInstr::ListReorder
            )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function(instructions: Vec<Instr>) -> Func {
        Func {
            name: String::new(),
            type_params: 0,
            effect_params: 0,
            params: Vec::new(),
            param_muts: Vec::new(),
            ret: 0,
            row: Vec::new(),
            captures: Vec::new(),
            local_types: Vec::new(),
            blocks: vec![instructions],
            param_names: Vec::new(),
        }
    }

    #[test]
    fn summaries_propagate_through_recursive_direct_calls() {
        let functions = lm_bytecode::CodeTable::from(vec![
            function(vec![Instr::Call(1), Instr::Return]),
            function(vec![Instr::Call(0), Instr::ListPush, Instr::Return]),
        ]);
        let summaries = FunctionBehaviors::analyze(&functions);
        assert!(summaries.get(0).may_grow_list());
        assert!(summaries.get(1).may_grow_list());
        assert!(summaries.get(0).may_mutate());
        assert!(!summaries.get(0).may_allocate());
        assert!(summaries.get(0).may_collect());
    }

    #[test]
    fn dynamic_calls_remain_conservative() {
        let functions = lm_bytecode::CodeTable::from(vec![function(vec![
            Instr::CallValue { argc: 0 },
            Instr::Return,
        ])]);
        let summary = FunctionBehaviors::analyze(&functions).get(0);
        assert!(summary.has_dynamic_call());
        assert!(summary.may_allocate());
        assert!(summary.may_collect());
        assert!(summary.may_suspend_or_perform());
    }

    #[test]
    fn scalar_leaf_does_not_gain_heap_effects() {
        let functions = lm_bytecode::CodeTable::from(vec![function(vec![
            Instr::LoadLocal(0),
            Instr::ConstInt(1),
            Instr::Add,
            Instr::Return,
        ])]);
        let summary = FunctionBehaviors::analyze(&functions).get(0);
        assert!(!summary.may_allocate());
        assert!(!summary.may_collect());
        assert!(!summary.may_grow_list());
        assert!(!summary.may_mutate());
        assert!(summary.may_fault_or_replay());
    }

    #[test]
    fn allocation_and_effect_facts_remain_independent() {
        let functions = lm_bytecode::CodeTable::from(vec![
            function(vec![Instr::New(0), Instr::Return]),
            function(vec![
                Instr::Perform {
                    op: lm_abi::OP_CLOCK_NOW,
                    argc: 0,
                    reply_ty: 0,
                },
                Instr::Return,
            ]),
        ]);
        let summaries = FunctionBehaviors::analyze(&functions);
        assert!(summaries.get(0).may_allocate());
        assert!(summaries.get(0).may_collect());
        assert!(!summaries.get(0).may_suspend_or_perform());
        assert!(!summaries.get(1).may_allocate());
        assert!(summaries.get(1).may_suspend_or_perform());
    }
}
