# Guarded JIT compilation

Status: Stages 0 through 4 are implemented for the first native execution experiment.

This sidecar refines the executor contract in the multi-threaded scheduler sidecar.

The first milestone tests whether native execution preserves Loom's reified machine state.

The milestone does not define the final compiled surface.

## 1. Goals

The first implementation has these goals:

- compile verified scalar control flow with Cranelift;
- preserve canonical `VmState` at every engine boundary;
- preserve exact LMBC fuel accounting;
- preserve deterministic interpreter behavior;
- support fresh, captured, and externally restored state through guarded entry;
- measure native entry guard cost;
- measure cold compilation and warm execution separately;
- keep engine choice under host control.

The experiment succeeds when supported warm loops improve by more than two times.

A smaller gain requires an overhead investigation before surface expansion.

## 2. Non-goals

The first implementation does not compile these operations:

- function calls;
- closures or captures;
- generic environments;
- heap references;
- allocation;
- fields or collections;
- effects;
- native LMBC instructions;
- extended LMBC instructions.

The implementation does not replace `restored_any` during this milestone.

The implementation does not validate all values in an external snapshot.

The implementation does not store native code in artifacts or snapshots.

The implementation does not expose a guest engine-selection operation.

## 3. Independent proofs

Every native execution needs verified code.

Publication verifies every executable function.

Native compilation rechecks the target function to obtain block-entry states.

It also checks the target unit tables.

It does not check other function bodies.

The JIT also needs valid representations for the values that its region reads.

The first implementation proves that fact locally at native entry.

It does not attach a global type-integrity property to the world.

`restored_any` continues to select existing boundary checks.

It does not select the execution engine.

## 4. Terms

An **engine** executes one bounded machine turn.

A **region** is one supported function control-flow graph.

A **segment** is a fixed-cost path between fuel checkpoints.

An **entry point** is a supported LMBC program position for one region.

An **entry plan** lists the live scalar values required by one entry point.

A **materialization map** describes canonical machine state at one native exit.

A **guard** checks one required `Value` representation without changing machine state.

## 5. Engine policy

The host selects one engine policy:

```rust
pub enum EngineMode {
    Interpreter,
    Auto,
    Native,
}
```

`Interpreter` executes no native code.

`Auto` compiles an eligible region at its first entry.

It uses interpreter fallback for an ineligible region.

`Native` exposes every eligibility failure through metrics and test assertions.

Engine policy is not part of `VmConfig`.

Engine policy is not guest state.

Artifacts and snapshots never contain engine policy.

Engine choice cannot change a guest result, fault, trace, or fuel charge.

## 6. Executor boundary

Deterministic execution borrows machine state.

Parallel execution owns an `ExecutionLease`.

The implementation keeps these ownership forms.

Both forms call one internal engine operation:

```text
run_engine_turn(machine, code, environments, slots, limits, engine)
    -> engine result
```

The operation receives no mutable `World` pointer.

Compiled code stores no worker identifier.

Compiled code uses one immutable function version.

The parallel lease pins that version through its existing execution code.

The deterministic path reads the same immutable execution code.

## 7. Guarded native entry

The entry plan comes from verifier-derived program-point metadata.

The plan names only values that the native region can read.

The guard process follows this order:

1. Read every required local and operand.
2. Match each value against its scalar representation.
3. Copy each scalar into the native activation.
4. Enter native code after every match succeeds.

The guard process changes no machine state.

A failed guard resumes the interpreter from the original program position.

A failed guard does not produce a guest fault.

A successful guard proves only the active region inputs.

It does not prove dormant locals, heap values, mailboxes, or other machines.

External snapshots can therefore enter supported native regions.

Native operations preserve the scalar types established by the entry plan.

Native loop backedges do not repeat entry guards.

## 8. Verifier metadata

The verifier already computes local and operand types at each block entry.

The JIT consumes that result through a public immutable metadata type.

The metadata contains no verifier implementation state.

The metadata states these facts for each reachable block:

- initialized local types;
- operand-stack types;
- block reachability.

The JIT does not maintain a second type checker.

The first implementation recomputes one function's metadata during cold compilation.

Measurement decides whether publication retains this metadata later.

## 9. Region normalization

Conditional jumps split one LMBC block into fixed-cost segments.

Every segment has one known LMBC instruction cost.

Every segment ends at one of these points:

- a control-flow transfer;
- a native exit;
- a fault exit;
- a return;
- a fuel checkpoint.

The native function can continue across segment edges and loop backedges.

It does not return to Rust after every segment.

A restored program can name an instruction inside an LMBC block.

The interpreter runs until it reaches a supported native entry point.

## 10. Initial compiled surface

The first region has one monomorphic frame and an empty type environment.

It has no active closure or callback.

The first scalar types are `Int`, `Bool`, `Float`, and `Unit`.

The first supported instructions are:

- scalar constants;
- `LoadLocal`;
- `StoreLocal`;
- `Pop`;
- checked integer add, subtract, multiply, and negate;
- integer comparisons;
- Boolean equality and negation;
- float add, subtract, multiply, divide, negate, and comparisons;
- `Jump`;
- `JumpIfFalse`;
- `JumpIfTrue`;
- `Return`.

Division and remainder follow after exact fault exits pass their tests.

An unsupported instruction makes the complete function ineligible.

## 11. Native ABI

Generated code does not depend on Rust's `Value` layout.

`Value` has no stable foreign-function ABI.

The runtime uses an explicit native function ABI:

```rust
#[repr(C)]
struct NativeExit {
    retired: u64,
    kind: u32,
    block: u32,
    instruction: u32,
    stack_len: u32,
    result: u64,
}

type NativeFunction = unsafe extern "C" fn(
    locals: *mut u64,
    dirty: *mut u8,
    operands: *mut u64,
    fuel: u64,
    entry: u32,
    exit: *mut NativeExit,
);
```

The ABI stores scalar payloads without Rust enum tags.

Boolean payloads use zero and one.

Float payloads use canonical binary64 bits.

Rust converts `Value` instances before native entry.

Rust reconstructs canonical `Value` instances after native exit.

Native registers and stack slots are execution caches only.

## 12. Canonical state

`Frame`, `VmState.locals`, and `VmState.operands` remain canonical state.

Native code materializes all changed state before returning to Rust.

Each exit records the next LMBC program position.

Fault exits record the post-increment instruction position.

Fault exits reproduce the interpreter's operand consumption.

Return can initially exit before the interpreter executes `Return`.

That choice must preserve the exact retired instruction count.

## 13. Fuel

Fuel remains measured in retired LMBC instructions.

Every segment has one fixed LMBC cost.

Native code checks available fuel before entering a segment.

Insufficient fuel exits before that segment changes state.

Native code can continue through loop backedges while fuel remains.

A faulting instruction charges only its executed LMBC prefix.

Deterministic and native turns return the same state for every fuel limit.

Parallel recall occurs at a native fuel checkpoint.

The first implementation can use every normalized segment as a recall checkpoint.

## 14. Faults

The first implementation supports exact integer overflow exits.

Later integer division adds divide-by-zero and signed-overflow exits.

Each fault exit records these values:

- the Loom fault code;
- the exact retired count;
- the post-increment program position;
- the exact local and operand state.

Cranelift traps do not represent Loom guest faults.

Generated code returns explicit fault records to Rust.

## 15. Compiled-code ownership

One host engine owns the compiled-region cache.

Each compiled region owns one finalized Cranelift `JITModule`.

The implementation pins Cranelift 0.129.2 for Rust 1.91 compatibility.

The cache serializes compilation for one missing region.

Published native functions are immutable.

Workers can call published functions concurrently.

The engine owner outlives every published function pointer.

The cache key contains the exact function version and backend configuration.

The cache never keys by source name alone.

The first cache limits region count, instructions, locals, and operand depth.

An exhausted cache keeps interpreter execution available.

### 15.1 Crate boundary

`lm-jit` owns region analysis, Cranelift, the native ABI, and executable memory.

`lm-vm` owns engine policy, entry guards, canonical state, and exit materialization.

`lm-jit` depends only on lower bytecode, verifier, ABI, and value crates.

`lm-jit` never depends on `lm-vm`.

The native backend never reads `Machine`, `VmState`, or Rust `Value` storage.

## 16. Metrics

The runtime records clock-free counters:

- native compilation attempts;
- compiled regions;
- compiled segments;
- native entry attempts;
- guarded values;
- guard failures;
- native entries;
- native-retired LMBC instructions;
- materializations;
- native fault exits;
- fallbacks by reason.

The benchmark harness measures wall time outside pure runtime crates.

## 17. Correctness tests

Each supported semantic test compares `Interpreter` and forced `Native` policies.

Separate tests cover `Auto` fallback.

Forced `Native` tests reject an unexpected eligible-region fallback.

### 17.1 Segment comparison

Fuel-limited differential tests force materialization at segment boundaries.

The tests compare complete native and interpreter machine states.

### 17.2 Fuel sweep

Tests sweep every fuel limit around each segment cost.

They compare these values:

- stop reason;
- retired instructions;
- remaining fuel;
- frame program position;
- locals;
- operands;
- terminal state;
- fault state.

### 17.3 Engine switching

Tests alternate interpreter and native turns on one machine.

Tests capture after native execution and resume through the interpreter.

Tests capture after interpreter execution and resume through native code.

### 17.4 External state

External snapshots can enter a native region after successful guards.

A malformed required scalar causes interpreter fallback without state mutation.

A malformed dormant value does not block an unrelated native region.

### 17.5 Parallel execution

A forced native test executes one supported region through `ExecutionLease`.

The test covers worker execution, turn expiry, materialization, and recall.

Later surface stages repeat the pause, barrier, and replacement tests.

Workers share immutable compiled functions.

No raw guest pointer survives a checkpoint.

## 18. Performance measurements

The benchmark report separates these measurements:

- interpreter execution;
- native cold execution with compilation;
- native warm execution;
- sliced native execution;
- a guard-cost upper bound.

Every native benchmark asserts a nonzero native-retired count.

The first workload set contains `int_loop`, `float_add`, and `int_eq`.

The report also records an unsupported tiny program in `Auto` mode.

That program measures the interpreter fallback overhead.

The guard report compares zero and 32 additional live scalar values.

It reports an upper-bound cost for each additional guarded value.

## 19. Performance gates

Warm supported loops must improve by more than two times.

The target is a clear multiple of interpreter performance.

A typical two-value guard must stay below five percent of scheduled native loop time.

The 32-value stress row reports the linear cost without this threshold.

`Auto` mode must keep unsupported interpreter workloads within five percent.

Interpreter mode must stay within benchmark noise.

Cold results must report compilation cost without hiding it in setup.

Workspace build time and release binary size must appear in the report.

## 20. Platform behavior

The first backend supports platforms accepted by the pinned Cranelift version.

An unsupported platform reports that native execution is unavailable.

`Interpreter` always remains available.

`Auto` uses the interpreter on unsupported platforms.

No unsupported platform changes guest semantics.

## 21. Stages

### Stage 0: Specification and baseline

- write this sidecar;
- record interpreter rows for the first workloads;
- record workspace build and suite measurements;
- record the current executor ownership forms.

Stop gate: the baseline names revision, profile, engine, processor, and measurement method.

### Stage 1: Engine boundary

- add host engine policy;
- factor one internal engine operation for both executor wrappers;
- add clock-free engine metrics;
- keep interpreter behavior unchanged;
- add forced-policy tests.

Stop gate: interpreter traces and execution benchmarks remain within noise.

### Stage 2: Verified region metadata

- expose immutable verifier block-entry metadata;
- normalize supported scalar control flow;
- build entry plans and materialization maps;
- test unsupported-region diagnostics and reasons;
- add one-segment state comparison scaffolding.

Stop gate: metadata matches verifier states for crafted control-flow functions.

### Stage 3: Native integer and Boolean regions

- add the pinned Cranelift backend;
- add the explicit activation ABI;
- compile supported integer and Boolean control flow;
- add guarded entry;
- add exact fuel and overflow exits;
- add native code ownership and cache limits.

Stop gate: `int_loop` and `int_eq` pass differential tests and improve by more than two times.

### Stage 4: Float regions and measurement

- add canonical float arithmetic and comparisons;
- pass engine-switch and external-snapshot tests;
- measure cold, warm, guard, and fallback costs;
- record binary size and build time;
- run the complete workspace suite.

Stop gate: report the first workload results and guard overhead.

This stage is the first review point.

### Stage 5: Exact division faults

- add integer division and remainder;
- add exact zero and overflow exits;
- sweep fuel around every faulting instruction.

### Stage 6: Calls

- compile direct monomorphic calls;
- keep exact function versions pinned;
- materialize before unsupported callees;
- add recursive-call limits and tests.

### Stage 7: Heap access

- define stable object guards;
- define safepoint root maps;
- add allocation exits;
- preserve collection and snapshot rules.

No heap instruction enters native execution before this stage.

## 22. Acceptance statement

The first milestone answers one architectural question.

It proves whether native scalar execution can preserve Loom's canonical reified state.

Success requires exact interpreter equivalence and clear warm speedups.

Later stages expand the compiled surface without changing the executor contract.

## 23. Stage 0 measurements

The base revision is `44c43a7bcaa40f9fe39da24afcf1e9e57eb96722`.

The host uses an AMD Ryzen 9 9950X processor.

The host has 16 physical cores and 32 logical processors.

Runtime measurements use the release profile and `Interpreter` mode.

Each runtime process runs on logical processor zero.

Each result is the median of nine measured runs after one warm run.

| Workload | Base time |
| --- | ---: |
| `int_loop` | 33.7–33.8 ms |
| `float_add` | 34.5–35.4 ms |
| `int_eq` | 33.0 ms |

The pre-change same-worktree suite took 39.37 seconds.

Cargo used 9.30 seconds of that time for compilation.

One clean release CLI build took 16.60 seconds.

The base release benchmark executable used 14,390,048 bytes.

## 24. Stage 4 review measurements

One final pinned run produced these results.

Cold time starts after artifact publication.

It includes region analysis and native compilation.

| Workload | Interpreter | Native cold | Native warm | Warm gain |
| --- | ---: | ---: | ---: | ---: |
| `jit_int_loop` | 33.951 ms | 1.261 ms | 0.910 ms | 37.31 times |
| `jit_float_add` | 34.700 ms | 2.753 ms | 2.410 ms | 14.40 times |
| `jit_int_eq` | 32.832 ms | 1.243 ms | 0.933 ms | 35.20 times |

The interpreter-only base comparison used separate interleaved processes.

No interpreter row changed by more than 2.4 percent.

The 4,096-instruction sliced integer loop improved by 19.43 times.

The deterministic scheduler loop improved by 7.23 times.

An early implementation interpreted a complete quantum after an interior stop.

That defect limited the scheduled gain to 1.1 times.

The executor now advances only to the next native segment entry.

The guard stress row measured 7.62 nanoseconds for each additional live scalar.

Two live values account for approximately 4.2 percent of the scheduled native loop.

This estimate excludes fixed activation and materialization costs.

The 32-value stress case increased its sliced time from 1.334 milliseconds to 1.930 milliseconds.

The wide stress case confirms that guard cost scales with live state.

It does not justify a world-wide integrity property for this first surface.

The stable unsupported `Auto` workload stayed within 0.4 percent of `Interpreter`.

The complete workspace suite passes.

The first final run took 46.33 seconds and rebuilt test targets.

The next warm run took 31.08 seconds.

The new JIT test target used 1.23 seconds.

The pre-change execution component took approximately 30.07 seconds.

The other warm test targets used approximately 29.85 seconds.

Existing test execution therefore stayed within 0.8 percent of the pre-change result.

One clean release CLI build took 29.94 seconds.

Cranelift increases this clean Rust build by 80.4 percent.

The JIT-enabled benchmark executable uses 20,056,000 bytes.

This size is 39.4 percent larger than the base executable.

The release CLI uses 19,163,064 bytes.

This size is 41.7 percent larger than its 13,521,160-byte base.

`Engine` owns the JIT cache even when it selects `Interpreter`.

That ownership retains Cranelift drop code in the CLI.

The backend build cost and enabled binary size remain review items.

### 24.1 Crate extraction measurements

The extraction moved native compilation from `lm-vm` into `lm-jit`.

The adapter resolves source-unit data only when a function misses the native cache.

All measurements used one pinned processor and the release profile.

| Workload | Before | After |
| --- | ---: | ---: |
| Integer warm | 0.910 ms | 0.912 ms |
| Float warm | 2.410 ms | 2.383 ms |
| Equality warm | 0.933 ms | 0.948 ms |
| Sliced warm | 1.780 ms | 1.826 ms |
| Scheduled warm | 4.890 ms | 4.698 ms |
| Wide guard | 1.930 ms | 1.945 ms |

The values remain within measurement variance.

An initial extraction resolved the source unit before every cache lookup.

That error made scheduled execution more than two times slower.

The lazy lookup removed that cost without weakening the crate boundary.
