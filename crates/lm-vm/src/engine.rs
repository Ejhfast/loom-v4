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
    pub native_entry_attempts: u64,
    pub guarded_values: u64,
    pub guard_failures: u64,
    pub native_entries: u64,
    pub native_retired_instructions: u64,
    pub materializations: u64,
    pub native_fault_exits: u64,
    pub unsupported_region_fallbacks: u64,
    pub missing_entry_fallbacks: u64,
    pub backend_unavailable_fallbacks: u64,
}

#[derive(Debug, Default)]
struct EngineCounters {
    compilation_attempts: AtomicU64,
    compiled_regions: AtomicU64,
    compiled_segments: AtomicU64,
    native_entry_attempts: AtomicU64,
    guarded_values: AtomicU64,
    guard_failures: AtomicU64,
    native_entries: AtomicU64,
    native_retired_instructions: AtomicU64,
    materializations: AtomicU64,
    native_fault_exits: AtomicU64,
    unsupported_region_fallbacks: AtomicU64,
    missing_entry_fallbacks: AtomicU64,
    backend_unavailable_fallbacks: AtomicU64,
}

impl EngineCounters {
    fn read(&self) -> EngineMetrics {
        let read = |value: &AtomicU64| value.load(Ordering::Relaxed);
        EngineMetrics {
            compilation_attempts: read(&self.compilation_attempts),
            compiled_regions: read(&self.compiled_regions),
            compiled_segments: read(&self.compiled_segments),
            native_entry_attempts: read(&self.native_entry_attempts),
            guarded_values: read(&self.guarded_values),
            guard_failures: read(&self.guard_failures),
            native_entries: read(&self.native_entries),
            native_retired_instructions: read(&self.native_retired_instructions),
            materializations: read(&self.materializations),
            native_fault_exits: read(&self.native_fault_exits),
            unsupported_region_fallbacks: read(&self.unsupported_region_fallbacks),
            missing_entry_fallbacks: read(&self.missing_entry_fallbacks),
            backend_unavailable_fallbacks: read(&self.backend_unavailable_fallbacks),
        }
    }

    fn reset(&self) {
        let reset = |value: &AtomicU64| value.store(0, Ordering::Relaxed);
        reset(&self.compilation_attempts);
        reset(&self.compiled_regions);
        reset(&self.compiled_segments);
        reset(&self.native_entry_attempts);
        reset(&self.guarded_values);
        reset(&self.guard_failures);
        reset(&self.native_entries);
        reset(&self.native_retired_instructions);
        reset(&self.materializations);
        reset(&self.native_fault_exits);
        reset(&self.unsupported_region_fallbacks);
        reset(&self.missing_entry_fallbacks);
        reset(&self.backend_unavailable_fallbacks);
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
        self.counters.read()
    }

    /// Reset every clock-free counter.
    pub fn reset_metrics(&self) {
        self.counters.reset();
    }

    pub(crate) fn note_backend_unavailable(&self) {
        self.counters
            .backend_unavailable_fallbacks
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn execute_native(
        &self,
        machine: &mut crate::machine::Machine,
        module: &crate::NamespaceRuntime,
        instruction_limit: u32,
    ) -> crate::jit::NativeAttempt {
        self.jit.execute(self, machine, module, instruction_limit)
    }

    pub(crate) fn note_compilation_attempt(&self) {
        self.counters
            .compilation_attempts
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn note_compiled_region(&self, segments: u64) {
        self.counters
            .compiled_regions
            .fetch_add(1, Ordering::Relaxed);
        self.counters
            .compiled_segments
            .fetch_add(segments, Ordering::Relaxed);
    }

    pub(crate) fn note_native_entry_attempt(&self) {
        self.counters
            .native_entry_attempts
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn note_guarded_values(&self, values: u64) {
        self.counters
            .guarded_values
            .fetch_add(values, Ordering::Relaxed);
    }

    pub(crate) fn note_guard_failure(&self, values: u64) {
        self.note_guarded_values(values);
        self.counters.guard_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn note_native_entry(&self) {
        self.counters.native_entries.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn note_native_retired(&self, instructions: u64) {
        self.counters
            .native_retired_instructions
            .fetch_add(instructions, Ordering::Relaxed);
    }

    pub(crate) fn note_materialization(&self) {
        self.counters
            .materializations
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn note_native_fault_exit(&self) {
        self.counters
            .native_fault_exits
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn note_unsupported_region_fallback(&self) {
        self.counters
            .unsupported_region_fallbacks
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn note_missing_entry_fallback(&self) {
        self.counters
            .missing_entry_fallbacks
            .fetch_add(1, Ordering::Relaxed);
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
        engine.note_backend_unavailable();
        assert_eq!(engine.mode(), EngineMode::Native);
        assert_eq!(engine.metrics().backend_unavailable_fallbacks, 1);
        engine.reset_metrics();
        assert_eq!(engine.metrics(), EngineMetrics::default());
    }
}
