//! Native region cache for one arena layout.

use lm_jit::{Failure, FunctionInput};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

const MAX_COMPILED_REGIONS: usize = 256;
const AUTO_COMPILE_WORK: u64 = 100_000;
const TIER_COMPILED: u64 = u64::MAX - 1;
const TIER_DENIED: u64 = u64::MAX;
const ENTRY_SAMPLE_COUNT: u32 = 8;
const MIN_ENTRY_RETIRED: u64 = 32;
const PRODUCTIVITY_PROVEN: u32 = u32::MAX - 1;
const PRODUCTIVITY_DENIED: u32 = u32::MAX;

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
    candidates: Arc<Vec<u64>>,
    compiled: Arc<AtomicUsize>,
}

impl NativeCodeState {
    pub(crate) fn new(module: &crate::NamespaceRuntime) -> NativeCodeState {
        let mut revision = NativeCodeRevision {
            slots: lm_bytecode::CodeTable::default(),
            entries: Arc::new(Vec::new()),
            candidates: Arc::new(Vec::new()),
            compiled: Arc::new(AtomicUsize::new(0)),
        };
        while revision.slots.len() < module.funcs.len() {
            revision.push_slot(module, revision.slots.len());
        }
        NativeCodeState(Arc::new(revision))
    }

    pub(crate) fn extend(&mut self, module: &crate::NamespaceRuntime) {
        if self.0.slots.len() >= module.funcs.len() {
            return;
        }
        let mut revision = self.0.as_ref().clone();
        while revision.slots.len() < module.funcs.len() {
            revision.push_slot(module, revision.slots.len());
        }
        self.0 = Arc::new(revision);
    }

    pub(super) fn slot(&self, function: u32) -> Option<&Arc<NativeSlot>> {
        self.0.slots.get(function as usize)
    }

    pub(super) fn entries(&self) -> &[usize] {
        self.0.entries.as_slice()
    }

    pub(super) fn compiled_count(&self) -> &AtomicUsize {
        self.0.compiled.as_ref()
    }

    pub(crate) fn enter_frame(&self, function: u32, work_scale: u32) -> TierDecision {
        if !self.is_candidate(function) {
            return TierDecision {
                enter_native: false,
            };
        }
        self.slot(function).map_or(
            TierDecision {
                enter_native: false,
            },
            |slot| slot.enter_frame(work_scale),
        )
    }

    pub(crate) fn note_event(&self, function: u32, work_scale: u32) -> bool {
        self.is_candidate(function)
            && self
                .slot(function)
                .is_some_and(|slot| slot.note_event(work_scale))
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

    pub(crate) fn call_target_is_denied(&self, function: u32) -> bool {
        self.slot(function).is_none_or(|slot| slot.is_denied())
    }
}

impl NativeCodeRevision {
    fn push_slot(&mut self, module: &crate::NamespaceRuntime, function: usize) {
        let definition = &module.funcs[function];
        let candidate = function_is_candidate(module, definition);
        let word = function / 64;
        let bit = function % 64;
        if Arc::make_mut(&mut self.candidates).len() <= word {
            Arc::make_mut(&mut self.candidates).resize(word + 1, 0);
        }
        if candidate {
            Arc::make_mut(&mut self.candidates)[word] |= 1_u64 << bit;
        }
        let slot = Arc::new(NativeSlot::new(definition, candidate));
        Arc::make_mut(&mut self.entries).push(Arc::as_ptr(&slot.entry) as usize);
        self.slots.push(slot);
    }
}

pub(super) struct NativeSlot {
    verdict: OnceLock<Result<Arc<lm_jit::CompiledRegion>, Failure>>,
    entry: Arc<lm_jit::NativeEntryCell>,
    event_weight: u32,
    tier: AtomicU64,
    productivity: AtomicU32,
}

impl NativeSlot {
    fn new(function: &lm_bytecode::Func, candidate: bool) -> NativeSlot {
        let event_weight = if candidate {
            function
                .blocks
                .iter()
                .map(Vec::len)
                .sum::<usize>()
                .clamp(1, u32::MAX as usize) as u32
        } else {
            0
        };
        NativeSlot {
            verdict: OnceLock::new(),
            entry: Arc::new(lm_jit::NativeEntryCell::default()),
            event_weight,
            tier: AtomicU64::new(if candidate { 0 } else { TIER_DENIED }),
            productivity: AtomicU32::new(0),
        }
    }

    pub(super) fn region<'a, F>(
        &self,
        compiler: &lm_jit::JitEngine,
        compiled: &AtomicUsize,
        input: F,
    ) -> Result<Arc<lm_jit::CompiledRegion>, Failure>
    where
        F: FnOnce() -> Result<FunctionInput<'a>, Failure>,
    {
        let result = self
            .verdict
            .get_or_init(|| {
                compiled
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                        (count < MAX_COMPILED_REGIONS).then_some(count + 1)
                    })
                    .map_err(|_| Failure::BackendUnavailable)?;
                let result = input()
                    .and_then(|input| compiler.compile(input))
                    .and_then(|region| self.entry.publish(&region).map(|()| region));
                if result.is_err() {
                    compiled.fetch_sub(1, Ordering::Relaxed);
                }
                result
            })
            .clone();
        self.tier.store(
            if result.is_ok() {
                TIER_COMPILED
            } else {
                TIER_DENIED
            },
            Ordering::Release,
        );
        result
    }

    pub(super) fn compiled(&self) -> Option<Arc<lm_jit::CompiledRegion>> {
        self.verdict.get()?.as_ref().ok().cloned()
    }

    pub(super) fn ready_for_auto(&self) -> bool {
        self.productivity.load(Ordering::Relaxed) != PRODUCTIVITY_DENIED
            && matches!(
                self.tier.load(Ordering::Acquire),
                AUTO_COMPILE_WORK | TIER_COMPILED
            )
    }

    fn enter_frame(&self, work_scale: u32) -> TierDecision {
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
        self.productivity.load(Ordering::Relaxed) != PRODUCTIVITY_DENIED
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

    fn is_denied(&self) -> bool {
        self.tier.load(Ordering::Acquire) == TIER_DENIED
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
            lm_bytecode::Instr::Call(target) => {
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

fn type_is_candidate(module: &crate::NamespaceRuntime, ty: u32) -> bool {
    matches!(
        module.types.get(ty as usize),
        Some(
            lm_bytecode::BcType::Unit
                | lm_bytecode::BcType::Bool
                | lm_bytecode::BcType::Int
                | lm_bytecode::BcType::Float
                | lm_bytecode::BcType::Class(_)
                | lm_bytecode::BcType::Inst(_, _)
                | lm_bytecode::BcType::List(_)
                | lm_bytecode::BcType::Tuple(_)
                | lm_bytecode::BcType::Op(_, _)
        )
    )
}
