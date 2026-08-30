//! Native region cache for one arena layout.

use lm_jit::{Failure, FunctionInput};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

const MAX_COMPILED_REGIONS: usize = 256;

#[derive(Clone)]
pub(crate) struct NativeCodeState(Arc<NativeCodeRevision>);

#[derive(Clone)]
struct NativeCodeRevision {
    slots: lm_bytecode::CodeTable<Arc<NativeSlot>>,
    entries: Arc<Vec<usize>>,
    compiled: Arc<AtomicUsize>,
}

impl NativeCodeState {
    pub(crate) fn new(functions: usize) -> NativeCodeState {
        let mut revision = NativeCodeRevision {
            slots: lm_bytecode::CodeTable::default(),
            entries: Arc::new(Vec::new()),
            compiled: Arc::new(AtomicUsize::new(0)),
        };
        while revision.slots.len() < functions {
            revision.push_slot();
        }
        NativeCodeState(Arc::new(revision))
    }

    pub(crate) fn extend(&mut self, functions: usize) {
        if self.0.slots.len() >= functions {
            return;
        }
        let mut revision = self.0.as_ref().clone();
        while revision.slots.len() < functions {
            revision.push_slot();
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
}

impl NativeCodeRevision {
    fn push_slot(&mut self) {
        let slot = Arc::new(NativeSlot::default());
        Arc::make_mut(&mut self.entries).push(Arc::as_ptr(&slot.entry) as usize);
        self.slots.push(slot);
    }
}

#[derive(Default)]
pub(super) struct NativeSlot {
    verdict: OnceLock<Result<Arc<lm_jit::CompiledRegion>, Failure>>,
    entry: Arc<lm_jit::NativeEntryCell>,
}

impl NativeSlot {
    pub(super) fn region<'a, F>(
        &self,
        compiler: &lm_jit::JitEngine,
        compiled: &AtomicUsize,
        input: F,
    ) -> Result<Arc<lm_jit::CompiledRegion>, Failure>
    where
        F: FnOnce() -> Result<FunctionInput<'a>, Failure>,
    {
        self.verdict
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
            .clone()
    }

    pub(super) fn compiled(&self) -> Option<Arc<lm_jit::CompiledRegion>> {
        self.verdict.get()?.as_ref().ok().cloned()
    }
}
