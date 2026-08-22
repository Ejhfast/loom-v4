//! The `run` allocation gate: the dedicated internal loop allocates
//! no event, request, or other Rust object per instruction.
//!
//! The test counts Rust heap allocations around a pure 700,000
//! instruction guest loop and requires a small constant total. The
//! counting allocator is test-only and is the one `unsafe` block in
//! the repository: it only forwards to the system allocator and
//! increments a counter.

use lm_bytecode::{BcType, Func, Instr, Module};
use lm_value::Value;
use lm_vm::{Outcome, Vm, VmConfig};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;

// SAFETY: the allocator forwards every call to the system allocator
// unchanged; the counter update has no other effect.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// A pure counting loop: about seven instructions per iteration over
/// 100,000 iterations.
fn loop_module() -> Module {
    Module {
        strings: vec![],
        bytes: vec![],
        types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
        selectors: vec![],
        apps: vec![],
        interfaces: vec![],
        conformances: vec![],
        class_bounds: vec![],
        func_bounds: vec![vec![]],
        classes: vec![],
        funcs: vec![Func {
            name: "main".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: 2,
            row: vec![],
            captures: vec![],
            local_types: vec![2],
            blocks: vec![
                vec![Instr::ConstInt(0), Instr::StoreLocal(0), Instr::Jump(1)],
                vec![
                    Instr::LoadLocal(0),
                    Instr::ConstInt(100_000),
                    Instr::LtInt,
                    Instr::JumpIfFalse(3),
                    Instr::Jump(2),
                ],
                vec![
                    Instr::LoadLocal(0),
                    Instr::ConstInt(1),
                    Instr::Add,
                    Instr::StoreLocal(0),
                    Instr::Jump(1),
                ],
                vec![Instr::LoadLocal(0), Instr::Return],
            ],
        }],
        imports: vec![],
        slots: vec![],
        core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
        entry: 0,
        exports: vec![],
        bindings: vec![],
        debug: Vec::new(),
    }
}

#[test]
fn run_allocates_no_event_per_instruction() {
    let loaded = lm_vm::load(loop_module()).expect("the loop module verifies");
    let mut vm = Vm::new(&loaded, VmConfig::default());
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let outcome = vm.run();
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(outcome, Outcome::Done(Value::Int(100_000)));
    let allocations = after - before;
    // About 700,000 instructions retired. A per-instruction event
    // would show up as hundreds of thousands of allocations; the
    // fixed setup cost stays far below the bound.
    assert!(
        allocations < 100,
        "run allocated {allocations} times over ~700,000 instructions"
    );
}
