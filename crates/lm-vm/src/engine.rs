//! Host-selected execution policy and clock-free engine counters.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

/// The host-selected machine execution policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum EngineMode {
    /// Execute all code through the interpreter.
    #[default]
    Interpreter = 0,
    /// Use native code when an eligible region is available.
    Auto = 1,
    /// Expose every eligible native fallback through counters.
    Native = 2,
}

impl EngineMode {
    fn from_u8(value: u8) -> EngineMode {
        match value {
            1 => EngineMode::Auto,
            2 => EngineMode::Native,
            _ => EngineMode::Interpreter,
        }
    }
}

/// Clock-free execution-engine counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineMetrics {
    pub compilation_attempts: u64,
    pub compiled_regions: u64,
    pub compiled_segments: u64,
    pub compiled_call_sites: u64,
    pub compiled_heap_read_sites: u64,
    pub compiled_heap_write_sites: u64,
    pub compiled_allocation_sites: u64,
    pub compiled_effect_sites: u64,
    pub native_entry_attempts: u64,
    pub guarded_values: u64,
    pub guard_failures: u64,
    pub native_entries: u64,
    pub native_retired_instructions: u64,
    pub materializations: u64,
    pub native_continuation_suspends: u64,
    pub native_continuation_resumes: u64,
    pub native_continuation_materializations: u64,
    pub native_activation_grows: u64,
    pub unproductive_native_demotions: u64,
    pub native_fault_exits: u64,
    pub native_allocation_exits: u64,
    pub native_allocations: u64,
    pub native_effect_exits: u64,
    pub unsupported_region_fallbacks: u64,
    pub missing_entry_fallbacks: u64,
    pub backend_unavailable_fallbacks: u64,
}

#[derive(Debug, Default)]
struct EngineCounters {
    native_entry_attempts: AtomicU64,
    guarded_values: AtomicU64,
    guard_failures: AtomicU64,
    native_entries: AtomicU64,
    native_retired_instructions: AtomicU64,
    materializations: AtomicU64,
    native_continuation_suspends: AtomicU64,
    native_continuation_resumes: AtomicU64,
    native_continuation_materializations: AtomicU64,
    native_activation_grows: AtomicU64,
    unproductive_native_demotions: AtomicU64,
    native_fault_exits: AtomicU64,
    native_allocation_exits: AtomicU64,
    native_allocations: AtomicU64,
    native_effect_exits: AtomicU64,
    unsupported_region_fallbacks: AtomicU64,
    missing_entry_fallbacks: AtomicU64,
    backend_unavailable_fallbacks: AtomicU64,
}

impl EngineCounters {
    fn read(&self, compiler: lm_jit::CompilerMetrics) -> EngineMetrics {
        let read = |value: &AtomicU64| value.load(Ordering::Relaxed);
        EngineMetrics {
            compilation_attempts: compiler.compilation_attempts,
            compiled_regions: compiler.compiled_regions,
            compiled_segments: compiler.compiled_segments,
            compiled_call_sites: compiler.compiled_call_sites,
            compiled_heap_read_sites: compiler.compiled_heap_read_sites,
            compiled_heap_write_sites: compiler.compiled_heap_write_sites,
            compiled_allocation_sites: compiler.compiled_allocation_sites,
            compiled_effect_sites: compiler.compiled_effect_sites,
            native_entry_attempts: read(&self.native_entry_attempts),
            guarded_values: read(&self.guarded_values),
            guard_failures: read(&self.guard_failures),
            native_entries: read(&self.native_entries),
            native_retired_instructions: read(&self.native_retired_instructions),
            materializations: read(&self.materializations),
            native_continuation_suspends: read(&self.native_continuation_suspends),
            native_continuation_resumes: read(&self.native_continuation_resumes),
            native_continuation_materializations: read(&self.native_continuation_materializations),
            native_activation_grows: read(&self.native_activation_grows),
            unproductive_native_demotions: read(&self.unproductive_native_demotions),
            native_fault_exits: read(&self.native_fault_exits),
            native_allocation_exits: read(&self.native_allocation_exits),
            native_allocations: read(&self.native_allocations),
            native_effect_exits: read(&self.native_effect_exits),
            unsupported_region_fallbacks: read(&self.unsupported_region_fallbacks),
            missing_entry_fallbacks: read(&self.missing_entry_fallbacks),
            backend_unavailable_fallbacks: read(&self.backend_unavailable_fallbacks),
        }
    }

    fn reset(&self) {
        let reset = |value: &AtomicU64| value.store(0, Ordering::Relaxed);
        reset(&self.native_entry_attempts);
        reset(&self.guarded_values);
        reset(&self.guard_failures);
        reset(&self.native_entries);
        reset(&self.native_retired_instructions);
        reset(&self.materializations);
        reset(&self.native_continuation_suspends);
        reset(&self.native_continuation_resumes);
        reset(&self.native_continuation_materializations);
        reset(&self.native_activation_grows);
        reset(&self.unproductive_native_demotions);
        reset(&self.native_fault_exits);
        reset(&self.native_allocation_exits);
        reset(&self.native_allocations);
        reset(&self.native_effect_exits);
        reset(&self.unsupported_region_fallbacks);
        reset(&self.missing_entry_fallbacks);
        reset(&self.backend_unavailable_fallbacks);
    }

    fn add(&self, values: &EngineMetrics) {
        let add = |target: &AtomicU64, value: u64| {
            if value != 0 {
                target.fetch_add(value, Ordering::Relaxed);
            }
        };
        add(&self.native_entry_attempts, values.native_entry_attempts);
        add(&self.guarded_values, values.guarded_values);
        add(&self.guard_failures, values.guard_failures);
        add(&self.native_entries, values.native_entries);
        add(
            &self.native_retired_instructions,
            values.native_retired_instructions,
        );
        add(&self.materializations, values.materializations);
        add(
            &self.native_continuation_suspends,
            values.native_continuation_suspends,
        );
        add(
            &self.native_continuation_resumes,
            values.native_continuation_resumes,
        );
        add(
            &self.native_continuation_materializations,
            values.native_continuation_materializations,
        );
        add(
            &self.native_activation_grows,
            values.native_activation_grows,
        );
        add(
            &self.unproductive_native_demotions,
            values.unproductive_native_demotions,
        );
        add(&self.native_fault_exits, values.native_fault_exits);
        add(
            &self.native_allocation_exits,
            values.native_allocation_exits,
        );
        add(&self.native_allocations, values.native_allocations);
        add(&self.native_effect_exits, values.native_effect_exits);
        add(
            &self.unsupported_region_fallbacks,
            values.unsupported_region_fallbacks,
        );
        add(
            &self.missing_entry_fallbacks,
            values.missing_entry_fallbacks,
        );
        add(
            &self.backend_unavailable_fallbacks,
            values.backend_unavailable_fallbacks,
        );
    }
}

/// Counters collected during one engine turn.
pub(crate) struct EngineTurnMetrics<'a> {
    counters: &'a EngineCounters,
    values: EngineMetrics,
    sample_productivity: bool,
}

impl EngineTurnMetrics<'_> {
    pub(crate) fn sample_productivity(&self) -> bool {
        self.sample_productivity
    }

    pub(crate) fn note_backend_unavailable(&mut self) {
        self.values.backend_unavailable_fallbacks += 1;
    }

    pub(crate) fn note_native_entry_attempt(&mut self) {
        self.values.native_entry_attempts += 1;
    }

    pub(crate) fn note_guarded_values(&mut self, values: u64) {
        self.values.guarded_values = self.values.guarded_values.saturating_add(values);
    }

    pub(crate) fn note_guard_failure(&mut self, values: u64) {
        self.note_guarded_values(values);
        self.values.guard_failures += 1;
    }

    pub(crate) fn note_native_entry(&mut self) {
        self.values.native_entries += 1;
    }

    pub(crate) fn note_native_retired(&mut self, instructions: u64) {
        self.values.native_retired_instructions = self
            .values
            .native_retired_instructions
            .saturating_add(instructions);
    }

    pub(crate) fn note_materialization(&mut self) {
        self.values.materializations += 1;
    }

    pub(crate) fn note_native_continuation_suspend(&mut self) {
        self.values.native_continuation_suspends += 1;
    }

    pub(crate) fn note_native_continuation_resume(&mut self) {
        self.values.native_continuation_resumes += 1;
    }

    pub(crate) fn note_native_continuation_materialization(&mut self) {
        self.values.native_continuation_materializations += 1;
    }

    pub(crate) fn note_native_activation_grow(&mut self) {
        self.values.native_activation_grows += 1;
    }

    pub(crate) fn note_unproductive_native_demotion(&mut self) {
        self.values.unproductive_native_demotions += 1;
    }

    pub(crate) fn note_native_fault_exit(&mut self) {
        self.values.native_fault_exits += 1;
    }

    pub(crate) fn note_native_allocation_exit(&mut self) {
        self.values.native_allocation_exits += 1;
    }

    pub(crate) fn note_native_allocations(&mut self, allocations: u64) {
        self.values.native_allocations = self.values.native_allocations.saturating_add(allocations);
    }

    pub(crate) fn note_native_effect_exit(&mut self) {
        self.values.native_effect_exits += 1;
    }

    pub(crate) fn note_unsupported_region_fallback(&mut self) {
        self.values.unsupported_region_fallbacks += 1;
    }

    pub(crate) fn note_missing_entry_fallback(&mut self) {
        self.values.missing_entry_fallbacks += 1;
    }
}

impl Drop for EngineTurnMetrics<'_> {
    fn drop(&mut self) {
        self.counters.add(&self.values);
    }
}

/// One host-owned execution engine and compiled-code cache.
#[derive(Debug)]
pub struct Engine {
    mode: AtomicU8,
    counters: EngineCounters,
    jit: crate::jit::JitEngine,
}

impl Engine {
    /// Create one engine with the selected policy.
    pub fn new(mode: EngineMode) -> Engine {
        Engine {
            mode: AtomicU8::new(mode as u8),
            counters: EngineCounters::default(),
            jit: crate::jit::JitEngine::default(),
        }
    }

    /// Return the selected execution policy.
    pub fn mode(&self) -> EngineMode {
        EngineMode::from_u8(self.mode.load(Ordering::Relaxed))
    }

    /// Select the execution policy for later turns.
    pub fn set_mode(&self, mode: EngineMode) {
        self.mode.store(mode as u8, Ordering::Relaxed);
    }

    /// Return the current clock-free counters.
    pub fn metrics(&self) -> EngineMetrics {
        self.counters.read(self.jit.metrics())
    }

    /// Reset every clock-free counter.
    pub fn reset_metrics(&self) {
        self.counters.reset();
        self.jit.reset_metrics();
    }

    pub(crate) fn turn_metrics(&self) -> EngineTurnMetrics<'_> {
        EngineTurnMetrics {
            counters: &self.counters,
            values: EngineMetrics::default(),
            sample_productivity: self.mode() == EngineMode::Auto,
        }
    }

    pub(crate) fn execute_native(
        &self,
        machine: &mut crate::machine::Machine,
        module: &crate::NamespaceRuntime,
        native: &crate::jit::NativeCodeState,
        scratch: &mut crate::jit::NativeScratch,
        metrics: &mut EngineTurnMetrics<'_>,
        instruction_limit: u32,
    ) -> crate::jit::NativeAttempt {
        if metrics.sample_productivity()
            && !machine.has_native_continuation()
            && machine
                .vm
                .frames
                .last()
                .is_some_and(|frame| !native.ready_for_auto(frame.func))
        {
            return crate::jit::NativeAttempt::Fallback;
        }
        self.jit
            .execute(machine, module, native, scratch, metrics, instruction_limit)
    }

    pub(crate) fn native_code(
        &self,
        module: &crate::NamespaceRuntime,
    ) -> crate::jit::NativeCodeState {
        self.jit.native_code(module)
    }

    pub(crate) fn materialize_native_state(
        &self,
        machine: &mut crate::machine::Machine,
    ) -> Result<bool, crate::FaultCode> {
        let mut metrics = self.turn_metrics();
        match crate::jit::materialize_native_continuation(machine) {
            Ok(materialized) => {
                if materialized {
                    metrics.note_materialization();
                    metrics.note_native_continuation_materialization();
                }
                Ok(materialized)
            }
            Err(()) => {
                metrics.note_backend_unavailable();
                metrics.note_native_fault_exit();
                Err(crate::FaultCode::MalformedState)
            }
        }
    }
}

impl Default for Engine {
    fn default() -> Engine {
        Engine::new(EngineMode::Interpreter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_mode_and_metrics_are_thread_safe() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Engine>();
        let engine = Engine::default();
        engine.set_mode(EngineMode::Native);
        {
            let mut metrics = engine.turn_metrics();
            metrics.note_backend_unavailable();
        }
        assert_eq!(engine.mode(), EngineMode::Native);
        assert_eq!(engine.metrics().backend_unavailable_fallbacks, 1);
        engine.reset_metrics();
        assert_eq!(engine.metrics(), EngineMetrics::default());
    }
}
