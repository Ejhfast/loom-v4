# JIT compilation

Status: Native calls, the heap ABI, sampled tiering, scheduler continuation, and growable activations are implemented.

Direct instance, tuple, list, and byte access use the canonical heap layout.

Representative-program gains remain.

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
- one growable frame table;
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

A successful call keeps the caller's live values in native registers.

A successful return does not spill the completed child frame.

An exceptional exit spills each suspended frame while the native calls unwind.

The frame table stores deoptimization state, limits, and continuation positions.

A resumed return tail-calls the saved parent entry.

The final exit uses the active frame's result type.

Frame and stack limits match the interpreter limits.

Native storage starts small and grows geometrically.

The native engine performs growth outside generated call code.

Storage growth does not materialize VmState.

Only the guest frame and stack limits bound native depth.

No call materializes VmState.

No call invokes a generic runtime dispatcher.

An ordinary scheduler quantum can retain the native activation.

The machine moves its canonical stack storage into that continuation.

The machine does not retain a second scalar-state copy.

The continuation pins every active compiled region.

Garbage collection derives active object roots when it needs them.

A later worker can resume the same continuation.

Worker identity does not enter the continuation.

Recall materializes the continuation before it returns the lease.

Snapshots materialize all selected machines before encoding.

Snapshot code materializes only resident machines.

Leased machines report state through their scoped barrier.

Inspection and interpreter entry also materialize native state.

Faults, effects, and terminal returns always materialize native state.

## 7. Region plans

A region is one verified function control-flow graph.

A segment is one fixed-cost path between fuel checkpoints.

Conditional jumps split LMBC blocks into segments.

Each segment has one exact LMBC instruction cost.

Every segment ends at one of these points:

- a control transfer;
- a safepoint;
- a fault;
- a return;
- a temporary interpreter site.

Native code can continue through segment edges and loop backedges.

It does not return to Rust after every segment.

Every instruction receives one permanent treatment:

- A: direct register code;
- B: guarded memory access;
- C: an inline fast path with one typed slow path;
- D: a native call with an inline cache;
- E: one fixed typed runtime function;
- F: an observable engine exit.

An unfinished treatment uses one temporary interpreter site.

The site exits before the instruction retires.

The interpreter executes exactly that instruction.

The engine then retries native entry at the next program point.

A temporary site never rejects the complete function.

A temporary site does not count as native instruction coverage.

Auto can demote a function with frequent interpreter sites.

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

A slow path never replaces an opcode with an inline treatment.

Classes A through D never use a runtime stub.

The JIT does not add a stub only to increase a coverage number.

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

A conservative classifier rejects only unsupported types, shapes, and resource sizes.

One missing instruction treatment does not reject a function.

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

A compiled function can also become an unproductive verdict.

The engine samples retired work after native entry.

Repeated quick exits disable later native entry for that function.

A quick exit includes an interpreter boundary or an unsupported callee.

Forced Native mode reports this decision without hiding it.

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
- unproductive-entry demotions;
- native frame-storage growth;
- native fault exits.
- compiled interpreter sites;
- native interpreter exits.

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
- deep recursion;
- quick native exits;
- scheduler-sliced scalar loops.

Auto cannot slow a large corpus program by more than five percent.

Interpreter mode must remain within measurement noise.

At least one JSON row must improve by more than two times.

At least one HTTP row must improve by more than two times.

Common field and list loops target the scalar-loop performance range.

Cold and warm results remain separate.

A warm measurement uses one stable arena and one engine.

The harness discards timings after any later compilation.

It records measured rounds only after the compiled set becomes stable.

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
- demote compiled functions that repeatedly retire little native work;
- keep specialization behind cache hits;
- add compilation budgets.

Gate: Unsupported large programs remain within five percent.

Gate result: The four representative rows remain within two percent in Auto mode.

Virtual-call inline caches remain part of Stage F surface expansion.

### Stage E2: Growable native activations

- grow frame and scalar storage outside generated code;
- resume the pending native call after growth;
- preserve exact fuel and stack-limit behavior;
- test recursion beyond every initial capacity.

Gate: Deep recursion does not cross an engine boundary for storage growth.

Gate: The recursion benchmark improves in Auto and Native modes.

Gate result: Deep recursion improved 1.27 times in Auto mode.

Gate result: Deep recursion improved 1.28 times in Native mode.

Gate result: Quick exits remained within three percent in Auto mode.

### Stage E3: Lazy native call state

- keep caller values native across successful calls;
- avoid child spills on successful returns;
- spill suspended frames only during exceptional exits;
- preserve exact faults, effects, fuel, and scheduler continuation;
- measure shallow and deep non-inline calls.

Gate: A branch-bearing native call improves by more than four times.

Gate: Deep recursion remains faster in single-turn and scheduled execution.

### Stage F: Representative programs

- measure the complete corpus;
- profile weighted unsupported instructions;
- add only high-value fast paths;
- keep complex operations in typed slow paths.

Gate: JSON and HTTP meet the representative gains.

### Stage F1: Immutable byte reads

- make the immutable byte view part of the heap ABI;
- compile byte length and indexed byte reads directly;
- deoptimize invalid handles and indexes;
- keep byte storage immutable during native reads.

Gate: One million indexed byte reads improve by more than five times.

Gate result: Scheduled byte reads improved 10.42 times in Auto mode.

### Stage F2: Mixed instruction coverage

- split a region at each instruction without a dedicated treatment;
- use verifier states at the exact program points;
- interpret exactly one instruction at each temporary site;
- retry native entry after that instruction;
- preserve exact fuel and canonical machine state;
- count compiled sites and executed exits;
- demote functions with frequent exits in Auto mode.

Gate: One untreated integer instruction does not reject its function.

Gate: Every fuel boundary matches the interpreter around that instruction.

### Stage F3: Direct scalar coverage

- compile integer bit operations and wrapping arithmetic;
- compile checked shifts and rotations;
- compile scalar float conversions and queries;
- compile Char constants, queries, and comparisons;
- compile reference equality for supported object types;
- replay invalid shifts and casts through one uncommon exit.

Gate: The scalar surface uses no temporary interpreter site.

Gate: Character values materialize exactly at every tested fuel boundary.

Gate: The scheduled bitwise loop improves by more than five times.

### Stage F4: Direct class guards

- pass one append-only class-parent table into each native turn;
- compile `is` and successful `as` operations;
- use subtype guards for fields and loaded object values;
- use one native representation for all object references;
- replay failed casts through one uncommon interpreter exit.

Gate: A subclass passes a guard for its parent without an interpreter exit.

Gate: The scheduled class-guard loop improves by more than five times.

### Stage F5: Canonical object values

- represent String, Map, and function values as canonical object references;
- validate each object kind at native entry and interpreter reentry;
- carry these values through native calls and scheduler continuations;
- keep one representation for every object reference.

Gate: Native calls carry each supported object kind across short scheduler quanta.

Gate: Representative functions fail only at instructions without a dedicated treatment.

### Stage F6: Direct collection metadata

- compile list capacity reads;
- compile list epoch observation and validation;
- compile safe byte reads without an out-of-range memory load;
- compile frozen-instance sealing;
- replay invalid handles and changed collections through one uncommon exit.

Gate: Scheduled list traversal improves by more than five times.

Gate: A changed collection produces the exact interpreter fault and state.

Gate: Safe byte reads return the byte or minus one without a runtime helper.

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

The direct array-access pass produced these focused release rows.

| Workload | Interpreter | Native warm | Warm gain |
| --- | ---: | ---: | ---: |
| Instance field read | 48.014 ms | 1.717 ms | 27.97 times |
| Instance field write | 64.536 ms | 8.598 ms | 7.51 times |
| Tuple read | 39.468 ms | 1.529 ms | 25.81 times |
| List read | 45.300 ms | 1.920 ms | 23.60 times |
| List replacement | 53.366 ms | 1.791 ms | 29.80 times |

The first stable JSON parse run reached 0.91 percent native coverage.

Auto took 52.272 ms, compared with 46.353 ms for the interpreter.

This result does not pass the representative-program gate.

The Stage E2 run used the deterministic scheduler and a 1,024-instruction quantum.

It used nine stable warm rounds after all native compilation stopped.

| Workload | Interpreter | Auto warm | Native warm | Auto gain |
| --- | ---: | ---: | ---: | ---: |
| Deep recursion | 35.823 ms | 28.124 ms | 27.955 ms | 1.27 times |
| Quick exits | 3.604 ms | 3.711 ms | 9.056 ms | 0.97 times |
| Class initialization | 110.204 ms | 41.271 ms | 40.609 ms | 2.67 times |

The lazy-call pass produced these focused release rows.

| Workload | Interpreter | Native warm | Warm gain |
| --- | ---: | ---: | ---: |
| Branch-bearing call | 52.929 ms | 9.539 ms | 5.55 times |
| Deep recursion | 36.029 ms | 9.309 ms | 3.87 times |

The scheduled branch-bearing call improved 4.68 times.

The scheduled deep-recursion row improved 1.29 times.

Forced Native mode exposes the expected quick-exit loss.

Auto mode avoids that loss through its productive-entry policy.

The HTTP parser now passes after native continuations return nested object results.

It still gains only 1.01 times in Auto mode.

The direct byte-read pass used the deterministic scheduler.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native coverage |
| --- | ---: | ---: | ---: | ---: | ---: |
| Byte reads | 69.597 ms | 6.683 ms | 6.578 ms | 10.42 times | 100.00% |
| JSON parse | 46.387 ms | 48.011 ms | 56.959 ms | 0.97 times | 1.49% |
| HTTP parse | 43.394 ms | 43.566 ms | 45.819 ms | 1.00 times | 36.57% |

The byte-read gate passes.

The representative-program gate remains open.

The Stage F4 run added direct class guards and subtype-compatible object values.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native gain |
| --- | ---: | ---: | ---: | ---: | ---: |
| Class guard | 110.816 ms | 16.512 ms | 17.474 ms | 6.71 times | 6.34 times |

Both native modes reached complete coverage. Neither mode used an interpreter exit.

The Stage F5 run added String, Map, and function object representations.

Both representative programs became native candidates without type-based rejection.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native coverage |
| --- | ---: | ---: | ---: | ---: | ---: |
| JSON parse | 46.028 ms | 47.437 ms | 67.736 ms | 0.97 times | 5.23% |
| HTTP parse | 43.927 ms | 44.681 ms | 63.361 ms | 0.98 times | 46.88% |

Frequent instruction exits still make forced Native mode slower.

The representative-program performance gate remains open.

The Stage F6 run added direct collection metadata and safe byte reads.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native gain |
| --- | ---: | ---: | ---: | ---: | ---: |
| List traversal | 60.124 ms | 6.679 ms | 6.257 ms | 9.00 times | 9.61 times |

Both modes reached complete measured coverage after one setup exit per round.

JSON remained within three percent in Auto mode.

HTTP improved by one percent in Auto mode.

The representative-program performance gate remains open.

The Stage F2 run used nine stable scheduler rounds.

One integer bitwise instruction used one temporary site per loop iteration.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native gain |
| --- | ---: | ---: | ---: | ---: | ---: |
| Bitwise interpreter site | 39.521 ms | 39.495 ms | 116.588 ms | 1.00 times | 0.34 times |
| Branch-bearing call | 55.676 ms | 11.879 ms | 11.797 ms | 4.69 times | 4.72 times |
| Deep recursion | 40.381 ms | 27.854 ms | 27.701 ms | 1.45 times | 1.46 times |

Auto recorded no interpreter exits during measured bitwise rounds.

Forced Native recorded one million interpreter exits in each bitwise round.

JSON parse remained within one percent in Auto mode.

HTTP parse remained within one percent in Auto mode.

Forced Native exposes the expected cost of frequent temporary sites.

The Stage F3 run added direct scalar treatments.

The numeric row includes bit operations, wrapping arithmetic, shifts, and rotations.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native gain |
| --- | ---: | ---: | ---: | ---: | ---: |
| Numeric surface | 40.940 ms | 3.897 ms | 3.848 ms | 10.51 times | 10.64 times |
| List-push interpreter site | 2.271 ms | 2.332 ms | 6.201 ms | 0.97 times | 0.37 times |
| JSON parse | 46.050 ms | 46.584 ms | 58.970 ms | 0.99 times | 0.78 times |
| HTTP parse | 44.187 ms | 42.366 ms | 45.416 ms | 1.04 times | 0.97 times |

The numeric row reached complete native coverage.

The list-push row still uses one temporary site per append.

Auto demoted the list-push region before measured execution.

The representative-program gate remains open.

## 24. Rejected designs

A generic callback dispatcher cannot implement common heap instructions.

A runtime stub cannot replace an opcode with an inline treatment.

A temporary interpreter site cannot count as native coverage.

A parallel side record cannot mirror mutable object layout.

Per-operation metadata decoding cannot remain on a native fast path.

Semantic hash walks cannot remain in runtime class checks.

Interpreter profiling cannot take a mutex per call.

Specialization cannot occur before a cache hit.

Every eligible function cannot compile on its first entry.

A call cannot materialize canonical state.

A successful call cannot spill a complete caller or child frame.

A scheduler continuation cannot keep stale duplicate scalar state.

Generated code cannot serialize process-local heap addresses.

These designs remain rejected even when local differential tests pass.
