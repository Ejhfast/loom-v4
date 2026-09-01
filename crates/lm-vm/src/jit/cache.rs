//! Native region cache for one arena layout.

use lm_jit::{Failure, FunctionInput, NativeDispatchRow};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

pub(crate) const DEFAULT_CODE_BUDGET: usize = 128 * 1024 * 1024;
const AUTO_COMPILE_WORK: u64 = 100_000;
const TIER_COMPILED: u64 = u64::MAX - 1;
const TIER_DENIED: u64 = u64::MAX;
const ENTRY_SAMPLE_COUNT: u32 = 8;
const MIN_ENTRY_RETIRED: u64 = 32;
const PRODUCTIVITY_PROVEN: u32 = u32::MAX - 1;
const PRODUCTIVITY_DENIED: u32 = u32::MAX;
const NO_CAPACITY_EPOCH: u64 = u64::MAX;
const MAX_DENSE_EFFECT_CYCLE_COST: usize = 16;

#[derive(Clone, Copy)]
pub(crate) struct TierDecision {
    pub(crate) enter_native: bool,
}

#[derive(Clone)]
pub(crate) struct NativeCodeState(Arc<NativeCodeRevision>);

#[derive(Clone)]
struct NativeCodeRevision {
    slots: lm_bytecode::CodeTable<Arc<NativeSlot>>,
    entries: Arc<Vec<usize>>,
    class_parents: Arc<Vec<u32>>,
    dispatch_rows: Arc<Vec<NativeDispatchRow>>,
    dispatch_methods: Arc<Vec<u32>>,
    candidates: Arc<Vec<u64>>,
    budget: Arc<CodeBudget>,
    compiled: Arc<AtomicUsize>,
}

pub(super) struct CodeBudget {
    limit: usize,
    used: AtomicUsize,
    epoch: AtomicU64,
}

impl CodeBudget {
    pub(super) fn new(limit: usize) -> CodeBudget {
        CodeBudget {
            limit,
            used: AtomicUsize::new(0),
            epoch: AtomicU64::new(0),
        }
    }

    fn reserve(&self, bytes: usize) -> bool {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|next| *next <= self.limit)
            })
            .is_ok()
    }

    fn release(&self, bytes: usize) {
        self.used.fetch_sub(bytes, Ordering::AcqRel);
        self.epoch.fetch_add(1, Ordering::Release);
    }

    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
}

pub(super) enum RegionFailure {
    Compile(Failure),
    Capacity,
    Busy,
}

impl NativeCodeState {
    #[cfg(test)]
    pub(crate) fn new(module: &crate::NamespaceRuntime) -> NativeCodeState {
        Self::with_budget(module, Arc::new(CodeBudget::new(DEFAULT_CODE_BUDGET)))
    }

    pub(super) fn with_budget(
        module: &crate::NamespaceRuntime,
        budget: Arc<CodeBudget>,
    ) -> NativeCodeState {
        let mut revision = NativeCodeRevision {
            slots: lm_bytecode::CodeTable::default(),
            entries: Arc::new(Vec::new()),
            class_parents: Arc::new(Vec::new()),
            dispatch_rows: Arc::new(Vec::new()),
            dispatch_methods: Arc::new(Vec::new()),
            candidates: Arc::new(Vec::new()),
            budget,
            compiled: Arc::new(AtomicUsize::new(0)),
        };
        while revision.slots.len() < module.funcs.len() {
            revision.push_slot(module, revision.slots.len());
        }
        revision.extend_classes(module);
        revision.extend_dispatch(module);
        NativeCodeState(Arc::new(revision))
    }

    pub(crate) fn extend(&mut self, module: &crate::NamespaceRuntime) {
        if self.0.slots.len() >= module.funcs.len()
            && self.0.class_parents.len() >= module.classes.len()
            && self.0.dispatch_rows.len() >= module.dispatch.len()
        {
            return;
        }
        let mut revision = self.0.as_ref().clone();
        while revision.slots.len() < module.funcs.len() {
            revision.push_slot(module, revision.slots.len());
        }
        revision.extend_classes(module);
        revision.extend_dispatch(module);
        self.0 = Arc::new(revision);
    }

    pub(super) fn slot(&self, function: u32) -> Option<&Arc<NativeSlot>> {
        self.0.slots.get(function as usize)
    }

    pub(super) fn entries(&self) -> &[usize] {
        self.0.entries.as_slice()
    }

    pub(super) fn class_parents(&self) -> &[u32] {
        self.0.class_parents.as_slice()
    }

    pub(super) fn dispatch_rows(&self) -> &[NativeDispatchRow] {
        self.0.dispatch_rows.as_slice()
    }

    pub(super) fn dispatch_methods(&self) -> &[u32] {
        self.0.dispatch_methods.as_slice()
    }

    pub(crate) fn enter_frame(
        &self,
        function: u32,
        work_scale: u32,
        profile: bool,
    ) -> TierDecision {
        let Some(slot) = self.slot(function) else {
            return TierDecision {
                enter_native: false,
            };
        };
        slot.note_profile(work_scale, profile);
        if !self.is_candidate(function) {
            return TierDecision {
                enter_native: false,
            };
        }
        slot.enter_frame(work_scale)
    }

    pub(crate) fn note_event(&self, function: u32, work_scale: u32, profile: bool) -> bool {
        let Some(slot) = self.slot(function) else {
            return false;
        };
        slot.note_profile(work_scale, profile);
        self.is_candidate(function) && slot.note_event(work_scale)
    }

    pub(crate) fn is_candidate(&self, function: u32) -> bool {
        let word = function as usize / 64;
        let bit = function % 64;
        self.0
            .candidates
            .get(word)
            .is_some_and(|value| value & (1_u64 << bit) != 0)
    }

    pub(crate) fn has_compiled_code(&self) -> bool {
        self.0.compiled.load(Ordering::Relaxed) != 0
    }

    pub(crate) fn ready_for_auto(&self, function: u32) -> bool {
        self.is_candidate(function)
            && self
                .slot(function)
                .is_some_and(|slot| slot.ready_for_auto())
    }

    pub(crate) fn note_native_exit(&self, function: u32, retired: u64) -> bool {
        self.slot(function)
            .is_some_and(|slot| slot.note_native_exit(retired))
    }

    pub(crate) fn promote_call_target(&self, function: u32) {
        if self.is_candidate(function) {
            if let Some(slot) = self.slot(function) {
                slot.promote();
            }
        }
    }

    pub(crate) fn call_target_is_denied(&self, function: u32) -> bool {
        self.slot(function)
            .is_none_or(|slot| !slot.call_promotable || slot.is_denied())
    }

    pub(super) fn append_profile(
        &self,
        tables: &lm_bytecode::CodeTables,
        profile: &mut crate::JitProfile,
        rejection_totals: &mut BTreeMap<String, u64>,
        treatment_totals: &mut BTreeMap<String, u64>,
    ) {
        for function in 0..self.0.slots.len() {
            let Some(slot) = self.0.slots.get(function) else {
                continue;
            };
            let estimated = slot.profile_work.load(Ordering::Relaxed);
            if estimated == 0 {
                continue;
            }
            let Some(definition) = tables.funcs.get(function) else {
                continue;
            };
            let candidate = self.is_candidate(function as u32);
            profile.estimated_instructions =
                profile.estimated_instructions.saturating_add(estimated);
            if candidate {
                profile.candidate_instructions =
                    profile.candidate_instructions.saturating_add(estimated);
            }
            let mut rejections = function_rejections(tables, definition, candidate);
            if let Some(reason) = slot.unsupported_reason() {
                let label = reason.label();
                if let Some((_, count)) = rejections
                    .iter_mut()
                    .find(|(existing, _)| existing == label)
                {
                    *count = count.saturating_add(1);
                } else {
                    rejections.push((label.to_string(), 1));
                }
            }
            let treatment_gaps = function_treatment_gaps(definition);
            let unit = estimated / u64::from(slot.event_weight.max(1));
            for (reason, count) in &rejections {
                let weight = unit.saturating_mul(u64::from(*count));
                rejection_totals
                    .entry(reason.clone())
                    .and_modify(|current| *current = current.saturating_add(weight))
                    .or_insert(weight);
            }
            for (instruction, count) in &treatment_gaps {
                let weight = unit.saturating_mul(u64::from(*count));
                treatment_totals
                    .entry(instruction.clone())
                    .and_modify(|current| *current = current.saturating_add(weight))
                    .or_insert(weight);
            }
            profile.hot_functions.push(crate::JitFunctionProfile {
                function: function as u32,
                name: definition.name.clone(),
                estimated_instructions: estimated,
                candidate,
                rejections: rejections.into_iter().map(|(reason, _)| reason).collect(),
                treatment_gaps: treatment_gaps
                    .into_iter()
                    .map(|(instruction, _)| instruction)
                    .collect(),
            });
        }
    }

    pub(super) fn reset_profile(&self) {
        for function in 0..self.0.slots.len() {
            if let Some(slot) = self.0.slots.get(function) {
                slot.profile_work.store(0, Ordering::Relaxed);
            }
        }
    }
}

impl NativeCodeRevision {
    fn extend_dispatch(&mut self, module: &crate::NamespaceRuntime) {
        let dispatch = module.dispatch_store();
        let rows = Arc::make_mut(&mut self.dispatch_rows);
        let methods = Arc::make_mut(&mut self.dispatch_methods);
        while rows.len() < dispatch.len() {
            let row = &dispatch[rows.len()];
            let native = NativeDispatchRow::new(row.base(), row.cells().len(), methods.len());
            methods.extend_from_slice(row.cells());
            rows.push(native);
        }
    }

    fn extend_classes(&mut self, module: &crate::NamespaceRuntime) {
        let parents = Arc::make_mut(&mut self.class_parents);
        while parents.len() < module.classes.len() {
            let parent = module.classes[parents.len()]
                .parent()
                .unwrap_or(lm_bytecode::NO_PARENT);
            parents.push(parent);
        }
    }

    fn push_slot(&mut self, module: &crate::NamespaceRuntime, function: usize) {
        let definition = &module.funcs[function];
        let candidate = function_is_candidate(module, definition);
        let prefers_interpreter = function_prefers_interpreter(module, definition);
        let word = function / 64;
        let bit = function % 64;
        if Arc::make_mut(&mut self.candidates).len() <= word {
            Arc::make_mut(&mut self.candidates).resize(word + 1, 0);
        }
        if candidate {
            Arc::make_mut(&mut self.candidates)[word] |= 1_u64 << bit;
        }
        let slot = Arc::new(NativeSlot::new(
            definition,
            candidate,
            prefers_interpreter,
            Arc::clone(&self.budget),
            Arc::clone(&self.compiled),
        ));
        Arc::make_mut(&mut self.entries).push(Arc::as_ptr(&slot.entry) as usize);
        self.slots.push(slot);
    }
}

pub(super) struct NativeSlot {
    verdict: OnceLock<Result<Arc<lm_jit::CompiledRegion>, Failure>>,
    compiling: AtomicBool,
    capacity_epoch: AtomicU64,
    entry: Arc<lm_jit::NativeEntryCell>,
    budget: Arc<CodeBudget>,
    compiled: Arc<AtomicUsize>,
    event_weight: u32,
    tier: AtomicU64,
    productivity: AtomicU32,
    profile_work: AtomicU64,
    call_promotable: bool,
    prefers_interpreter: bool,
}

impl NativeSlot {
    fn new(
        function: &lm_bytecode::Func,
        candidate: bool,
        prefers_interpreter: bool,
        budget: Arc<CodeBudget>,
        compiled: Arc<AtomicUsize>,
    ) -> NativeSlot {
        let event_weight = function
            .blocks
            .iter()
            .map(Vec::len)
            .sum::<usize>()
            .clamp(1, u32::MAX as usize) as u32;
        let call_promotable = !prefers_interpreter;
        NativeSlot {
            verdict: OnceLock::new(),
            compiling: AtomicBool::new(false),
            capacity_epoch: AtomicU64::new(NO_CAPACITY_EPOCH),
            entry: Arc::new(lm_jit::NativeEntryCell::default()),
            budget,
            compiled,
            event_weight,
            tier: AtomicU64::new(if candidate { 0 } else { TIER_DENIED }),
            productivity: AtomicU32::new(0),
            profile_work: AtomicU64::new(0),
            call_promotable,
            prefers_interpreter,
        }
    }

    pub(super) fn region<'a, F>(
        &self,
        compiler: &lm_jit::JitEngine,
        input: F,
    ) -> Result<Arc<lm_jit::CompiledRegion>, RegionFailure>
    where
        F: FnOnce() -> Result<FunctionInput<'a>, Failure>,
    {
        if let Some(result) = self.verdict.get() {
            return result.clone().map_err(RegionFailure::Compile);
        }
        let budget_epoch = self.budget.epoch();
        if self.capacity_epoch.load(Ordering::Acquire) == budget_epoch {
            return Err(RegionFailure::Capacity);
        }
        if self
            .compiling
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(RegionFailure::Busy);
        }
        let result = input().and_then(|input| compiler.compile(input));
        let result = match result {
            Ok(region) => {
                if !self.budget.reserve(region.code_size()) {
                    self.capacity_epoch.store(budget_epoch, Ordering::Release);
                    Err(RegionFailure::Capacity)
                } else if let Err(error) = self.entry.prepare(&region) {
                    self.budget.release(region.code_size());
                    let result = Err(error);
                    let _ = self.verdict.set(result.clone());
                    self.tier.store(TIER_DENIED, Ordering::Release);
                    Err(RegionFailure::Compile(error))
                } else {
                    let result = Ok(Arc::clone(&region));
                    if self.verdict.set(result).is_err() {
                        self.budget.release(region.code_size());
                        Err(RegionFailure::Busy)
                    } else {
                        self.compiled.fetch_add(1, Ordering::Relaxed);
                        self.tier.store(TIER_COMPILED, Ordering::Release);
                        self.entry.publish_prepared(&region);
                        Ok(region)
                    }
                }
            }
            Err(Failure::Unsupported(reason)) => {
                let result = Err(Failure::Unsupported(reason));
                let _ = self.verdict.set(result.clone());
                self.tier.store(TIER_DENIED, Ordering::Release);
                Err(RegionFailure::Compile(Failure::Unsupported(reason)))
            }
            Err(Failure::BackendUnavailable) => {
                let result = Err(Failure::BackendUnavailable);
                let _ = self.verdict.set(result.clone());
                self.tier.store(TIER_DENIED, Ordering::Release);
                Err(RegionFailure::Compile(Failure::BackendUnavailable))
            }
        };
        self.compiling.store(false, Ordering::Release);
        result
    }

    pub(super) fn compiled(&self) -> Option<Arc<lm_jit::CompiledRegion>> {
        self.verdict.get()?.as_ref().ok().cloned()
    }

    fn unsupported_reason(&self) -> Option<lm_jit::UnsupportedReason> {
        match self.verdict.get() {
            Some(Err(Failure::Unsupported(reason))) => Some(*reason),
            _ => None,
        }
    }

    pub(super) fn ready_for_auto(&self) -> bool {
        !self.prefers_interpreter
            && self.productivity.load(Ordering::Relaxed) != PRODUCTIVITY_DENIED
            && matches!(
                self.tier.load(Ordering::Acquire),
                AUTO_COMPILE_WORK | TIER_COMPILED
            )
    }

    fn enter_frame(&self, work_scale: u32) -> TierDecision {
        if self.prefers_interpreter {
            return TierDecision {
                enter_native: false,
            };
        }
        let state = if work_scale != 0 {
            self.add_work(work_scale)
        } else {
            self.tier.load(Ordering::Acquire)
        };
        TierDecision {
            enter_native: self.productivity.load(Ordering::Relaxed) != PRODUCTIVITY_DENIED
                && matches!(state, AUTO_COMPILE_WORK | TIER_COMPILED),
        }
    }

    fn note_event(&self, work_scale: u32) -> bool {
        !self.prefers_interpreter
            && self.productivity.load(Ordering::Relaxed) != PRODUCTIVITY_DENIED
            && matches!(self.add_work(work_scale), AUTO_COMPILE_WORK | TIER_COMPILED)
    }

    fn note_native_exit(&self, retired: u64) -> bool {
        let mut state = self.productivity.load(Ordering::Relaxed);
        loop {
            if matches!(state, PRODUCTIVITY_PROVEN | PRODUCTIVITY_DENIED) {
                return false;
            }
            let next = if retired >= MIN_ENTRY_RETIRED {
                PRODUCTIVITY_PROVEN
            } else if state + 1 >= ENTRY_SAMPLE_COUNT {
                PRODUCTIVITY_DENIED
            } else {
                state + 1
            };
            match self.productivity.compare_exchange_weak(
                state,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    if next == PRODUCTIVITY_DENIED {
                        return true;
                    }
                    return false;
                }
                Err(current) => state = current,
            }
        }
    }

    fn promote(&self) {
        if self.prefers_interpreter
            || !self.call_promotable
            || self.productivity.load(Ordering::Relaxed) == PRODUCTIVITY_DENIED
        {
            return;
        }
        let _ = self
            .tier
            .fetch_update(Ordering::Release, Ordering::Relaxed, |state| {
                (state < AUTO_COMPILE_WORK).then_some(AUTO_COMPILE_WORK)
            });
    }

    fn is_denied(&self) -> bool {
        self.tier.load(Ordering::Acquire) == TIER_DENIED
    }

    fn note_profile(&self, work_scale: u32, enabled: bool) {
        if !enabled || work_scale == 0 {
            return;
        }
        let work = u64::from(self.event_weight).saturating_mul(u64::from(work_scale));
        self.profile_work.fetch_add(work, Ordering::Relaxed);
    }

    fn add_work(&self, work_scale: u32) -> u64 {
        if self.event_weight == 0 {
            return TIER_DENIED;
        }
        let weight = u64::from(self.event_weight).saturating_mul(u64::from(work_scale));
        let previous = self
            .tier
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |state| {
                (state < AUTO_COMPILE_WORK)
                    .then(|| state.saturating_add(weight).min(AUTO_COMPILE_WORK))
            })
            .unwrap_or_else(|state| state);
        if previous < AUTO_COMPILE_WORK {
            previous.saturating_add(weight).min(AUTO_COMPILE_WORK)
        } else {
            previous
        }
    }
}

impl Drop for NativeSlot {
    fn drop(&mut self) {
        if let Some(Ok(region)) = self.verdict.get() {
            self.budget.release(region.code_size());
            self.compiled.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

fn function_is_candidate(module: &crate::NamespaceRuntime, function: &lm_bytecode::Func) -> bool {
    if !lm_jit::is_candidate(function)
        || !function
            .local_types
            .iter()
            .chain(std::iter::once(&function.ret))
            .all(|ty| type_is_candidate(module, *ty))
    {
        return false;
    }
    function
        .blocks
        .iter()
        .flatten()
        .all(|instruction| match instruction {
            lm_bytecode::Instr::Call(target) | lm_bytecode::Instr::CallG { func: target, .. } => {
                module.funcs.get(*target as usize).is_some_and(|callee| {
                    callee
                        .params
                        .iter()
                        .chain(std::iter::once(&callee.ret))
                        .all(|ty| type_is_candidate(module, *ty))
                })
            }
            lm_bytecode::Instr::Perform { reply_ty, .. }
            | lm_bytecode::Instr::PerformValue { reply_ty, .. } => {
                type_is_candidate(module, *reply_ty)
            }
            _ => true,
        })
}

fn function_prefers_interpreter(
    module: &crate::NamespaceRuntime,
    function: &lm_bytecode::Func,
) -> bool {
    effect_cycle_prefers_interpreter(function, |op| {
        module
            .bundle()
            .op(op)
            .is_some_and(|operation| operation.kind == lm_abi::OpKind::VmControl)
    })
}

fn effect_cycle_prefers_interpreter(
    function: &lm_bytecode::Func,
    mut expensive_effect: impl FnMut(u32) -> bool,
) -> bool {
    let block_count = function.blocks.len();
    let effect_blocks: Vec<usize> = function
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(block, instructions)| {
            instructions
                .iter()
                .any(|instruction| {
                    matches!(
                        instruction,
                        lm_bytecode::Instr::Perform { .. }
                            | lm_bytecode::Instr::PerformValue { .. }
                    )
                })
                .then_some(block)
        })
        .collect();
    if effect_blocks.is_empty() {
        return false;
    }
    let mut successors = vec![Vec::new(); block_count];
    let mut predecessors = vec![Vec::new(); block_count];
    for (block, instructions) in function.blocks.iter().enumerate() {
        for instruction in instructions {
            let target = match instruction {
                lm_bytecode::Instr::Jump(target)
                | lm_bytecode::Instr::JumpIfFalse(target)
                | lm_bytecode::Instr::JumpIfTrue(target) => Some(*target as usize),
                _ => None,
            };
            let Some(target) = target.filter(|target| *target < block_count) else {
                continue;
            };
            if !successors[block].contains(&target) {
                successors[block].push(target);
                predecessors[target].push(block);
            }
        }
    }
    let mut visited = vec![false; block_count];
    let mut reverse_visited = vec![false; block_count];
    let mut stack = Vec::new();
    for effect_block in effect_blocks {
        reachable_blocks(effect_block, &successors, &mut visited, &mut stack);
        reachable_blocks(
            effect_block,
            &predecessors,
            &mut reverse_visited,
            &mut stack,
        );
        let members: Vec<usize> = (0..block_count)
            .filter(|block| visited[*block] && reverse_visited[*block])
            .collect();
        let self_loop = successors[effect_block].contains(&effect_block);
        if members.len() == 1 && !self_loop {
            continue;
        }
        let internal_edges = members
            .iter()
            .map(|block| {
                successors[*block]
                    .iter()
                    .filter(|target| visited[**target] && reverse_visited[**target])
                    .count()
            })
            .sum::<usize>();
        if internal_edges != members.len() {
            continue;
        }
        let mut effect_count = 0usize;
        let mut instruction_count = 0usize;
        let mut expensive = false;
        for block in members {
            instruction_count = instruction_count.saturating_add(function.blocks[block].len());
            for instruction in &function.blocks[block] {
                match instruction {
                    lm_bytecode::Instr::Perform { op, .. } => {
                        effect_count = effect_count.saturating_add(1);
                        expensive |= expensive_effect(*op);
                    }
                    lm_bytecode::Instr::PerformValue { .. } => {
                        effect_count = effect_count.saturating_add(1);
                    }
                    _ => {}
                }
            }
        }
        if expensive
            || effect_count != 0
                && instruction_count <= effect_count.saturating_mul(MAX_DENSE_EFFECT_CYCLE_COST)
        {
            return true;
        }
    }
    false
}

fn reachable_blocks(
    start: usize,
    edges: &[Vec<usize>],
    visited: &mut [bool],
    stack: &mut Vec<usize>,
) {
    visited.fill(false);
    stack.clear();
    stack.push(start);
    while let Some(block) = stack.pop() {
        if std::mem::replace(&mut visited[block], true) {
            continue;
        }
        stack.extend(edges[block].iter().copied());
    }
}

fn type_is_candidate(module: &crate::NamespaceRuntime, ty: u32) -> bool {
    module
        .types
        .get(ty as usize)
        .is_some_and(lm_jit::type_has_native_representation)
}

fn table_type_is_candidate(tables: &lm_bytecode::CodeTables, ty: u32) -> bool {
    tables
        .types
        .get(ty as usize)
        .is_some_and(lm_jit::type_has_native_representation)
}

fn function_rejections(
    tables: &lm_bytecode::CodeTables,
    function: &lm_bytecode::Func,
    candidate: bool,
) -> Vec<(String, u32)> {
    if candidate {
        return Vec::new();
    }
    let mut reasons = BTreeMap::<String, u32>::new();
    if !function
        .local_types
        .iter()
        .chain(std::iter::once(&function.ret))
        .all(|ty| table_type_is_candidate(tables, *ty))
    {
        add_reason(&mut reasons, "non-native value type".to_string());
    }
    for instruction in function.blocks.iter().flatten() {
        if let lm_bytecode::Instr::Call(target) | lm_bytecode::Instr::CallG { func: target, .. } =
            instruction
        {
            let boundary_supported = tables.funcs.get(*target as usize).is_some_and(|callee| {
                callee
                    .params
                    .iter()
                    .chain(std::iter::once(&callee.ret))
                    .all(|ty| table_type_is_candidate(tables, *ty))
            });
            if !boundary_supported {
                add_reason(&mut reasons, "direct call boundary type".to_string());
            }
        }
    }
    if reasons.is_empty() {
        add_reason(&mut reasons, "region shape".to_string());
    }
    reasons.into_iter().collect()
}

fn function_treatment_gaps(_function: &lm_bytecode::Func) -> Vec<(String, u32)> {
    Vec::new()
}

fn add_reason(reasons: &mut BTreeMap<String, u32>, reason: String) {
    reasons
        .entry(reason)
        .and_modify(|count| *count = count.saturating_add(1))
        .or_insert(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function(blocks: Vec<Vec<lm_bytecode::Instr>>) -> lm_bytecode::Func {
        lm_bytecode::Func {
            name: "effect-loop".to_string(),
            param_names: Vec::new(),
            type_params: 0,
            effect_params: 0,
            params: Vec::new(),
            param_muts: Vec::new(),
            ret: 0,
            row: Vec::new(),
            captures: Vec::new(),
            local_types: Vec::new(),
            blocks,
        }
    }

    #[test]
    fn code_budget_uses_bytes_and_reopens_after_release() {
        let budget = CodeBudget::new(100);
        assert!(budget.reserve(60));
        let epoch = budget.epoch();
        assert!(!budget.reserve(50));
        budget.release(60);
        assert_ne!(budget.epoch(), epoch);
        assert!(budget.reserve(50));
    }

    #[test]
    fn one_dense_effect_cycle_prefers_the_interpreter() {
        let function = function(vec![vec![
            lm_bytecode::Instr::Perform {
                op: lm_abi::OP_CLOCK_NOW,
                argc: 0,
                reply_ty: 0,
            },
            lm_bytecode::Instr::Pop,
            lm_bytecode::Instr::Jump(0),
        ]]);
        assert!(effect_cycle_prefers_interpreter(&function, |_| false));
    }

    #[test]
    fn nested_work_keeps_a_sparse_effect_cycle_native() {
        let function = function(vec![
            vec![
                lm_bytecode::Instr::Perform {
                    op: lm_abi::OP_CLOCK_NOW,
                    argc: 0,
                    reply_ty: 0,
                },
                lm_bytecode::Instr::Pop,
                lm_bytecode::Instr::Jump(1),
            ],
            vec![
                lm_bytecode::Instr::ConstBool(true),
                lm_bytecode::Instr::JumpIfFalse(0),
                lm_bytecode::Instr::Jump(2),
            ],
            vec![lm_bytecode::Instr::Jump(1)],
        ]);
        assert!(!effect_cycle_prefers_interpreter(&function, |_| false));
    }

    #[test]
    fn one_vm_control_cycle_prefers_the_interpreter() {
        let mut instructions = Vec::new();
        for _ in 0..32 {
            instructions.push(lm_bytecode::Instr::ConstUnit);
            instructions.push(lm_bytecode::Instr::Pop);
        }
        instructions.push(lm_bytecode::Instr::Perform {
            op: lm_abi::OP_VM_DRIVE,
            argc: 0,
            reply_ty: 0,
        });
        instructions.push(lm_bytecode::Instr::Pop);
        instructions.push(lm_bytecode::Instr::Jump(0));
        assert!(effect_cycle_prefers_interpreter(
            &function(vec![instructions]),
            |op| op == lm_abi::OP_VM_DRIVE
        ));
    }
}
