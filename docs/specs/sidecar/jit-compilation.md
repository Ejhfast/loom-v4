# JIT compilation

Status: Native calls, the heap ABI, sampled tiering, scheduler continuation, growable activations, and demand-driven polls are implemented.

All scheduler tasks use one measured 16,384-instruction quantum.

Generated code rearms idle scheduler polls without materializing machine state.

Direct instance, tuple, list, and byte access use the canonical heap layout.

Stable list parameters and locals retain their canonical data pointer across safe loop backedges.

One reserved acyclic path can charge several segments with one fuel update.

Common native allocations omit unrelated root stores before the rare collection path.

Native local initialization uses the canonical frame-state bytes.

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

Representative workloads diagnose coverage, overhead, and regressions.

They do not define completion.

Native scalar kernels use optimized Rust implementations as performance ceilings.

The engine must approach production typed-JIT code quality while it preserves Loom semantics.

No workload ratio closes optimization while a removable structural hot-path cost remains.

### 1.1 Completion rule

A selected benchmark ratio cannot complete this project.

Benchmarks expose costs and prevent regressions.

Ratios are diagnostic evidence only.

A stage ends only after its structural target is complete and its measurements expose no new defect.

The completed engine must have these properties:

- adjacent typed operations can optimize as one native control-flow graph;
- fuel accounting adds no avoidable work inside a reserved native region;
- validated heap addresses remain available across safe call-free regions and loop backedges;
- common field, element, map-hit, and builder-capacity paths stay inline;
- typed helpers handle only work that dominates their call cost;
- direct calls use the native convention, and small monomorphic calls can inline;
- continuation work does not scan unchanged native depth on every scheduler quantum;
- cold compilation does not block ordinary execution for unbounded time;
- hotness policy cannot hide a structural code-generation defect.

Optimized Rust kernels define ceilings, not required fixed ratios.

The broad corpus detects semantic errors and workload regressions.

Work remains open while measurements expose a removable structural cost.

The project ends only after the engine contains the required production JIT mechanisms.

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

A generic function uses one type-erased native body.

Each native frame records its exact TypeEnvId.

Each machine owns a small type-metadata cache.

Each cache key contains the call site, canonical type-store identity, and parent environment.

The cache value contains a derived environment or one closed type identifier.

The cache persists across scheduler turns and temporary interpreter exits.

Compiled code contains no world-local type-environment index.

A cache miss exits before the call retires.

The VM derives the child environment and resumes the native site.

Generated code does not call a generic type dispatcher.

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

Resume reads the top native frame in constant time.

Continuation region lookup visits only frames that changed during the turn.

The machine moves its canonical stack storage into that continuation.

The machine does not retain a second scalar-state copy.

The continuation pins every active compiled region.

Garbage collection derives active object roots when it needs them.

Each suspended frame stores canonical tags for every scanned slot.

A reused scalar window cannot retain an old object tag.

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
- an observable engine boundary.

Native code can continue through segment edges and loop backedges.

It does not return to Rust after every segment.

Every instruction receives one permanent treatment:

- A: direct register code;
- B: guarded memory access;
- C: an inline fast path with one typed slow path;
- D: a native call through a fixed target, dispatch table, or inline cache;
- E: one fixed typed runtime function;
- F: an observable engine exit.

One exhaustive opcode ledger records each class, implementation status, exit behavior, replay point, and fault stack shape.

The Rust compiler rejects a new opcode until the ledger classifies it.

The bytecode verifier supplies local initialization and operand shapes at every instruction boundary.

The region planner does not duplicate bytecode stack effects.

The backend contains one per-opcode lowering path.

Every verified opcode has one production treatment.

The current engine has no temporary opcode treatment.

Opcode coverage does not prove function coverage.

The function gate must cover every value type, reachable control-flow shape, and relocated contract.

Each rejected function reports one exact rejection reason.

An observable boundary can execute one instruction in the interpreter.

This boundary is the permanent class F treatment for that instruction.

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

A statically tagged slot keeps only its payload in SSA form.

Only `Tagged` and `Callback` slots keep a dynamic tag in SSA form.

Canonical materialization writes the fixed tag for each static slot.

A bare union uses its canonical tag and payload as two native values.

The JIT never reserves one payload bit pattern for `Option` or another union.

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

The heap also exposes one writable charge counter and one collection threshold.

One canonical page holds 1,024 entries.

Each page reserves its complete capacity before publication.

The page address never changes after publication.

No raw pointer becomes guest data.

## 11. Heap pointer lifetime

A validated handle remains stable until one safepoint.

The heap does not move live objects.

The machine lease prevents concurrent heap mutation.

A movable payload pointer lives only in straight-line code between calls.

A static proof can retain one stable payload pointer across loop backedges.

The proof forbids owner reassignment and payload growth.

A guest call ends the proof because it can mutate an alias.

A typed runtime helper ends the proof when its contract can move that payload.

Native code reloads a movable payload pointer after its proof ends.

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
- list element replacement;
- list append when the current array has capacity.

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

Generated code selects each signature from one fixed function table.

No slow path receives a generic operation number.

No slow path decodes a metadata contract.

A slow path returns one explicit result code.

Common success paths do not call Rust.

A coarse operation can remain one slow call when its work dominates the transition.

A slow path never replaces an opcode with an inline treatment.

Class A and class B instructions never use a runtime stub.

Class C instructions call a stub only after their inline fast path fails.

Class D calls use fixed entry cells.

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

Each dense function slot owns one permanent compiler verdict.

Each slot also owns one stable native entry cell.

An unsupported function records one negative verdict.

Backend failure records one negative verdict.

Code-cache capacity never records a compiler verdict.

The host sets one machine-code byte budget.

The cache never uses a fixed region-count limit.

A released layout opens capacity for later compilation.

Concurrent compilation serializes only one function request.

Published native code is immutable.

Every region releases its executable memory on final drop.

A semantic function hash never identifies arena-relative operands.

The cache key includes the arena layout identity.

## 17. Tiering and profiling

Auto starts in the interpreter.

A conservative classifier rejects only unsupported types, shapes, and resource sizes.

The exhaustive opcode ledger prevents a missing instruction treatment.

Rejected functions use no hotness counter and cause no compiler probe.

Auto samples direct calls and loop backedges before it compiles one region.

Each sample adds an estimated retired-instruction count to one relaxed counter.

The interpreter path performs no mutex operation.

Tier checks run inside direct-call, return, and taken-backedge dispatch.

Ordinary interpreter instructions run the unchanged dispatch path.

A compiled region enables direct-call entry checks for its namespace.

Virtual calls read immutable dispatch rows.

A later monomorphic tier can add one atomic inline-cache cell per site.

A classified site stops collecting observations.

A cache lookup occurs before specialization work.

A permanent negative verdict stops later entry probes.

A compiled function can also become an unproductive verdict.

The engine samples retired work after native entry.

Repeated quick exits disable later native entry for that function.

A quick exit includes an observable boundary or an unavailable callee.

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
- native fault exits;
- class F boundary exits.

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

The standalone corpus runs through direct turns and deterministic scheduler turns.

The corpus gate compiles every unique verified artifact function.

No corpus function can return an `Unsupported` result.

Scheduler runs use 1,024-instruction quanta and one deterministic recording host.

Corpus comparisons include outcomes, live state, output bytes, and host operation order.

The generic Option regression must retire native instructions during its scheduler run.

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

## 20. Performance evidence

The complete corpus is a permanent correctness gate.

The language benchmark set detects performance regressions.

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

Same-session comparisons identify regressions outside measured variance.

Each identified regression needs a structural explanation or a correction.

Interpreter mode must remain within measurement noise.

Optimized Rust kernels show the remaining code-generation cost.

Complete programs show the combined engine and runtime cost.

No fixed workload ratio defines completion.

Section 1.1 defines the production-JIT completion rule.

Cold and warm results remain separate.

A warm measurement uses one stable arena and one engine.

The harness discards timings after any later compilation.

It records measured rounds only after the compiled set becomes stable.

Every report names the revision, host, profile, and measurement method.

## 21. Crate boundaries

lm-jit owns:

- verified region planning;
- the exhaustive opcode treatment ledger;
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

Stage measurements validate one change and prevent regressions.

Historical ratio targets do not define project completion.

Each `Gate:` line validates only its local stage.

A local gate cannot stop later production-mechanism work.

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
- complete every opcode treatment by production class;
- use profile weights only to order the work;
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

This stage used a transitional mechanism.

Stage F32 removes this mechanism.

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

### Stage F7: Direct text metadata and hash mixing

- make immutable text metadata part of the heap ABI;
- compile byte and scalar length reads for String and Substring;
- compile ordered and unordered integer hash mixing;
- guard both text object tags before each direct metadata read.

Gate: Text metadata reads and hash mixing use no runtime helper.

Gate: JSON and HTTP remain within five percent in Auto mode.

### Stage F8: Canonical Option values

- preserve one canonical tag and payload for each bare union value;
- compile `OptionSome`, `OptionNone`, and `OptionPayload` directly;
- compile `Option` family and arm type tests directly;
- resolve closed family identifiers through one lazy metadata exit;
- cache each resolved identifier by bytecode site and active type environment;
- resume the same native instruction after successful resolution;
- interpret one instruction when type resolution reaches its limit.

The metadata exit does not retire the pending `Option` instruction.

The steady native path uses no runtime helper or sentinel payload.

Gate: An `Option` loop uses no temporary interpreter site.

Gate: Every tested fuel boundary matches the interpreter.

### Stage F9: Canonical literal loads

- store cached literals as canonical `Value` entries;
- expose the fixed table only during one native execution;
- load cached String and Bytes references directly;
- exit before an uncached literal and interpret that instruction once;
- compile `Unreachable` as its exact native fault exit;
- preserve literal roots and snapshot encoding.

No generated literal load calls a runtime helper.

Native execution cannot change the literal table.

Gate: Cached literal loops improve by more than five times.

Gate: Literal and `Unreachable` gaps disappear from parser profiles.

### Stage F10: Generic direct calls

- compile one type-erased body for each generic function;
- preserve each frame's exact TypeEnvId;
- reuse the machine-local type-metadata cache for exact `CallG` sites;
- derive one child environment on each cache miss;
- resume the call without retiring it on a miss;
- preserve exact state at every tested fuel boundary.

Gate: Repeated generic direct calls improve by more than two times.

Gate: `CallG` disappears from representative treatment gaps.

### Stage F11: Generic allocation and optional list access

- use one environment-site mechanism for `CallG` and `NewG`;
- cache each exact allocation environment on the machine;
- pass the environment through the typed allocation path;
- store the exact witness in each generic instance;
- compile `ListGet` as one guarded array read;
- create canonical `Option.None` values on missing indexes;
- preserve exact state at every tested fuel boundary.

Gate: Repeated generic allocation improves by more than two times.

Gate: Repeated optional list reads improve by more than five times.

Gate: `NewG` and `ListGet` disappear from representative treatment gaps.

### Stage F12: Direct list append

- compile the common `ListPush` path as one guarded array write;
- update the canonical object charge and heap charge directly;
- call one fixed typed slow path for growth or collection;
- pass complete native roots before a possible collection;
- preserve heap-limit, frozen-value, and epoch faults;
- preserve exact state at every tested fuel boundary.

Gate: Repeated list append improves by more than five times.

Gate: `ListPush` disappears from representative treatment gaps.

Gate: The common append path calls no runtime function.

### Stage F13: Direct list capacity changes

- compile `ListReserve` with one inline capacity check;
- call one fixed typed slow path only when capacity must grow;
- pass complete native roots before a possible collection;
- compile `ListReorder` as one guarded epoch update;
- preserve frozen-value, heap-limit, and epoch faults;
- preserve exact state at every tested fuel boundary.

Gate: Repeated reserve operations improve by more than five times.

Gate: `ListReserve` and `ListReorder` disappear from treatment gaps.

Gate: A reserve operation with sufficient capacity calls no runtime function.

### Stage F14: Exhaustive opcode organization

- add one exhaustive opcode ledger;
- request verified state at every instruction boundary;
- derive fuel, replay, fault, and exit shapes from verified state;
- remove the independent planner stack simulation;
- remove the second inline-call emitter;
- compare direct and scheduler-driven corpus results.

Gate: A new opcode causes a compile error until the ledger classifies it.

Gate: Both corpus paths match Interpreter state and host observations.

Gate: The structural change preserves existing warm performance.

### Stage F15: Direct immutable and collection views

- compile map length and epoch operations directly;
- compile digest comparison through direct immutable reads;
- compile UTF-8 byte access and boundary tests directly;
- validate callback conversion with one closure guard;
- keep each failure on one exact replay exit.

Gate: Common map, digest, and UTF-8 reads call no runtime function.

Gate: Direct and scheduler corpus results match Interpreter results.

Gate: Representative Auto performance does not regress.

### Stage F16: Direct scalar text access

- compile scalar-index text access as guarded native code;
- scan immutable UTF-8 data without a runtime function;
- retain the exact replay exit for invalid indexes;
- measure scalar traversal with its surrounding calls.

Gate: `TextAt` has one dedicated guarded treatment.

Gate: Direct and scheduler corpus results match Interpreter results.

Gate: Auto mode demotes call-heavy scalar traversal without regression.

### Stage F17: Direct virtual dispatch

- expose immutable class dispatch rows to native execution;
- derive the receiver class through direct value guards;
- resolve the selector from the class dispatch row;
- use the existing native calling convention;
- promote a complete missing dynamic callee through the Auto compile path;
- preserve exact scheduler retirement counts.

Gate: A polymorphic virtual-call loop uses no interpreter exit.

Gate: Direct and scheduled corpus results match Interpreter results.

Gate: Scalar text traversal improves in Auto and Native modes.

### Stage F18: Interface dispatch caches

- derive one stable receiver key from the value ABI;
- cache targets by call site, parent environment, and receiver key;
- keep each cache in the machine that owns its native continuation;
- resolve a cold miss before the call retires;
- resume the same native instruction after cache publication;
- use the existing native calling convention after each cache hit.

Gate: One polymorphic interface loop uses no interpreter exit.

Gate: Direct and scheduled corpus results match Interpreter results.

Gate: Cold misses preserve exact scheduler retirement counts.

### Stage F19: Generic virtual dispatch caches

- derive the receiver class environment through the direct heap ABI;
- include the parent environment and receiver environment in the cache key;
- resolve each cold miss with the interpreter's generic dispatch rules;
- cache the exact target and method environment;
- resume the same native instruction after cache publication;
- use the existing native calling convention after each cache hit.

Gate: One polymorphic generic virtual loop uses no interpreter exit.

Gate: Direct and scheduled corpus results match Interpreter results.

Gate: Cold misses preserve exact scheduler retirement counts.

### Stage F20: Native closure calls

- store one closure or callback handle in each native frame;
- read closure targets and environments through the stable heap ABI;
- resolve callback slots through one machine-local call-site cache;
- retain capture handles across calls and scheduler suspension;
- remove the callable before the child frame starts;
- preserve exact stack-limit and materialization state.

Gate: A captured closure loop uses no interpreter call exit.

Gate: Direct and scheduled closure results match Interpreter results.

Gate: Closure calls improve in Auto and Native modes.

Closure and callback creation remain later coverage work.

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

The Stage F7 run added direct text metadata and integer hash mixing.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native coverage |
| --- | ---: | ---: | ---: | ---: | ---: |
| Text metadata | 134.918 ms | 29.616 ms | 29.458 ms | 4.56 times | 100.00% |
| JSON parse | 45.652 ms | 46.568 ms | 67.168 ms | 0.98 times | 5.23% |
| HTTP parse | 43.319 ms | 44.206 ms | 60.916 ms | 0.98 times | 46.88% |

The text row uses exact String and Substring functions.

The measured loop reaches complete native coverage.

Its `ConstStr`, `CallG`, and `TupleNew` sites run only during setup.

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

The Stage F8 run added canonical tagged values and direct `Option` operations.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native coverage |
| --- | ---: | ---: | ---: | ---: | ---: |
| Option values | 232.038 ms | 22.014 ms | 23.119 ms | 10.54 times | 100.00% |
| JSON parse | 45.780 ms | 47.849 ms | 72.378 ms | 0.96 times | 5.23% |
| HTTP parse | 44.832 ms | 46.995 ms | 66.870 ms | 0.95 times | 47.30% |

The `Option` row used no temporary interpreter exit.

External invalid payloads replay through the interpreter and preserve `TypeMismatch`.

JSON and HTTP remain within five percent in Auto mode.

The representative-program gate remains open.

The Stage F9 run added direct cached literals and the terminal fault exit.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native coverage |
| --- | ---: | ---: | ---: | ---: | ---: |
| Literal loads | 39.812 ms | 4.543 ms | 4.550 ms | 8.76 times | 100.00% |
| JSON parse | 45.423 ms | 46.615 ms | 72.063 ms | 0.97 times | 5.23% |
| HTTP parse | 42.364 ms | 45.511 ms | 65.743 ms | 0.93 times | 47.30% |

Each machine uses one slow exit for each first literal load.

Later loads read the canonical literal table directly.

The parser profiles contain no literal or `Unreachable` gap.

`CallG` is now the largest gap in both parsers.

The HTTP Auto row does not pass the five-percent gate.

The Stage F10 run added generic direct calls and one machine-local environment cache.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native coverage |
| --- | ---: | ---: | ---: | ---: | ---: |
| Generic direct calls | 53.957 ms | 15.290 ms | 15.069 ms | 3.53 times | 100.00% |
| JSON parse | 46.514 ms | 46.850 ms | 80.981 ms | 0.99 times | 6.33% |
| JSON encode | 20.591 ms | 20.922 ms | 42.236 ms | 0.98 times | 51.76% |
| HTTP parse | 42.607 ms | 46.681 ms | 70.908 ms | 0.91 times | 48.23% |
| HTTP encode | 22.803 ms | 25.303 ms | 25.686 ms | 0.90 times | 41.78% |

The generic row used nine setup exits and no cache fallback.

Each parser used at most 27 setup exits across all measured rounds.

The cache persists across scheduler turns and temporary interpreter sites.

The parser profiles contain no `CallG` gap.

Builder, collection, text, and virtual-call operations now dominate the gaps.

The representative-program performance gate remains open.

The Stage F11 run added generic allocation and optional list reads.

| Workload | Interpreter | Native warm | Warm gain |
| --- | ---: | ---: | ---: |
| Optional list reads | 188.287 ms | 4.614 ms | 40.81 times |
| Generic allocation | 9.668 ms | 4.536 ms | 2.13 times |

Generic allocation used one typed allocation path.

Each allocated generic instance kept its exact `TypeEnvId` witness.

Optional list reads used no runtime helper.

The representative profiles contain no `NewG` or `ListGet` gap.

Representative timing did not improve because other operations dominate these programs.

HTTP Auto performance still does not pass the five-percent gate.

The Stage F12 run added direct list append and typed growth.

| Workload | Interpreter | Native warm | Warm gain |
| --- | ---: | ---: | ---: |
| List append | 4.373 ms | 0.253 ms | 17.29 times |

The common append path called no runtime function.

Capacity growth and collection used one fixed typed function.

The parser profiles contain no `ListPush` gap.

Representative timing remained stable because other operations dominate these programs.

The Stage F13 run added direct list reserve and reorder operations.

| Workload | Interpreter | Native warm | Warm gain |
| --- | ---: | ---: | ---: |
| List reserve | 44.189 ms | 1.512 ms | 29.23 times |

The common reserve path called no runtime function.

Capacity growth and collection used one fixed typed function.

The list-sort profile contains no `ListReserve` or `ListReorder` gap.

List-sort Auto performance changed from 0.915 times to 0.918 times.

`CallInterface` now dominates the list-sort treatment gaps.

The Stage F14 run centralized opcode treatments and verified program-point state.

Direct and scheduled runs matched Interpreter results across 163 shipped programs.

The complete debug workspace suite took 48.763 seconds.

The release scalar results remained within the prior variance.

| Workload | Interpreter | Native warm | Warm gain |
| --- | ---: | ---: | ---: |
| Integer loop | 33.235 ms | 1.124 ms | 29.56 times |
| Factorial | 5.340 ms | 1.234 ms | 4.33 times |
| Direct scalar call | 48.850 ms | 10.148 ms | 4.81 times |
| Branch-bearing call | 53.276 ms | 10.523 ms | 5.06 times |
| Scheduled integer loop | 35.718 ms | 3.999 ms | 8.93 times |

JSON parse remained at 0.975 times in Auto mode.

HTTP parse remained at 0.907 times in Auto mode.

The representative-program performance gate remains open.

The Stage F15 run added direct map, digest, callback, and UTF-8 treatments.

The text ABI now exposes one stable visible-data pointer.

This pointer replaced a redundant byte offset.

The object payload stayed within its 64-byte limit.

Direct and scheduled corpus runs matched Interpreter results across 163 shipped programs.

The focused JIT suite passed 85 tests.

| Workload | Auto gain | Native coverage |
| --- | ---: | ---: |
| List iteration | 7.755 times | 100.00 percent |
| JSON parse | 0.969 times | 6.33 percent |
| JSON stringify | 0.892 times | 51.76 percent |
| HTTP parse | 0.925 times | 48.23 percent |
| HTTP serialize | 0.913 times | 41.78 percent |

These treatments removed no dominant representative gap.

The representative-program performance gate remains open.

The Stage F16 run added direct scalar-index UTF-8 access.

Direct and scheduled corpus runs matched Interpreter results across 163 shipped programs.

The focused JIT suite passed 86 tests.

Scalar traversal reached 98.32 percent Native coverage.

Forced Native ran at 0.584 times Interpreter speed.

Nine million temporary call-family exits dominated that result.

Auto mode demoted the function and ran at 0.975 times Interpreter speed.

This result makes call-family coverage the next structural stage.

The Stage F17 run added direct virtual dispatch through immutable class rows.

Auto promotes a complete missing dynamic callee after a compiled caller reaches it.

The focused JIT suite passed 88 tests.

Direct and scheduled corpus runs matched Interpreter results across 163 shipped programs.

The scheduled corpus comparison includes exact retired instruction counts.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native coverage |
| --- | ---: | ---: | ---: | ---: | ---: |
| Virtual calls | 67.121 ms | 17.629 ms | 17.524 ms | 3.81 times | 100.00 percent |
| Scalar text traversal | 311.182 ms | 64.348 ms | 64.599 ms | 4.84 times | 100.00 percent |
| JSON stringify | 20.779 ms | 22.226 ms | 40.901 ms | 0.94 times | 51.94 percent |
| HTTP parse | 43.924 ms | 47.325 ms | 73.023 ms | 0.93 times | 48.23 percent |
| HTTP serialize | 23.044 ms | 25.563 ms | 25.839 ms | 0.90 times | 41.78 percent |

The representative-program performance gate remains open.

`CallInterface` remains the largest call-family gap.

The Stage F18 run added machine-local polymorphic interface caches.

Each key includes the call site, parent environment, and receiver shape.

A cold miss resolves through the verified interface witness.

The miss does not retire the interface call.

Native execution resumes the same instruction after cache publication.

The focused JIT suite passed 90 tests.

Direct and scheduled corpus runs matched Interpreter results across 163 shipped programs.

The scheduled corpus comparison includes exact retired instruction counts.

The warm debug workspace suite took 46.05 seconds.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native coverage |
| --- | ---: | ---: | ---: | ---: | ---: |
| Polymorphic interface calls | 141.171 ms | 26.417 ms | 26.078 ms | 5.34 times | 100.00 percent |

The representative-program performance gate remains open.

The Stage F19 run added generic virtual calls to the resolved-call cache.

The cache key includes the call site, parent environment, class, and class environment.

A cold miss uses the interpreter's exact generic dispatch rules.

The miss does not retire the generic virtual call.

Native execution resumes the same instruction after cache publication.

The focused JIT suite passed 92 tests.

Direct and scheduled corpus runs matched Interpreter results across 163 shipped programs.

The direct and scheduled corpus gate took 8.92 seconds after compilation.

The warm debug workspace suite took 44.59 seconds.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native coverage |
| --- | ---: | ---: | ---: | ---: | ---: |
| Generic virtual calls | 62.757 ms | 17.349 ms | 16.806 ms | 3.62 times | 100.00 percent |

Generic field-result specialization remains outside this stage.

The representative-program performance gate remains open.

The Stage F20 run added native closure frames and `CallValue` dispatch.

Each native frame now retains its exact closure or callback handle.

Heap closures load their function and environment through the stable heap ABI.

Callback slots resolve through a machine-local call-site cache.

Direct and scheduled closure tests matched Interpreter state.

Stack-limit exits preserved the exact callable and argument state.

The direct and scheduled corpus paths now use separate fresh engines.

The fresh-engine debug corpus gate took 11.75 seconds after compilation.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native coverage |
| --- | ---: | ---: | ---: | ---: | ---: |
| Captured closure calls | 65.887 ms | 67.699 ms | 17.247 ms | 0.97 times | 100.00 percent |

Native gained 3.82 times on the scheduled closure row.

Auto retired no measured native instruction on this row.

The Auto closure performance gate remains open.

### Stage F21: Native capture allocation

- keep one immutable capture-array view in each native frame;
- load closure and callback captures through the same frame view;
- allocate closures through one fixed typed helper;
- allocate callback descriptors through one fixed typed helper;
- stage captures and complete collection roots in one bounded buffer;
- replay nested collection before it changes state;
- record dynamic post-pop fault stacks in the region plan.

Gate: Closure and callback creation use no temporary interpreter site.

Gate: Direct and scheduled corpus results match Interpreter results.

Gate: Fuel and heap-limit exits preserve exact canonical state.

The focused JIT suite passed 99 tests.

The fresh-engine corpus gate took 11.82 seconds after compilation.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native gain | Native coverage |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Captured closure calls | 66.528 ms | 70.356 ms | 16.682 ms | 0.95 times | 3.99 times | 100.00 percent |

The callback path used no compiled temporary site.

Auto retired no measured native instruction on the closure row.

The Auto closure performance gate remains open.

JSON and HTTP remain below the representative gate.

### Stage F22: Native collection literal allocation

- allocate tuple values with one fixed typed helper;
- allocate list values with one fixed typed helper;
- allocate map values with one fixed typed helper;
- stage items and complete collection roots in one bounded buffer;
- preserve duplicate-key replacement during map construction;
- replay nested collection before it changes state;
- record each dynamic post-pop fault stack.

Gate: Tuple, list, and map literals use no temporary interpreter site.

Gate: Direct and scheduled corpus results match Interpreter results.

Gate: Fuel and heap-limit exits preserve exact canonical state.

The focused JIT suite passed 104 tests.

The fresh-engine corpus gate took 11.94 seconds after compilation.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native gain | Native coverage |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| JSON parse | 45.585 ms | 46.890 ms | 79.711 ms | 0.97 times | 0.57 times | 6.38 percent |
| JSON stringify | 20.592 ms | 20.920 ms | 41.663 ms | 0.98 times | 0.49 times | 52.07 percent |
| HTTP parse | 42.541 ms | 46.845 ms | 72.561 ms | 0.91 times | 0.59 times | 48.23 percent |
| HTTP serialize | 22.649 ms | 25.689 ms | 25.960 ms | 0.88 times | 0.87 times | 41.78 percent |

This stage removed literal allocation exits.

The representative gate remains open because hot operations still use temporary sites.

### Stage F23: Typed map lookups

- use one fixed typed helper for `MapHas`;
- use one fixed typed helper for `MapAt`;
- keep map entries in native frames across each helper call;
- return helper faults through one explicit guest-fault exit;
- validate loaded values against verifier contracts;
- keep unexpected state on the replay path.

Gate: Map lookup loops use no temporary interpreter site.

Gate: A missing key preserves the exact fault state.

Gate: Direct and scheduled corpus results match Interpreter results.

The focused JIT suite passed 107 tests.

The fresh-engine corpus gate took 11.87 seconds after compilation.

| Workload | Interpreter | Native cold | Native warm | Native gain |
| --- | ---: | ---: | ---: | ---: |
| Map lookup | 91.200 ms | 37.039 ms | 31.128 ms | 2.93 times |

The representative JSON and HTTP rows did not change beyond measurement variance.

Their hot functions still contain other temporary sites.

### Stage F24: Typed map insertion

- use one fixed typed helper when the result is discarded;
- probe before mutation when the program uses the previous value;
- validate the previous value against its verifier contract;
- commit only after the validation succeeds;
- bind each probe token to the unchanged map entry count;
- preserve frozen, heap-limit, and exact fuel exits;
- replay malformed external values before native mutation.

Gate: Map insertion loops use no temporary interpreter site.

Gate: Used and discarded results match Interpreter results.

Gate: Direct and scheduled corpus results match Interpreter results.

The focused JIT suite passed 111 tests.

The direct and scheduled corpus gate took 12.03 seconds after compilation.

| Workload | Interpreter | Native cold | Native warm | Native gain |
| --- | ---: | ---: | ---: | ---: |
| Map insertion | 5.213 ms | 7.711 ms | 4.374 ms | 1.19 times |

Map insertion uses semantic hashing and the derived map index.

The fixed helper keeps this complex work outside generated code.

The representative gate remains open.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native gain | Native coverage |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| JSON parse | 45.735 ms | 46.908 ms | 80.458 ms | 0.98 times | 0.57 times | 6.38 percent |
| JSON stringify | 21.659 ms | 21.917 ms | 42.611 ms | 0.99 times | 0.51 times | 52.07 percent |
| HTTP parse | 43.538 ms | 48.323 ms | 73.675 ms | 0.90 times | 0.59 times | 48.23 percent |
| HTTP serialize | 22.852 ms | 26.204 ms | 26.467 ms | 0.87 times | 0.86 times | 41.78 percent |

### Stage F25: Value comparison and hashing

- inline unequal tags, scalar equality, and identical object references;
- call one fixed typed helper for recursive structural equality;
- use the interpreter's structural equality implementation;
- keep the native frame active during each equality walk;
- use one separate fixed helper for `ListContains`;
- compare text through one fixed typed ordering helper;
- compare bytes through one fixed typed ordering helper;
- hash text and bytes through fixed typed helpers;
- preserve exact fuel, fault, and scheduler state.

Gate: Structural enum and tuple equality uses no temporary interpreter site.

Gate: List containment uses the same structural equality rule.

Gate: All text and byte comparisons use no temporary interpreter site.

Gate: Direct and scheduled corpus results match Interpreter results.

The focused JIT suite passed 116 tests.

The direct and scheduled corpus gate took 12.03 seconds after compilation.

| Workload | Interpreter | Native cold | Native warm | Native gain |
| --- | ---: | ---: | ---: | ---: |
| Structural equality | 19.775 ms | 20.451 ms | 10.099 ms | 1.96 times |
| Text and byte comparison | 20.242 ms | 25.622 ms | 7.421 ms | 2.73 times |

The batch completed 17 opcode treatments.

The ledger now has 118 temporary treatments across 260 concrete operations.

The representative gate remains open.

| Workload | Auto gain | Native coverage |
| --- | ---: | ---: |
| List sort | 1.01 times | 31.20 percent |
| JSON parse | 0.97 times | 6.38 percent |
| JSON stringify | 0.98 times | 52.07 percent |
| HTTP parse | 0.90 times | 48.23 percent |
| HTTP serialize | 0.87 times | 41.78 percent |

### Stage F26: Graph freeze and digest

- freeze object graphs through one fixed typed helper;
- digest typed graphs through one separate fixed helper;
- keep the native activation active during successful graph walks;
- pass all native object roots to digest allocation;
- reuse the world type and identity implementation;
- replay failed graph operations before mutation;
- preserve graph limits and exact fault messages.

Gate: Freeze and Digest use no temporary interpreter site.

Gate: Direct and scheduled corpus results match Interpreter results.

The focused JIT suite passed 118 tests.

The direct and scheduled corpus gate took 11.99 seconds after compilation.

| Workload | Interpreter | Native cold | Native warm | Native gain |
| --- | ---: | ---: | ---: | ---: |
| Freeze and Digest | 3.442 ms | 17.205 ms | 1.879 ms | 1.83 times |

The batch completed two opcode treatments.

The ledger now has 116 temporary treatments across 260 concrete operations.

The representative gate remains open.

| Workload | Auto gain | Native coverage |
| --- | ---: | ---: |
| List sort | 1.01 times | 31.20 percent |
| JSON parse | 0.97 times | 6.38 percent |
| JSON stringify | 0.99 times | 52.07 percent |
| HTTP parse | 0.86 times | 48.23 percent |
| HTTP serialize | 0.83 times | 41.78 percent |

### Stage F27: Direct list mutations

- pop values through direct guarded array access;
- insert values through an inline move and one growth path;
- remove values through direct ordered or swapped moves;
- truncate lists through direct length and charge updates;
- preserve structural epochs for every mutation;
- preserve exact option values, faults, and fuel state.

Gate: All five list mutations use no temporary interpreter site.

Gate: Direct and scheduled corpus results match Interpreter results.

The focused JIT suite passed 119 tests.

The direct and scheduled corpus gate took 11.94 seconds after compilation.

| Workload | Interpreter | Native cold | Native warm | Native gain |
| --- | ---: | ---: | ---: | ---: |
| List mutations | 34.140 ms | 19.276 ms | 2.058 ms | 16.59 times |

The batch completed five opcode treatments.

The ledger now has 111 temporary treatments across 260 concrete operations.

### Stage F28: Map mutation and iteration

- run map hashing and probes through fixed typed helpers;
- keep successful map operations inside the native activation;
- validate raw probe tokens with the interpreter rules;
- compile `MapProbeFound` as integer tests;
- compile `MapWriteGuard` as a direct object guard;
- preserve map order, epochs, tombstones, and compaction;
- pass complete native roots to operations that can collect;
- preserve exact option values, faults, and fuel state.

No helper dispatches on an opcode number.

Gate: All 15 map mutation and iteration operations use dedicated treatments.

Gate: Native and Interpreter results match for native and user-defined map keys.

Gate: Direct and scheduled corpus results match Interpreter results.

The focused JIT suite passed 121 tests.

The direct and scheduled corpus gate took 12.43 seconds after compilation.

| Workload | Interpreter | Native cold | Native warm | Native gain |
| --- | ---: | ---: | ---: | ---: |
| Map mutations | 170.771 ms | 716.662 ms | 123.860 ms | 1.38 times |

Cold compilation covered 18 regions in this broad generic-map workload.

The batch completed 15 opcode treatments.

The ledger now has 96 temporary treatments across 260 concrete operations.

The representative gate remains open.

| Workload | Auto gain | Native coverage |
| --- | ---: | ---: |
| List sort | 1.02 times | 31.20 percent |
| JSON parse | 0.98 times | 6.38 percent |
| JSON stringify | 0.98 times | 52.07 percent |
| HTTP parse | 0.89 times | 48.23 percent |
| HTTP serialize | 0.86 times | 41.78 percent |

### Stage F29: Builders and byte construction

- give mutable byte arrays one stable heap layout;
- compile builder metadata and clear operations as guarded memory access;
- compile bounded appends through direct fast paths;
- use fixed typed helpers for growth and complex construction;
- preserve UTF-8 metadata, heap charges, faults, and finished states;
- grow shared root buffers before a native callee can exceed them;
- preserve exact fuel and scheduler state.

No helper dispatches on an opcode number.

Gate: All 29 builder and byte-construction operations use dedicated treatments.

Gate: Direct and scheduled corpus results match Interpreter results.

The focused JIT suite passed 126 tests.

The direct and scheduled corpus gate took 13.00 seconds after compilation.

| Workload | Interpreter | Native cold | Native warm | Native gain |
| --- | ---: | ---: | ---: | ---: |
| String builder | 10.255 ms | 7.800 ms | 0.902 ms | 11.37 times |
| Byte buffer | 8.871 ms | 10.758 ms | 8.961 ms | 0.99 times |
| Byte construction | 10.157 ms | 20.132 ms | 9.990 ms | 1.02 times |

The batch completed 29 opcode treatments.

The ledger now has 67 temporary treatments across 260 concrete operations.

The representative gate remains open.

| Workload | Auto gain | Native coverage |
| --- | ---: | ---: |
| List sort | 0.96 times | 31.20 percent |
| JSON parse | 0.96 times | 6.57 percent |
| JSON stringify | 1.00 times | 56.54 percent |
| HTTP parse | 0.89 times | 48.53 percent |
| HTTP serialize | 0.87 times | 41.81 percent |

### Stage F30: Text, byte, and numeric conversion

- compile fixed-cost text and byte queries through typed helpers;
- compile allocating transformations through typed helpers;
- preserve exact heap charges and collection roots;
- preserve UTF-8, parsing, formatting, and fault behavior;
- preserve exact fuel and scheduler state.

No helper dispatches on an opcode number.

Gate: All 33 text, byte, and numeric conversion operations use dedicated treatments.

Gate: Direct and scheduled corpus results match Interpreter results.

The focused JIT suite passed 129 tests.

The direct and scheduled corpus gate took 12.92 seconds after compilation.

| Workload | Interpreter | Native cold | Native warm | Native gain |
| --- | ---: | ---: | ---: | ---: |
| Text search | 88.386 ms | 79.967 ms | 43.038 ms | 2.05 times |
| Text transformation | 6.227 ms | 16.288 ms | 6.787 ms | 0.92 times |
| Numeric conversion | 36.904 ms | 100.922 ms | 77.980 ms | 0.47 times |

The numeric row crosses many short native regions in each source operation.

The batch completed 33 opcode treatments.

The ledger now has 34 temporary treatments across 260 concrete operations.

The representative gate remains open.

| Workload | Auto gain | Native coverage |
| --- | ---: | ---: |
| List sort | 1.00 times | 31.20 percent |
| JSON parse | 0.97 times | 6.92 percent |
| JSON stringify | 1.07 times | 56.72 percent |
| HTTP parse | 0.89 times | 48.94 percent |
| HTTP serialize | 0.86 times | 41.81 percent |

### Stage F31: Dynamic calls and observable boundaries

- call current function and constructor slots through the native call convention;
- read slot targets from one compact machine view;
- create and inspect faults through fixed typed helpers;
- materialize state before world, effect, and fault boundaries;
- execute each boundary instruction once in the interpreter;
- preserve exact fuel, fault, and scheduler state.

The boundary exit is a permanent class F treatment.

It is not a temporary interpreter site.

Gate: All 14 dynamic call, slot, effect, and fault operations use dedicated treatments.

Gate: Direct and scheduled corpus results match Interpreter results.

The focused JIT suite passed 132 tests.

The direct and scheduled corpus gate took 12.87 seconds after compilation.

| Workload | Interpreter | Auto warm | Native warm | Auto gain | Native gain | Native coverage |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Late-bound slot call | 156.854 ms | 83.028 ms | 82.892 ms | 1.89 times | 1.89 times | 100.00 percent |

The batch completed 14 opcode treatments.

The ledger now has 20 temporary treatments across 260 concrete operations.

The representative gate remains open.

| Workload | Auto gain | Native coverage |
| --- | ---: | ---: |
| List sort | 0.97 times | 31.20 percent |
| JSON parse | 0.98 times | 6.92 percent |
| JSON stringify | 1.08 times | 56.72 percent |
| HTTP parse | 0.87 times | 48.94 percent |
| HTTP serialize | 0.81 times | 41.81 percent |

### Stage F32: Syntax, dynamic values, and code boundaries

- compile syntax queries and builders through fixed typed helpers;
- compile `DynPack` through one typed allocation helper;
- materialize before dynamic rendering and reflective code inspection;
- remove the temporary opcode treatment state;
- make the backend opcode match exhaustive.

Gate: Every concrete opcode has one production treatment.

Gate: Direct and scheduled corpus results match Interpreter results.

The focused JIT suite passed 134 tests.

The corpus gate covered more than 160 programs in 12.72 seconds.

The batch completed 20 opcode treatments.

All 260 concrete operations now have production treatments.

The ledger has no temporary treatment.

| Workload | Auto gain | Native coverage |
| --- | ---: | ---: |
| List sort | 1.06 times | 31.20 percent |
| JSON parse | 0.99 times | 6.92 percent |
| JSON stringify | 1.06 times | 56.72 percent |
| HTTP parse | 0.86 times | 48.94 percent |
| HTTP serialize | 0.82 times | 41.81 percent |

The representative gate remains open.

### Stage F33: Complete function acceptance

- give every `BcType` one native value representation;
- accept generic instance fields through their relocated class;
- remove unreachable segments before native control-flow analysis;
- load tagged payloads without reading Rust padding bytes;
- preserve exact planner rejection reasons;
- compile every unique verified corpus function;
- require zero unsupported fallbacks in direct and scheduled runs.

Gate: Every unique verified corpus function compiles without `Unsupported`.

Gate: Direct and scheduled corpus runs match Interpreter results.

Gate: Direct and scheduled corpus runs report zero unsupported fallbacks.

The focused JIT suite passed 134 tests.

The full corpus gate passed in 61.93 seconds in the debug workspace profile.

The full workspace test suite passed.

The representative performance gate remains open.

### Stage F34: Measure complete-program performance

- preserve the complete function-acceptance gate;
- measure native entries, exits, helpers, materializations, and compilation for each representative row;
- remove repeated work from hot native entry and exit paths;
- keep class F exits only at observable runtime boundaries;
- keep common operations in classes A through D;
- remeasure every language benchmark and large corpus program;
- record JSON and HTTP ratios without using them as completion criteria.

Regression limit: Auto slows no large corpus program by more than five percent.

Diagnostic target: At least one JSON row improves by more than two times.

Diagnostic target: At least one HTTP row improves by more than two times.

Checkpoint: Native collection can inspect roots from every suspended native frame.

Nested builder completion and collection stay native.

The planner reads temporary generic types from the verified type table.

One immutable helper table serves each native runtime implementation.

The focused JIT suite passed 136 tests.

The corpus gate passed in 64.50 seconds in the debug workspace profile.

It compiled every unique verified corpus function.

Direct and scheduled forced-native runs matched Interpreter results.

No corpus run reported an unsupported fallback.

All representative forced-native rows reached complete native coverage.

| Workload | Auto gain | Native gain |
| --- | ---: | ---: |
| Slot call | 1.91 times | 1.92 times |
| Deep recursion | 1.07 times | 1.07 times |
| Call with branch | 3.67 times | 3.66 times |
| Virtual call | 3.62 times | 3.69 times |
| Interface call | 5.03 times | 5.07 times |
| Generic call | 3.66 times | 3.65 times |
| Closure call | 0.97 times | 3.82 times |
| Numeric surface | 10.31 times | 10.55 times |
| Option values | 7.89 times | 7.94 times |
| List sort | 2.81 times | 2.84 times |
| JSON parse | 1.74 times | 1.73 times |
| JSON stringify | 1.75 times | 1.75 times |
| HTTP parse | 1.48 times | 1.48 times |
| HTTP serialize | 2.46 times | 2.48 times |

No representative Auto row slowed by more than five percent.

HTTP serialization crossed its local diagnostic target.

JSON remained below its local diagnostic target.

Auto did not promote the closure row.

Deep recursion remains close to Interpreter performance under scheduler slices.

### Stage F35: Bound native code by bytes

- reuse one immutable Cranelift target configuration;
- measure emitted code bytes for every region;
- replace the fixed region limit with a host-owned byte budget;
- keep capacity refusal separate from permanent compiler verdicts;
- keep ready-region lookup lock-free;
- add cold-start and many-function benchmark gates.

Gate: More than 256 hot functions can compile when the byte budget permits them.

Gate: Capacity refusal reports no unsupported function.

The many-function gate compiled all 301 regions.

Auto improved from 0.94 times to 1.09 times.

Forced Native improved from 0.34 times to 1.09 times.

Both modes reached complete native coverage.

The cold JSON parse compiled 26 regions and 6,967,925 machine-code bytes.

Its first Auto run took 1,425.51 milliseconds.

Cold compilation remains the largest open performance defect.

The corpus differential passed in 65.45 seconds.

The full workspace suite passed.

### Stage F36: Specialize static scalar tags

- derive each local tag from its verified `ScalarKind`;
- load dynamic tags only for `Tagged` and `Callback` locals;
- derive stack tag storage from all verified stack shapes;
- keep canonical tag writes at each materialization point;
- preserve exact fuel, faults, snapshots, and scheduler continuation;
- remeasure all representative scheduler programs.

Gate: Direct and scheduled corpus results match Interpreter results.

Gate: Every representative forced-native row keeps complete native coverage.

The focused JIT suite passed 137 tests.

The direct and scheduled corpus differential passed in 52.13 seconds.

| Workload | Auto gain | Native gain |
| --- | ---: | ---: |
| Slot call | 6.81 times | 6.43 times |
| Deep recursion | 0.98 times | 0.99 times |
| Call with branch | 17.24 times | 17.66 times |
| Interface call | 28.05 times | 28.58 times |
| Numeric surface | 17.53 times | 18.10 times |
| Option values | 29.07 times | 29.54 times |
| List sort | 9.73 times | 9.81 times |
| JSON parse | 4.81 times | 4.86 times |
| JSON stringify | 3.33 times | 3.34 times |
| HTTP parse | 4.10 times | 4.12 times |
| HTTP serialize | 5.74 times | 5.78 times |

The JSON and HTTP warm rows improved.

These gains do not close native optimization.

Deep recursion remains close to Interpreter performance.

The cold JSON run took 1,811.88 milliseconds.

It compiled 26 regions and 6,236,954 machine-code bytes.

Cold compilation remains open.

The next work compares native kernels with optimized Rust ceilings.

It removes structural costs before it changes hotness policy.

### Stage F37: Reduce fuel accounting density

- keep one running fuel counter;
- derive retired instructions only at engine exits;
- reserve one maximum acyclic control-flow path;
- bypass interior fuel checks after a successful reserve check;
- end each reserve region at a backedge or safepoint;
- keep exact instruction paths for small remaining fuel;
- place exact and fault paths in cold machine-code blocks.

Gate: Every direct and scheduled fuel sweep matches Interpreter state.

The focused JIT suite passed 137 tests.

The direct and scheduled corpus differential passed in 56.17 seconds.

The same-session comparison used commit `8e6b257` as its parent baseline.

| Scalar path | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| Direct native warm | 1.005 ms | 0.551 ms | 45.2 percent faster |
| Scheduled native warm | 3.919 ms | 3.696 ms | 5.7 percent faster |

The direct scalar gain rose from 34.05 times to 57.97 times.

List sorting improved by 6.2 percent in the broad scheduler sample.

JSON parse stayed within two percent of the parent measurement.

HTTP parse improved by 2.2 percent.

These measurements do not close the optimized Rust ceiling gap.

### Stage F38: Carry heap proofs across loop backedges

- assign one optional proof cache to each object-bearing local;
- validate the slot, generation, live flag, object kind, and class on the first access;
- carry the validated entry and class proof across native loop backedges;
- invalidate every proof when `StoreLocal` changes its source local;
- keep payload pointers outside this stage;
- classify heap, table, and activation memory for backend alias analysis.

Correctness check: Every focused JIT test passed.

Correctness check: Direct and scheduled corpus runs matched Interpreter results.

The corpus reported no unsupported fallback.

The same-session comparison used commit `80c72ea` as its parent baseline.

| Direct native path | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| Field read | 1.526 ms | 1.011 ms | 33.7 percent faster |
| Tuple read | 1.230 ms | 0.901 ms | 26.7 percent faster |
| List read | 1.674 ms | 1.180 ms | 29.5 percent faster |
| List replacement | 1.619 ms | 1.102 ms | 31.9 percent faster |
| Scalar loop | 0.552 ms | 0.541 ms | 2.0 percent faster |

These results isolate repeated object translation as a material heap cost.

The next stages retain safe payload addresses and inline remaining builder capacity hits.

### Stage F39: Inline builder capacity hits

- mark immutable instance and tuple array pointers as movable, read-only heap values;
- compile integer formatting directly into native digit stores;
- compile character UTF-8 encoding directly into native byte stores;
- keep typed growth helpers for insufficient capacity;
- reload payload pointers after every runtime or native call;
- add fuel tests for signed integer bounds and all UTF-8 widths.

The focused builder tests passed.

The same-session comparison used commit `208c632` as its parent baseline.

| Direct native path | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| Integer builder append | 3.550 ms | 1.048 ms | 70.5 percent faster |
| Character builder append | 2.828 ms | 0.311 ms | 89.0 percent faster |
| Text builder append | 0.706 ms | 0.705 ms | unchanged |
| Byte builder append | 0.327 ms | 0.328 ms | unchanged |

An explicit runtime payload cache regressed field and element loops.

The implementation removed that cache.

These changes do not complete payload-address optimization.

### Stage F40: Expose the canonical map layout

- replace the map-entry `Vec` with one stable owned array;
- replace the map-slot boxed slice with one stable owned slice;
- keep map entries and lookup slots as the only source of truth;
- preserve the existing object payload size;
- publish data, length, element-size, and field offsets from `lm-heap`;
- keep snapshots, graph walks, and interpreter operations on the same storage.

Gate: Heap tests read entries and lookup slots through every published offset.

Gate: The compact object-size test still passes.

The native map probe does not read this layout yet.

The next stage inlines common map hits over these canonical arrays.

### Stage F41: Inline scalar map hits

- publish one private process hash key through the checked heap view;
- use the same keyed mixer in native and interpreter map probes;
- probe canonical lookup slots without a runtime call;
- compare Unit, Bool, Int, Float, and Char keys in native code;
- load hit values from the canonical entry array;
- keep index rebuilding and complex keys on fixed typed slow paths;
- test collisions, misses, tombstones, float zero, and fuel boundaries.

The same-session comparison used commit `ff2daf2` as its parent baseline.

| Direct native path | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| Integer map hit | 36.532 ms | 4.889 ms | 86.6 percent faster |
| Text map hit | 30.959 ms | 31.533 ms | within variance |

This stage does not complete map optimization.

Text keys, optional reads, and existing-value writes still use typed slow paths.

### Stage F42: Inline text and byte map hits

- publish cached text and byte hash offsets from `lm-heap`;
- validate each key through the canonical object table;
- use cached private hashes for direct map probes;
- compare equal text and byte candidates with one fixed typed helper;
- accept `String` and `Substring` values through the `Text` contract;
- keep uncached hashing and index rebuilding on fixed typed slow paths;
- keep missing `MapAt` faults on the fixed typed slow path.

The same-session comparison used commit `fa78315` as its parent baseline.

| Direct native path | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| Text map hit | 30.698 ms | 7.409 ms | 75.9 percent faster |
| Byte map hit | 16.458 ms | 3.227 ms | 80.4 percent faster |

The direct path uses the canonical map arrays and immutable shared data.

`MapGet` and existing-value `MapPut` still use typed slow paths.

### Stage F43: Inline optional map reads and replacements

- route `MapGet` through the direct canonical probe;
- construct the canonical `None` value after a direct miss;
- return the stored value directly after a hit;
- replace an existing entry value in canonical storage;
- preserve the prior value for result-producing `MapPut`;
- keep insertions, index rebuilding, and complex keys on typed slow paths;
- replay frozen writes through the interpreter fault path.

The same-session comparison used commit `95afc5d` as its parent baseline.

| Direct native path | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| Optional integer map read | 23.568 ms | 11.617 ms | 50.7 percent faster |
| Existing integer map replacement | 35.075 ms | 13.135 ms | 62.6 percent faster |

These paths preserve exact fuel, fault, and external-state behavior.

New-key insertion remains on its typed allocation slow path.

### Stage F44: Inline map insertion within reserved storage

- expose canonical entry capacity and logical entry cost;
- retain the vacant canonical index slot after a direct probe;
- carry the exact semantic and private lookup hashes;
- check entry capacity, index load, epoch, and heap charge before mutation;
- write canonical entries and lookup slots without a runtime call;
- publish entry and index counts after all payload writes;
- keep growth, collection, index rebuilds, and complex keys on typed slow paths;
- preserve exact fuel, faults, heap limits, and external-state behavior.

The focused JIT suite passed 146 tests.

The direct and scheduled corpus differential passed in 56.52 seconds.

The release comparison used commit `bb3e62d` as its exact parent baseline.

| Direct native path | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| New integer map entry | 3.947 ms | 1.717 ms | 56.5 percent faster |

The next stages reduce remaining helper and allocator costs.

Later stages add budgeted direct-call inlining and bounded continuation validation.

Cold compilation and hotness policy remain separate final stages.

### Stage F45: Inline common map removal and iteration paths

- remove scalar, text, and byte keys through the canonical probe;
- keep stable compaction and complex keys on typed slow paths;
- scan canonical map entries for iteration;
- load live map keys and values directly;
- preserve epoch, frozen-write, and external-state behavior;
- route byte equality through one fixed typed function table;
- avoid address-range assumptions in generated code.

The focused map tests and the complete JIT test target passed.

The release comparison used commit `381dac6` as its exact parent baseline.

| Direct native path | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| Map removal and reinsertion | 10.824 ms | 6.505 ms | 39.9 percent faster |
| Map iteration | 13.745 ms | 2.656 ms | 80.7 percent faster |

These measurements expose the removed helper costs.

They do not define a completion threshold.

### Stage F46: Share exact-fuel materialization

- keep one native entry for each exact bytecode position;
- route all depleted-fuel checkpoints to one cold exit block;
- pass live typed values through SSA block arguments;
- spill the canonical frame once in the shared block;
- preserve exact fuel, program positions, and scheduler continuation;
- record cold code size for scalar and complete-program compilation.

The focused JIT suite passed 147 tests.

The release comparison used commit `035163a` as its exact parent baseline.

| Compiled set | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| Scalar loop | 8,944 bytes | 6,784 bytes | 24.2 percent smaller |
| JSON cold set | 7,334,135 bytes | 6,893,690 bytes | 6.0 percent smaller |

Scalar cold execution stayed within measurement variance.

The sampled JSON cold run improved by 7.8 percent.

The exact tail still lowers each bytecode instruction a second time.

Later code-density work must remove this duplicate lowering.

### Stage F47: Retain stable list parameter data

- select list parameters that no native path reassigns;
- reject the proof when one guest call can mutate an alias;
- reject the proof when one list operation can grow storage;
- validate the canonical object once at native entry;
- retain its canonical data pointer across safe loop backedges;
- use no runtime branch at each element access;
- keep exact-fuel code on the checked load path;
- use one typed boundary when external state fails the entry proof;
- measure cold and warm costs separately.

This stage starts payload-address persistence.

Local list proofs and broader loop invariants remain open.

The focused JIT suite passed 148 tests.

The direct and scheduled corpus differential passed in 54.76 seconds.

The release comparison used commit `5883543` as its exact parent baseline.

| List parameter loop | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| Native warm | 1.150 ms | 1.095 ms | 4.8 percent faster |
| Native cold | 7.076 ms | 7.199 ms | within local variance |

The hot access performs no repeated payload-pointer load.

This result does not define a completion threshold.

### Stage F48: Coalesce reserved fuel charges

- identify unique forward segment chains inside one fuel reservation;
- carry one static uncharged cost through those chains;
- charge the aggregate before a join, backedge, call, effect, fault, or return;
- keep an exact canonical entry for every segment start;
- route a mid-chain canonical entry through exact fuel code;
- preserve the existing exact-instruction path for small fuel balances;
- preserve exact state at every tested fuel boundary.

This stage removes fuel writes from unique acyclic paths.

It does not remove the exact-fuel duplicate lowering.

The first entry design exposed mid-chain exact blocks directly.

That design doubled the scalar-loop time after Cranelift optimization.

The final design keeps one canonical segment dispatch and selects the exact path there.

The focused JIT suite passed 148 tests.

The direct and scheduled corpus differential passed in 53.25 seconds.

The release comparison used commit `79d4f33` as its exact parent baseline.

| Native warm row | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| Integer loop | 0.629 ms | 0.575 ms | 8.6 percent faster |
| Integer equality | 0.535 ms | 0.451 ms | 15.7 percent faster |
| Expression stack | 0.913 ms | 0.827 ms | 9.4 percent faster |
| Float addition | 2.350 ms | 2.294 ms | 2.4 percent faster |

These gains do not define a completion threshold.

### Stage F49: Retain stable local list data

- identify list locals in functions with no guest call or list growth;
- cache the canonical data pointer after the first checked list access;
- carry the pointer across safe native loop backedges;
- clear the entry, kind, and data proofs after each local assignment;
- keep parameter entry preloads as the branch-free case;
- keep exact-fuel accesses on the checked path;
- preserve external-state checks and exact fuel behavior.

This stage extends payload-address persistence beyond parameters.

The focused JIT suite passed 149 tests.

The direct and scheduled corpus differential passed in 53.35 seconds.

The release comparison used commit `8470fe2` as its exact parent baseline.

| Local list loop | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| Native warm | 1.390 ms | 1.278 ms | 8.1 percent faster |
| Native cold | 6.258 ms | 6.097 ms | 2.6 percent faster |

The local cache still tests its proof before each access.

Loop versioning can remove that branch for a proved hot loop.

These measurements do not define a completion threshold.

### Stage F50: Split allocation fast and collection paths

- call each typed allocator with only its direct inputs;
- forbid collection on the common call;
- return one dedicated status when collection is required;
- write complete native roots only on that status;
- retry the same typed allocator with collection enabled;
- preserve exact captures, array items, faults, and fuel state;
- count collection slow paths without adding a common-path atomic operation.

Instance allocation passes no roots on its common path.

Closure, tuple, list, and map allocation pass only their direct values.

Callback allocation also passes only its captures.

A small-heap test forced 41 collection retries.

The test preserved closure captures, tuple inputs, and list inputs.

The focused JIT suite passed 150 tests.

The direct and scheduled corpus differential passed in 53.36 seconds.

The release comparison used commit `48334ca` as its exact parent baseline.

| Native warm row | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| Instance allocation | 3.732 ms | 3.621 ms | 3.0 percent faster |
| Generic allocation | 4.864 ms | 4.826 ms | 0.8 percent faster |

The small timing changes do not define this stage.

The completed fast and slow allocation contract defines this stage.

### Stage F51: Keep local initialization in canonical state

- use each native frame's state bytes as the only local initialization state;
- initialize every guarded live local before native entry;
- initialize one dormant local when native code first stores it;
- materialize every initialized root-frame local;
- preserve dormant locals that native code did not read or write;
- remove local-state SSA values from fast and exact-fuel control flow;
- remove local-state values from the shared exact-fuel exit signature.

Liveness proves that a live segment-entry local already has initialized state.

A later `StoreLocal` does not write the same state byte again.

The integer loop contains no local-state load or store on its hot backedge.

The focused JIT suite passed 150 tests.

The dormant-value test preserved a wrong inactive local across native execution.

The direct and scheduled corpus differential passed in 39.28 seconds.

The prior checkpoint took 53.36 seconds in the same debug test configuration.

The release comparison used commit `85ce062` as its exact parent baseline.

| Native warm row | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| Integer loop | 0.578 ms | 0.542 ms | 6.2 percent faster |

The focused scalar group also preserved exact fuel and fault behavior.

This change removes one redundant state representation.

It does not complete the broader code-density work.

### Stage F52: Defer pure-segment overflow faults

- combine checked integer overflow flags inside one pure scalar segment;
- branch once after the segment instead of once after each checked operation;
- replay the segment from its canonical entry after a rare overflow;
- preserve the exact fault position through interpreter replay;
- defer no segment that initializes a dormant local;
- retain immediate checks around calls, heap changes, and other faults;
- load the root body through its published entry address;
- use no relative relocation between separate executable mappings.

The focused JIT suite passed 151 tests.

Two deferred fault positions matched Interpreter at every tested fuel boundary.

The direct and scheduled corpus differential passed in 39.23 seconds.

The integer loop contains one overflow branch for its two checked additions.

The expression loop contains one overflow branch for its four checked operations.

The release comparison used commit `6ef1a1f` as its parent baseline.

| Native warm row | Parent | Current | Change |
| --- | ---: | ---: | ---: |
| Integer loop | 0.695 ms | 0.536 ms | 22.9 percent faster |
| Expression stack | 0.730 ms | 0.707 ms | 3.2 percent faster |

An earlier parent run measured the integer loop at 0.542 ms.

Thus, the integer timing includes material local variance.

The removed hot branch defines this stage.

The root entry change also removed one direct Cranelift relocation.

This relocation could exceed its range during parallel compilation.

The cold integer region now contains 5,916 machine-code bytes.

Exact-fuel lowering still duplicates each instruction in the same function.

The next code-density stage must isolate that cold path without an interpreter transition.

### Stage F53: Preserve roots and test extended turns

- store canonical tags for every suspended native frame slot;
- preserve root payloads for each root-capable slot;
- add one direct JSON collection regression test;
- read top and indexed native frames without a depth scan;
- test a larger quantum for one pure task;
- keep the fixed-quantum corpus differential.

The direct JSON test uses a 32 KiB heap.

The test forces native collection during parsing and encoding.

The test preserved every live native object across collection.

The first implementation used a verified effect row and task existence.

That rule was too specific and is removed in Stage F54.

The fixed-quantum corpus gate continues to test native continuation state.

The focused JIT suite passed 153 tests.

The direct and fixed-quantum corpus differential passed in 39.61 seconds.

The full workspace suite passed in 90.56 seconds.

| Warm row | Before | Current | Change |
| --- | ---: | ---: | ---: |
| Scheduled integer loop | 3.861 ms | 0.542 ms | 86.0 percent faster |
| Scheduled deep recursion | not recorded | 12.502 ms | direct parity |
| Direct deep recursion | 12.452 ms | 12.408 ms | within variance |

The scheduled integer gain rose from 10.88 times to 63.03 times.

Scheduled deep recursion gained 2.77 times.

Direct deep recursion gained 2.79 times.

| Representative row | Interpreter | Auto | Native | Auto gain |
| --- | ---: | ---: | ---: | ---: |
| JSON parse | 43.657 ms | 18.189 ms | 18.371 ms | 2.400 times |
| JSON encode | 20.123 ms | 8.674 ms | 8.759 ms | 2.320 times |
| HTTP parse | 40.695 ms | 17.638 ms | 17.868 ms | 2.307 times |
| HTTP encode | 21.510 ms | 5.719 ms | 5.820 ms | 3.761 times |

Each representative native row reached complete instruction coverage.

These measurements used one stable arena and one shared engine.

They do not close the remaining allocation, helper, inlining, or code-density work.

### Stage F54: Recalibrate the fairness quantum

- remove the pure-task quantum;
- use one default quantum for every task;
- set deterministic and parallel defaults to 16,384 instructions;
- preserve explicit quantum configuration;
- measure message workloads and representative native workloads;
- retain the fixed-quantum corpus differential.

The 1,024-instruction default came from interpreter execution speed.

Native execution made each physical scheduler exit too frequent.

A 16,384-instruction quantum recovers most direct native throughput.

The longest measured slice stays below one millisecond.

Message rows stayed near their previous values.

| Message row | Before deterministic | Current deterministic | Before parallel | Current parallel |
| --- | ---: | ---: | ---: | ---: |
| Ping-pong | 4.471 ms | 4.453 ms | 4.485 ms | 4.462 ms |
| Stream | 0.212 ms | 0.232 ms | 0.214 ms | 0.231 ms |
| Independent pairs | 4.549 ms | 4.536 ms | 4.639 ms | 4.631 ms |
| Many senders | 0.388 ms | 0.405 ms | 0.389 ms | 0.404 ms |
| Allocated stream | 0.179 ms | 0.180 ms | 0.179 ms | 0.181 ms |

| Warm row | Interpreter | Native | Native gain |
| --- | ---: | ---: | ---: |
| Scheduled integer loop | 32.242 ms | 0.756 ms | 42.62 times |
| Scheduled deep recursion | 36.061 ms | 17.601 ms | 2.05 times |

This stage fixes a stale constant.

It does not make a scheduler poll cheap.

Stage F55 separates a poll decision from a physical yield.

### Stage F55: Yield only on coordinator demand

- keep deterministic poll points at the fairness interval;
- expose one coordinator-owned yield request to the active executor;
- rearm execution without materialization when no yield is requested;
- yield when another runnable task or scheduler operation needs service;
- apply one control contract to interpreter and native execution;
- remove every effect-row and task-existence shortcut;
- preserve explicit fixed-quantum execution for tests and hosts;
- measure physical yield cost and scheduler response delay.

The poll result must depend only on deterministic coordinator state.

The executor must not read mutable `World` state.

Snapshot, recall, and stop requests must force the next poll to yield.

The first implementation rearms execution inside the executor.

It skips `World` and coordinator work when no request exists.

It applies the same control to deterministic and parallel execution.

The parallel pool requests a yield when another lease waits.

The scheduler requests a yield when execution creates scheduler work.

The scheduler suite passed 35 tests.

| Warm row | Stage F54 | First F55 pass | Change |
| --- | ---: | ---: | ---: |
| Scheduled integer loop | 0.756 ms | 0.676 ms | 10.6 percent faster |
| Scheduled deep recursion | 17.601 ms | 17.495 ms | within variance |

The first pass still returns from generated code at every poll.

The second pass rearms fuel inside generated code.

This change keeps native frames and scalar values live across idle polls.

The common poll path must not materialize canonical machine state.

Each exact checkpoint keeps its live values on the current SSA path.

An idle poll reads one atomic request and rearms the next deadline.

A requested poll uses the existing exact materialization exit.

One worker reservation contains at most 64 poll intervals.

The reservation bounds shared fuel claims. It does not define fairness.

The scheduler poll interval defines fairness and request response time.

The native poll tests passed at exact instruction boundaries.

The focused JIT suite passed 157 tests.

The parallel scheduler suite passed 35 tests.

The direct and scheduled corpus differential passed for 163 programs.

The corpus gate completed in 45.48 seconds.

The warm debug workspace suite completed in 84.47 seconds.

Test-only poll sweeps use every interval from 1 through 64.

Production scheduling keeps the 16,384-instruction poll interval.

One `PollSchedule` implementation defines interpreter and native phase arithmetic.

A semantic boundary takes priority over a poll request at the same instruction.

An idle poll does not clear heap proofs because it does not mutate the heap.

| Warm row | Stage F54 | Generated rearm | Change |
| --- | ---: | ---: | ---: |
| Scheduled integer loop | 0.756 ms | 0.431 ms | 43.0 percent faster |
| Scheduled deep recursion | 17.601 ms | 12.124 ms | 31.1 percent faster |

The integer loop gained 76.72 times over interpreted execution.

Scheduled deep recursion now matches its direct native time.

| Representative row | Interpreter | Auto | Native | Auto gain |
| --- | ---: | ---: | ---: | ---: |
| JSON parse | 44.872 ms | 18.590 ms | 18.035 ms | 2.414 times |
| JSON encode | 20.491 ms | 9.095 ms | 8.844 ms | 2.253 times |
| HTTP parse | 41.523 ms | 19.555 ms | 19.372 ms | 2.123 times |
| HTTP encode | 21.796 ms | 6.133 ms | 6.103 ms | 3.554 times |

All representative rows retired every instruction through native code.

The generated rearm first used one whole-function SSA merge and backedge.

That graph exceeded the corpus memory limit and increased compile time.

Local rearm blocks preserve the same values without the large merge.

An active worker claim makes unavailable fuel pending, not exhausted.

The coordinator waits for pending claims before it faults another task.

A scarce claim takes at most half of the currently available fuel.

This rule lets another worker claim fuel under a small world budget.

Normal budgets still use the complete 64-poll reservation.

## 24. Rejected designs

A generic callback dispatcher cannot implement common heap instructions.

A runtime stub cannot replace an opcode with an inline treatment.

A temporary interpreter site cannot count as native coverage.

A complete engine cannot retain a temporary opcode treatment.

A parallel side record cannot mirror mutable object layout.

Per-operation metadata decoding cannot remain on a native fast path.

Semantic hash walks cannot remain in runtime class checks.

Interpreter profiling cannot take a mutex per call.

Specialization cannot occur before a cache hit.

Every eligible function cannot compile on its first entry.

A statically tagged scalar cannot keep a redundant dynamic tag register.

A call cannot materialize canonical state.

A successful call cannot spill a complete caller or child frame.

A scheduler continuation cannot keep stale duplicate scalar state.

A runtime payload cache cannot add one branch to every field or element access.

Generated code cannot serialize process-local heap addresses.

These designs remain rejected even when local differential tests pass.
