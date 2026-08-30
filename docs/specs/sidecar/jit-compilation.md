# JIT compilation

Status: Native calls, the heap ABI, sampled tiering, and scheduler continuation are implemented.

Direct instance reads use the canonical heap layout.

Corpus validation and broader direct heap access remain.

This sidecar refines the executor contract in the multi-threaded scheduler sidecar.

## 1. Goals

The JIT has these goals:

- improve complete Loom programs;
- preserve exact interpreter results and faults;
- preserve canonical machine state at every observable boundary;
- preserve LMBC fuel accounting;
- support fresh, captured, and externally restored machines;
- keep engine policy under host control;
- keep common operations inside native code;
- report cold and warm costs separately.

A supported warm workload must improve by more than two times.

A representative program cannot regress by more than five percent in Auto mode.

## 2. Core rules

Verified LMBC is the only executable input.

The verifier proves code structure and instruction types.

Native entry guards prove the active value representations.

External snapshot bytes never assert type integrity.

Generated code uses one explicit runtime memory ABI.

Generated code does not depend on private Rust layouts.

A private Rust layout and an explicit runtime ABI are different things.

Common field and element operations use direct native memory access.

Runtime slow paths handle uncommon or complex work.

A call path does not become an engine transition.

An effect remains an observable engine exit.

VmState remains the canonical form at observable engine exits.

## 3. Independent proofs

Native execution uses two independent facts.

The code proof states that the LMBC passed the verifier.

The state proof states that each active value has its required representation.

Artifact publication establishes the code proof.

The JIT consumes verifier program-point metadata.

The JIT does not implement another type checker.

The entry plan names only values that the region can read.

The entry guard checks those values before native entry.

A failed guard changes no machine state.

A failed guard resumes interpreter execution at the same position.

The guard does not prove dormant locals or other machines.

The guard does not prove every value reachable through the heap.

A native heap load checks its loaded tag when state has structural integrity only.

A loaded object reference also checks generation and shape.

These checks use direct native comparisons.

They do not use a runtime callback.

The existing restored_any property continues to select interpreter boundary checks.

It does not select the execution engine.

A later StateTypeIntegrity design can remove redundant native type guards.

That later design does not change this heap ABI.

## 4. Engine policy

The host selects one engine policy.

    pub enum EngineMode {
        Interpreter,
        Auto,
        Native,
    }

Interpreter executes no native code.

Auto begins with interpreter execution.

Auto compiles only after measured useful work.

Native exposes eligibility failures through metrics and tests.

Engine policy is not guest state.

Artifacts and snapshots never contain engine policy.

Engine choice cannot change guest results, faults, traces, or fuel.

Interpreter mode must add almost no JIT work.

## 5. Executor boundary

Deterministic execution borrows one machine.

Parallel execution owns one ExecutionLease.

Both forms call one internal engine operation.

    run_engine_turn(machine, code, environments, slots, limits, engine)
        -> engine result

The operation receives no mutable World pointer.

The machine lease gives one turn exclusive heap access.

Compiled code stores no worker identity.

Compiled code pins one exact function version.

A worker can share immutable compiled code with other workers.

## 6. Native turn stack

One native activation owns all compiled frames until an observable boundary.

The activation contains:

- one contiguous scalar area;
- one scalar-state area;
- one bounded frame table;
- one stable function-entry table;
- one fuel balance;
- one final exit record.

Each native frame records its function and resume position.

Each frame also records its scalar window and operand length.

Static region plans supply every scalar kind.

A direct call loads one stable function entry cell.

A published entry pushes one native frame and performs one native call.

A null entry exits before the call retires.

The interpreter can execute that call and compile the callee later.

A native return pops one native frame.

Recursion uses the same calling convention.

Frame and stack limits match the interpreter limits.

No call materializes VmState.

No call invokes a generic runtime dispatcher.

An ordinary scheduler quantum can retain the native activation.

The machine moves its canonical stack storage into that continuation.

The machine does not retain a second scalar-state copy.

The continuation pins every active compiled region.

It also stores all active object roots.

A later worker can resume the same continuation.

Worker identity does not enter the continuation.

Recall materializes the continuation before it returns the lease.

Snapshots materialize all selected machines before encoding.

Inspection and interpreter entry also materialize native state.

Faults, effects, and terminal returns always materialize native state.

## 7. Region plans

A region is one supported function control-flow graph.

A segment is one fixed-cost path between fuel checkpoints.

Conditional jumps split LMBC blocks into segments.

Each segment has one exact LMBC instruction cost.

Every segment ends at one of these points:

- a control transfer;
- a safepoint;
- a fault;
- a return;
- an unsupported instruction.

Native code can continue through segment edges and loop backedges.

It does not return to Rust after every segment.

A restored machine can name an interior LMBC position.

The interpreter advances to a supported native entry.

## 8. Fuel and faults

Fuel measures retired LMBC instructions.

Native code checks fuel before each segment.

Full segment fuel runs the normal segment path.

Partial segment fuel runs exact instruction checkpoints.

Each checkpoint charges one LMBC instruction.

The final checkpoint records the next instruction position.

A call instruction charges one fuel unit.

A faulting instruction charges its executed prefix.

Fault exits record the post-increment LMBC position.

Fault exits preserve exact operand consumption.

Loom faults use explicit exit records.

Cranelift traps never represent guest faults.

Every fuel limit must produce the same interpreter and native state.

## 9. Value ABI

Value is the canonical 16-byte runtime value.

The implementation gives Value a fixed representation.

    #[repr(C, u64)]
    pub enum Value {
        Unit = 0,
        Bool(bool) = 1,
        Int(i64) = 2,
        Float(u64) = 3,
        Char(char) = 4,
        Obj(ObjRef) = 5,
        Op(u32) = 6,
        Callback(CallbackRef) = 7,
        EmptyCase { ty: u32, arm: u32 } = 8,
        Uninit = 9,
    }

The discriminants are append-only.

A removed variant leaves one reserved discriminant.

The runtime asserts the size, alignment, tag width, and payload offset.

The runtime tests every variant conversion.

Native scalar registers can keep unboxed payloads.

Heap arrays store canonical Value instances.

Generated code reads their tag and payload through fixed offsets.

Native code writes only valid tags and canonical payloads.

Float writes use the canonical NaN encoding.

## 10. Heap ABI

The heap's canonical storage is the native ABI.

The heap keeps no parallel object record.

ValueArray owns one allocation through a stable record.

    #[repr(C)]
    pub struct ValueArray {
        data: *mut Value,
        len: usize,
        capacity: usize,
    }

ValueArray supports fallible reservation before guest-controlled growth.

Instance fields, list items, tuple items, and closure captures use ValueArray.

Object uses a fixed u32 tag and C field layout.

Native code reads only the object variants named by this ABI.

Other object variants remain opaque to native code.

Header uses a fixed C layout.

Its first byte contains the frozen flag.

Each entry stores its generation and one tagged state.

The state is Dead or Live.

The live payload contains Header and Object.

This state gives automatic drops and requires no uninitialized storage.

The heap exposes canonical page addresses, page count, and slot count.

One canonical page holds 1,024 entries.

Each page reserves its complete capacity before publication.

The page address never changes after publication.

No raw pointer becomes guest data.

## 11. Heap pointer lifetime

A validated handle remains stable until one safepoint.

The heap does not move live objects.

The machine lease prevents concurrent heap mutation.

A payload pointer lives only in straight-line code between calls.

Native code reloads every payload pointer after call_indirect.

Native code reloads every payload pointer after a native call.

Any callee can allocate or collect.

After a slow path, native code reloads:

- the page table;
- the object entry;
- the data address;
- the length;
- required guards.

A collection slow path receives complete roots for all native frames.

The materializer exposes those roots before collection.

Generation checks can move above repeated accesses within one safe segment.

Bounds checks remain before every unchecked address calculation.

## 12. Canonical storage maintenance

Allocation writes one canonical live entry.

Freeing drops its object and changes the state to Dead.

Freeing also advances the generation.

Freezing changes the canonical header byte.

Instance and tuple array addresses never change after allocation.

List growth updates its ValueArray directly.

List truncation and removal update the same ValueArray.

No mutation path synchronizes duplicate object metadata.

A new page extends the page-address table.

An allocation slow path refreshes the activation after that extension.

Tests compare exported offsets with canonical field addresses.

Graph operations and snapshots continue to read canonical Object values.

The native layout does not affect snapshot bytes or semantic digests.

## 13. Direct fast paths

Native code implements these common operations directly:

- handle validation;
- exact class tests;
- instance field loads;
- instance field stores;
- tuple element loads;
- list length;
- list element loads;
- list element replacement.

A field load performs these checks:

1. Check the slot bound.
2. Load the page and entry.
3. Check generation and liveness.
4. Check the cached concrete class.
5. Check the field bound.
6. Load the Value.
7. Check the loaded representation when required.

A field store also checks the frozen flag.

The store writes one canonical Value.

The current collector needs no generational write barrier.

Future collectors must extend this explicit ABI.

Runtime execution never walks semantic hashes.

Linked dense class indexes select runtime classes.

Inherited fields keep their verified prefix offsets.

The first tier uses an exact concrete-class guard.

A class mismatch replays the instruction in the interpreter.

A later inline cache can add one observed subclass guard.

A monomorphic cache guards the observed concrete class.

A miss leaves native code through one typed resolver path.

## 14. Slow paths

Slow paths handle:

- object allocation;
- collection;
- list growth;
- map operations;
- complex hashing;
- string construction;
- builder growth;
- deoptimization;
- observable effects.

Each slow path has one fixed typed signature.

No slow path receives a generic operation number.

No slow path decodes a metadata contract.

A slow path returns one explicit result code.

Common success paths do not call Rust.

A coarse operation can remain one slow call when its work dominates the transition.

## 15. Effects and safepoints

An effect always materializes canonical state.

The interpreter retires the effect instruction.

Policy and host handling remain outside generated code.

A reply can enter native code through the normal guard.

Recall, pause, capture, and debugging require canonical state.

Each such boundary materializes every native frame once.

Ordinary calls and field accesses are not safepoints.

## 16. Compiled-code ownership

One host engine owns native compilers.

The cache belongs to one exact arena layout.

Each dense function slot owns one compilation verdict.

Each slot also owns one stable native entry cell.

A failed compilation records one negative verdict.

Concurrent compilation serializes only one function request.

Published native code is immutable.

Every region releases its executable memory on final drop.

A semantic function hash never identifies arena-relative operands.

The cache key includes the arena layout identity.

## 17. Tiering and profiling

Auto starts in the interpreter.

A conservative classifier rejects bytecode that the current planner cannot compile.

Rejected functions use no hotness counter and cause no compiler probe.

Auto samples direct calls and loop backedges before it compiles one region.

Each sample adds an estimated retired-instruction count to one relaxed counter.

The interpreter path performs no mutex operation.

Tier checks run inside direct-call, return, and taken-backedge dispatch.

Ordinary interpreter instructions run the unchanged dispatch path.

A compiled region enables direct-call entry checks for its namespace.

A future virtual-call tier owns one atomic inline-cache cell per site.

A classified site stops collecting observations.

A cache lookup occurs before specialization work.

A negative verdict stops later entry probes.

Compilation uses an expected-retirement budget.

The budget includes region size and observed hotness.

Compilation does not hold a shared execution lock.

Cold compilation time is a permanent metric.

## 18. Metrics

Clock-free counters include:

- compilation attempts;
- compiled regions;
- compilation rejections by reason;
- native entries;
- native-retired LMBC instructions;
- direct fast-path operations;
- slow-path calls by kind;
- materializations;
- native continuation suspensions;
- native continuation resumptions;
- forced continuation materializations;
- guard failures;
- deoptimizations;
- native fault exits.

Native coverage counts instructions executed without runtime slow paths.

A slow-path instruction does not count as direct native work.

Wall-time reports include:

- interpreter execution;
- Auto cold execution;
- Auto warm execution;
- forced native execution;
- compilation time;
- code size.

## 19. Correctness tests

Every supported test compares Interpreter and Native.

Tests compare complete live machine state.

Fuel tests sweep every segment boundary.

Tests alternate engines between bounded turns.

Tests capture after native execution and resume in the interpreter.

Tests capture after interpreter execution and resume in native code.

External snapshot tests include wrong scalar values.

They also include wrong field and collection element values.

Malformed state must never cause host memory access outside the checked ABI.

Parallel tests cover recall, pause, barriers, and replacement.

Heap ABI tests cover stale handles and dead slots.

They cover every object kind exposed to native code.

Mutation tests force list reallocation between two native entries.

## 20. Performance gates

The complete corpus is a permanent gate.

The language benchmark set is a permanent gate.

Representative rows include:

- JSON parsing;
- JSON encoding;
- HTTP parsing;
- HTTP encoding;
- field access;
- list indexing;
- virtual calls;
- recursion;
- scheduler-sliced scalar loops.

Auto cannot slow a large corpus program by more than five percent.

Interpreter mode must remain within measurement noise.

At least one JSON row must improve by more than two times.

At least one HTTP row must improve by more than two times.

Common field and list loops target the scalar-loop performance range.

Cold and warm results remain separate.

Every report names the revision, host, profile, and measurement method.

## 21. Crate boundaries

lm-jit owns:

- verified region planning;
- Cranelift lowering;
- native activation records;
- executable memory;
- JIT ABI offsets.

lm-heap owns:

- canonical object storage;
- stable ValueArray storage;
- canonical heap layout constants;
- canonical page-address views;
- checked JIT heap views.

lm-vm owns:

- engine policy;
- canonical machine state;
- entry guards;
- materialization;
- typed slow-path implementations.

lm-jit never depends on lm-vm.

lm-jit depends on lm-heap only for explicit ABI types and constants.

lm-vm passes one heap view into each native activation.

No compiled function captures a mutable World pointer.

## 22. Implementation stages

### Stage A: Recovery baseline

- preserve the arena-scoped cache;
- preserve general arithmetic stacks;
- preserve native call frames;
- preserve entry cells;
- preserve exact chain materialization;
- split compiler and adapter modules;
- record scalar, call, field, allocation, and scheduler rows.

Gate: The split changes no result or benchmark profile.

### Stage B: Fixed value and heap ABI

- fix the Value representation;
- add stable ValueArray storage;
- fix the Object, Header, and entry layouts;
- expose checked heap views;
- add layout and mutation tests.

Gate: Exported offsets name canonical objects after every heap operation.

### Stage C: Remove the callback architecture

- delete the generic runtime service trait;
- delete the operation-number dispatcher;
- compile direct instance field loads;
- keep effects as explicit exits;
- keep allocation as one typed slow path;
- retain every differential test as a gate.

Gate: Field reads use no runtime slow call.

### Stage D: Complete direct object access

- compile instance field stores;
- compile tuple element loads;
- compile list length and element operations;
- add structural-state result guards;
- remove equivalent runtime slow paths.

Gate: Common operations use zero slow calls per iteration.

### Stage E: Tiering and inline caches

- classify unsupported function shapes before execution;
- sample calls and loop backedges;
- add lock-free hotness counters;
- add negative verdicts;
- keep specialization behind cache hits;
- add compilation budgets.

Gate: Unsupported large programs remain within five percent.

Gate result: The four representative rows remain within two percent in Auto mode.

Virtual-call inline caches remain part of Stage F surface expansion.

### Stage F: Representative programs

- measure the complete corpus;
- profile weighted unsupported instructions;
- add only high-value fast paths;
- keep complex operations in typed slow paths.

Gate: JSON and HTTP meet the representative gains.

Scheduler continuation landed before broader collection access.

It retains native frames across ordinary deterministic and parallel quanta.

Recall, snapshots, faults, effects, and engine changes force materialization.

## 23. Retained baseline

The recovery base is commit 791aff6.

The module split checkpoint is commit 73c70d1.

The release measurements used one same-session run.

| Workload | Interpreter | Native warm | Warm gain |
| --- | ---: | ---: | ---: |
| Integer loop | 33.712 ms | 0.709 ms | 47.54 times |
| Factorial | 5.355 ms | 0.905 ms | 5.92 times |
| Direct scalar call | 48.229 ms | 0.886 ms | 54.45 times |
| Instance field read | 48.113 ms | 5.406 ms | 8.90 times |
| Plain allocation | 7.854 ms | 2.575 ms | 3.05 times |
| Scheduled integer loop | 35.937 ms | 8.850 ms | 4.06 times |

The field and allocation rows use the temporary callback layer.

Later stages replace those rows with direct ABI measurements.

The canonical heap candidate produced these focused release rows.

| Workload | Interpreter | Native warm | Warm gain |
| --- | ---: | ---: | ---: |
| Integer loop | 30.044 ms | 0.713 ms | 42.12 times |
| Instance field read | 45.751 ms | 1.675 ms | 27.32 times |
| Plain allocation | 7.673 ms | 2.625 ms | 2.92 times |

These rows do not replace the scheduler corpus gate.

The first sampled-tiering run used the deterministic scheduler.

It used nine measured rounds after one warm round.

| Workload | Interpreter | Auto warm | Auto gain | Native coverage |
| --- | ---: | ---: | ---: | ---: |
| JSON parse | 46.161 ms | 46.534 ms | 0.99 times | 0.00% |
| JSON encode | 20.537 ms | 20.914 ms | 0.98 times | 0.00% |
| HTTP parse | 44.545 ms | 43.983 ms | 1.01 times | 35.46% |
| HTTP encode | 23.087 ms | 22.565 ms | 1.02 times | 41.68% |

Measured warm rounds performed no native compilation.

The first scheduler-continuation measurement used a 1,024-instruction quantum.

| Workload | Interpreter | Native warm | Warm gain |
| --- | ---: | ---: | ---: |
| Scheduled integer loop | 41.617 ms | 3.843 ms | 10.83 times |

The earlier native path gained 3.89 times on this row.

## 24. Rejected designs

A generic callback dispatcher cannot implement common heap instructions.

A parallel side record cannot mirror mutable object layout.

Per-operation metadata decoding cannot remain on a native fast path.

Semantic hash walks cannot remain in runtime class checks.

Interpreter profiling cannot take a mutex per call.

Specialization cannot occur before a cache hit.

Every eligible function cannot compile on its first entry.

A call cannot materialize canonical state.

A scheduler continuation cannot keep stale duplicate scalar state.

Generated code cannot serialize process-local heap addresses.

These designs remain rejected even when local differential tests pass.
