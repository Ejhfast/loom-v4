# Multi-threaded Scheduler

Status: proposed design. No implementation exists yet.

This sidecar refines language specification sections 17, 18, 22.12, and 23.9.

## 1. Decision

Loom adds a parallel scheduler beside the current deterministic scheduler.

The parallel scheduler uses one coordinator and a bounded worker pool.

The coordinator owns the complete `World` and every cross-machine semantic commit.

A worker receives one exclusive execution lease for one machine.

A worker runs guest instructions until a boundary or quantum limit.

A worker then returns the complete machine and one execution report.

The coordinator applies sends, host requests, control operations, and terminal publication.

The deterministic scheduler keeps the current FIFO policy and fixed quantum.

It executes machine slices inline and creates no worker pool.

Scheduler mode never changes Loom types, effects, bytecode, or snapshot bytes.

This work needs no source, interface, bytecode, operation, or snapshot format change.

## 2. Goals

This design has these goals:

- Run independent scheduler-owned tasks on different processor cores.
- Preserve one logical execution thread for each VM.
- Preserve private heaps and copied boundary values.
- Keep the current deterministic mode exact.
- Keep guest effects explicit under both modes.
- Keep snapshots canonical and independent of scheduler mode.
- Keep pause, resume, and code replacement safe.
- Keep root-only programs on a low-cost inline path.
- Give a future JIT the same safepoint contract.
- Keep `lm-vm` independent from threads and operating-system services.

## 3. Non-goals

This work does not add shared-memory guest objects.

This work does not make one VM execute concurrently.

This work does not make held VMs independent scheduler tasks.

This work does not add distributed scheduling.

This work does not promise repeatable parallel interleavings.

This work does not require work stealing.

This work does not add a JIT.

## 4. Current architecture

`lm-proc` owns task order, wake indexes, wait indexes, barriers, and deadlock detection.

`World::drive_slice` still needs exclusive access to the complete `World`.

The current thread-backed path moves the complete world to one operating-system thread.

It gives the guest a bounded Rust stack. It does not run procs in parallel.

### 4.1 Existing execution seam

`Machine::exec_for_quantum` already has a narrow input set:

- one mutable `Machine`;
- one immutable verified module;
- one immutable dispatch table;
- one mutable closed-type table;
- one immutable image slot table;
- one instruction limit.

It returns an `ExecOutcome` when guest execution reaches world state.

These outcomes already cover effects, waits, slots, table edits, rendering, and code values.

This boundary becomes the worker execution boundary.

### 4.2 State that blocks direct movement

Three current structures prevent `Machine` from moving safely between threads.

`HeapBudget` and `ResourceBudget` use `Rc<Cell<_>>` for aggregate counters.

The closed-type table is mutable and shared by all machines.

Cross-machine graph transfer needs mutable access to both machine heaps.

The implementation must remove these dependencies before parallel execution.

### 4.3 Activation stacks

A scheduler task is not always one machine.

A root or proc task can drive a stack of held VMs.

The coordinator retains each activation stack.

Only its current machine enters one execution lease.

A nested VM transition returns to the coordinator before another machine runs.

This rule preserves sequential held-VM semantics.

### 4.4 Current host wait defect

`CliHost` has separate readiness paths for I/O, network, process, compiler, signal, and sleep work.

`CliHost::wait` polls those paths with a ten-millisecond quantum.

It can block on the I/O receiver after a process timeout.

A later process completion cannot wake that I/O receiver.

Printing before one slow child wait can therefore deadlock the program.

A live raw terminal also keeps the signal guardian active.

That guardian prevents the only direct I/O blocking path.

Interactive terminal reads therefore pay repeated ten-millisecond waits.

This structure creates both a correctness defect and a measurement bias.

## 5. Design space

| Design | Benefit | Main cost | Decision |
|---|---|---|---|
| Parallel worlds only | No `World` change | Procs in one program stay sequential | Reject |
| One thread per proc | Simple ownership | Unbounded threads and stacks | Reject |
| One mutex around `World` | Small refactor | Guest computation stays serialized | Reject |
| One mutex per machine | Direct shared access | Lock order spreads through VM code | Reject first |
| Fully concurrent world | Highest possible throughput | Barriers and replacement become complex | Defer |
| Coordinator and leases | One commit point | World operations cross one boundary | Adopt |

### 5.1 Why one coordinator

The coordinator preserves the current ownership model.

It also gives every cross-machine action one linear commit point.

This point defines mailbox acceptance, close races, policy edits, and terminal publication.

The coordinator does not execute long guest slices in parallel mode.

### 5.2 Why moved leases

An `Arc<Mutex<Machine>>` would place locks on every machine access.

Two-machine operations would also need a global lock order.

Moved leases keep the interpreter hot path free from machine locks.

The resident machine table uses an explicit `Resident` or `Leased` state.

### 5.3 Why no work stealing first

A central dispatcher is small and measurable.

The fixed quantum already bounds every job.

Work stealing adds local queues, stealing rules, affinity rules, and barrier coordination.

The implementation adds work stealing only after coordinator measurements justify it.

## 6. Terms

A **task** is one root or scheduler-owned proc execution path.

A **machine** is one `VmState` with its heap and control state.

An **activation stack** records held-VM execution for one task.

An **execution lease** gives one worker exclusive access to one machine.

An **execution report** returns that machine and its slice result.

A **commit** is one coordinator state transition.

A **safepoint** is an interpreter boundary with complete machine state.

## 7. Scheduler modes

The public runtime has two scheduler modes.

### 7.1 Deterministic mode

`Deterministic` keeps the current ready queue and quantum.

It runs one machine slice at a time on the coordinator thread.

It preserves current task ordering, trace ordering, and counter behavior.

It creates no worker channels and pays no parallel scheduling cost.

Single-threaded mode means one guest execution thread.

Bounded host service threads can still perform external work.

Tests use this mode unless a test names parallel behavior.

### 7.2 Parallel mode

`Parallel { workers }` creates a fixed worker pool.

At most `workers` machine slices execute at one time.

The root task and scheduler-owned procs can execute at the same time.

Ten runnable procs create ten runnable tasks. The worker count limits simultaneous slices.

Each task holds at most one active execution lease.

Each machine belongs to at most one active execution lease.

A held VM remains part of its holder's task.

A paused proc remains holder-owned until `resume` transfers ownership.

The ready queue provides weak fairness among resident ready tasks.

A ready task eventually receives a worker while the scheduler continues.

Parallel mode does not promise equal processor time.

### 7.3 Mode selection

The Rust API requires an explicit `SchedulerConfig`.

The CLI provides `--scheduler deterministic` and `--scheduler parallel`.

The CLI provides `--threads N` for parallel mode.

`N` must be greater than zero and below a fixed safety limit.

Parallel mode uses available host parallelism when the user omits `N`.

The release CLI defaults to parallel mode after all performance gates pass.

The implementation keeps the deterministic default before Stage 6.

The test harness keeps deterministic mode as its default.

Guest code cannot observe the mode or worker count.

### 7.4 Pool ownership

One coordinator controls one `World`.

One worker pool can serve several coordinators in one host process.

Each job carries its own report route and world identity.

The CLI creates one pool for one command.

An embedding host can inject one shared pool.

This rule prevents thread growth when one host runs many worlds.

Each worker uses the current bounded Rust stack size.

## 8. Ownership model

The coordinator owns these values:

- `World`;
- the host adapter;
- task records;
- activation stacks;
- ready and wait indexes;
- VM images and current slot tables;
- aggregate budgets;
- snapshot barriers;
- deferred world transactions.

A worker owns these values during one job:

- one leased `Machine`;
- one lease token;
- one instruction budget;
- one heap growth allowance;
- immutable verified code;
- immutable dispatch data;
- one immutable image slot view;
- one closed-type access view.

The worker returns all mutable values in one report.

No worker stores a reference into `World`.

No worker invokes `Host`.

No guest heap reference enters a scheduler index.

### 8.1 Machine slot state

Each machine slot has this conceptual shape:

```text
MachineSlot {
  generation,
  owner,
  image,
  task,
  stop_state,
  payload: Resident(Machine) | Leased {
    lease_token,
    worker
  }
}
```

The slot retains the metadata that control operations need during a lease.

The returned machine must agree with that retained metadata.

The coordinator changes `Resident` to `Leased` before dispatch.

The report token must match the current slot token.

A stale or duplicate report is a runtime scheduler error.

The coordinator never reuses one token within a process.

The world identity prevents a report from entering another coordinator.

### 8.2 Execution interface

`lm-vm` exposes an opaque executor boundary.

Conceptually, the boundary has this shape:

```text
extract(task, limits) -> ExecutionLease
execute(ExecutionLease) -> ExecutionReport
commit(ExecutionReport) -> ScheduleEvents
```

Only `lm-vm` can construct or inspect the lease payload.

`lm-proc` can route leases without seeing guest values.

The deterministic scheduler calls the same executor inline.

## 9. Slice execution

A worker can execute only guest instructions and local heap work.

The worker stops for every operation that needs world state.

The stop list includes these actions:

- an effect perform;
- a host wait request;
- a proc send or close;
- a proc spawn or terminal publication;
- a nested VM transition;
- a policy-table edit;
- an image-slot edit;
- a cross-machine value transfer;
- a pause or snapshot request;
- a heap lease refill;
- an instruction quantum expiry;
- a guest terminal result or fault.

The report contains the retired instruction count and one optional action.

The coordinator commits the action before it dispatches that task again.

## 10. Cross-machine transactions

The current graph transfer mutates the source heap and destination heap.

The first parallel implementation retains this transfer algorithm.

A cross-machine action returns its source machine before the transfer.

If the destination is leased, the coordinator stores a deferred transaction.

The destination returns within its bounded quantum.

The coordinator then transfers the graph and updates the mailbox or reply state.

The source task stays blocked until the transaction commits.

This design avoids a second message graph representation.

### 10.1 Detached transfer packets

A later optimization can export one immutable `TransferPacket` from the source heap.

The destination can import that packet after it becomes resident.

This path adds an intermediate representation and another budget rule.

The implementation adds it only when destination waits dominate send cost.

### 10.2 Transaction order

The coordinator assigns one increasing commit ordinal to each accepted action.

The ordinal exists for tracing and diagnostics.

It never enters guest state or snapshot bytes.

## 11. Message semantics

Each machine executes its instructions in program order.

Messages from one sender to one receiver preserve send order.

Messages from different senders have no relative order guarantee.

The coordinator commit order defines their accepted mailbox order.

A send and a close can race in parallel mode.

The first committed action decides whether the send is accepted.

Accepted messages remain FIFO.

A refused message never enters the destination heap.

Self-send still copies within one heap.

Deterministic mode keeps its current exact acceptance order.

## 12. Closed types and generic calls

The current closed-type table combines canonical nodes with hot mutable caches.

One lock around every generic instruction would serialize workers.

The new design separates canonical storage from machine-local caches.

### 12.1 Canonical store

One world owns one append-only canonical type store.

Closed type and environment identifiers remain world-local implementation details.

Existing records never move or change.

A synchronized miss path interns one new record.

The first implementation can use one short interner lock.

The type access view has exclusive and shared forms.

Deterministic mode uses exclusive access and takes no interner lock.

### 12.2 Machine-local cache

Each machine caches frequent close and environment-derivation queries.

A cache hit needs no synchronization.

A cache miss enters the synchronized interner path.

It does not expose a guest-visible operation or coordinator transaction.

The implementation measures misses before it adds shards or lock-free storage.

Parallel insertion order cannot affect guest behavior.

Snapshot encoding reconstructs canonical type order from structural content.

Deterministic mode preserves the current insertion order for diagnostics and tests.

### 12.3 Measurement gate

Instrumentation records these values:

- closed-type cache hits per slice;
- closed-type cache misses per slice;
- interner lock wait time;
- new type nodes per million instructions;
- new environments per million instructions.

A segmented interner remains deferred while total miss cost stays below two percent.

## 13. Budgets

Shared `Rc<Cell<_>>` counters cannot cross worker threads.

Replacing them with atomic counters would charge every allocation.

The coordinator instead grants bounded budget leases.

### 13.1 Fuel

The coordinator reserves the exact slice fuel before dispatch.

The worker retires no instruction beyond that reservation.

The report returns unused fuel.

The coordinator then updates the world fuel counter.

The quantum bounds instruction count. It does not bound one instruction's wall time.

### 13.2 Heap

The coordinator grants one bounded heap growth allowance.

The machine still enforces its local byte and object limits.

The report states live growth, released space, and unused allowance.

The coordinator applies those values to the aggregate budget.

A worker stops before allocation when its allowance cannot cover the request.

The coordinator can grant another allowance or report the existing budget fault.

A lease shortage is not a guest `HeapLimit` fault.

An allocation-capable instruction preflights its required growth before visible mutation.

Insufficient allowance returns an internal refill report.

The coordinator grants more allowance or waits for active reservations to return.

It reports `HeapLimit` only when the real machine or world limit cannot satisfy the request.

The instruction retries from its unchanged start state after a refill.

An intrinsic that cannot provide this guarantee cannot execute inside a worker slice.

### 13.3 Resources

Resource creation happens after an effect reaches the coordinator.

Workers never mutate the resource registry.

The coordinator owns the aggregate resource count.

## 14. Host completions

The host adapter remains coordinator-owned and can remain non-`Send`.

Potentially blocking work stays in the existing bounded services.

Stage 0.5 gives `CliHost` one shared readiness queue.

This stage lands before any executor change.

### 14.1 Readiness events

The queue carries one private `HostReady` event type.

It has these conceptual variants:

```text
Completion(HostCompletion)
Network(NetworkCompletion)
SignalReady
```

I/O, process, compiler, and network workers clone one shared sender.

Each service keeps its request queue and cancellation state.

It no longer keeps a private completion receiver.

The queue remains host-private and changes no `Host` trait method.

Existing service request limits bound the number of normal queued events.

### 14.2 Network completion ownership

`NetworkCompletion` carries one retained-byte release guard.

Consuming or dropping the event releases that guard exactly once.

This rule preserves wait cancellation and retained-byte limits.

It also removes the current receiver-side release dependency.

### 14.3 Signal notification

The signal handler continues to write only one byte to its self-pipe.

A signal forwarder owns the read end and blocks on it.

The forwarder records the signal kind in one bounded inbox.

It sends one coalesced `SignalReady` marker to the shared queue.

`SignalService` consumes that inbox on the coordinator thread.

It retains the current delivery, cancellation, escalation, and guardian rules.

Dropping the signal service closes the pipe and joins the forwarder.

The signal handler never touches a Rust channel or lock.

### 14.4 Sleeps

Sleeps stay in the coordinator's timer map.

`wait()` computes the earliest deadline once per park.

It uses `recv_timeout` until that deadline.

It uses `recv` when no sleep exists.

After a timeout, it checks expired sleeps without a polling quantum.

An expired sleep cannot starve behind a continuing completion stream.

`poll()` also checks one expired sleep before the next queued event.

`ProcessService` keeps its internal two-millisecond progress tick in this stage.

That tick is separate from cross-service readiness.

### 14.5 Ordering

One service sender preserves its own completion order.

Different senders use the queue's accepted order.

Parallel service completion order remains unspecified.

Signals no longer have fixed priority over earlier accepted completions.

The scheduler drains all ready host events before it runs another guest slice.

The terminal guardian therefore handles a queued signal before more guest execution.

Deterministic `RecordingHost` behavior does not change.

Real host completion order was never part of deterministic replay.

### 14.6 Future scheduler wake path

Worker reports and host completions must wake one coordinator event source.

The host adds a notifier registration or an equivalent wake sink.

The coordinator does not poll with a fixed sleep interval.

The coordinator parks only after it checks all immediate work.

It checks ready tasks, reports, transactions, host completions, and barrier progress.

Deadlock requires no ready task, active lease, host wait, or committable transaction.

## 15. Determinism contract

Loom separates semantic determinism from scheduler determinism.

### 15.1 Pure values

One isolated machine returns the same pure result under both scheduler modes.

Independent machines also keep their results while no aggregate limit becomes exhausted.

Parallel tasks can race for aggregate fuel, heap, resource, and closed-type limits.

The failing task can differ when one shared limit becomes exhausted.

### 15.2 Deterministic replay

Deterministic mode preserves one repeatable scheduler trace.

Replay also needs equal artifacts, snapshot input, host transcript, limits, and runtime version.

Real clock, network, process, signal, and terminal input need a recorded host transcript.

Random draws remain repeatable under deterministic scheduling and an equal seed.

### 15.3 Parallel execution

Parallel mode guarantees data-race freedom for guest heaps.

It does not promise one repeatable global interleaving.

Random draw assignment across racing tasks can differ.

Host completion order can differ.

Task-ready order can differ.

### 15.4 Canonical identity

Artifact hashes and value digests do not depend on scheduler mode.

Snapshots encode logical machine state, not worker state.

Equal stopped worlds produce equal canonical snapshot bytes.

Worker identifiers, lease tokens, queue positions, and commit ordinals never enter snapshot bytes.

## 16. Wait and selection semantics

`select` remains left-biased among sources ready at its reevaluation point.

Parallel execution can change which source becomes ready first.

A wake notification does not reserve one result.

The coordinator commits one winner and withdraws every loser.

A worker never mutates a wait index.

Mixed mailbox, drive, and host selection keeps one coordinator commit rule.

## 17. Policies and effects

A perform exits the worker before policy resolution.

The coordinator selects one immutable policy action for that perform.

A later policy edit affects later performs.

It does not change an action already selected.

Effects from different tasks can commit in either order under parallel mode.

The trace records coordinator commit order.

Worker identifiers do not appear in the semantic trace.

## 18. Snapshot barriers

A parallel barrier first stops the target task set.

The coordinator marks each target task as stopping.

It issues no new lease for those tasks.

Each active lease returns after its current instruction or remaining quantum.

A native intrinsic or GC can extend this wall-clock delay.

The barrier then closes reachability over the stopped machine states.

It stops each newly reached task and repeats the closure.

The coordinator freezes mailbox acceptance at one commit cut.

Actions committed before the cut enter the captured world.

Later actions wait until the original world resumes.

Capture, admission, encoding, and resume then keep their current rules.

The coordinator builds one detached capture image before it resumes the target set.

Canonical encoding can run outside the coordinator after that image becomes independent.

The snapshot limits account for the detached image until encoding completes.

Disjoint barriers can overlap their encoding work.

Overlapping barriers serialize.

Snapshot mode does not enter the image.

A restored snapshot can use either scheduler mode.

## 19. Pause, resume, and replacement

### 19.1 Pause and resume

`pause` marks the proc task as stopping.

An active lease returns after its current instruction or remaining quantum.

The coordinator returns the held VM only after the machine becomes resident.

`resume` transfers ownership back to the scheduler.

No worker can execute a paused proc.

### 19.2 Slot replacement

Replacement creates an image-level safepoint.

The coordinator stops new leases for every task using that image.

It waits for all current image leases to return.

It validates and applies the complete replacement batch atomically.

It then resumes the affected tasks.

Existing frames keep their pinned function versions.

New calls read the new slot targets.

Workers use immutable slot-table views during each lease.

### 19.3 Code publication

Each execution lease pins one immutable module, dispatch, and slot-table version.

Installation builds and publishes a new immutable execution view.

Later leases use that view.

An active lease can finish with its prior view.

Any edit to an existing slot still uses the image safepoint.

A compiled-code cache keys each entry by verified function identity and engine version.

## 20. Termination and failure

### 20.1 Root termination

Root termination stops new task dispatch.

The coordinator requests every active lease back.

It does not commit later child effects after the root terminal commit.

It waits for every machine value before it drops `World`.

### 20.2 Worker failure

A worker panic is a runtime scheduler failure.

It is not a guest `Fault`.

The worker boundary catches unwinding where the Rust profile permits it.

The world becomes poisoned after a missing or failed report.

The runtime never resumes a partly executed machine.

Pool creation failure returns a host runtime error before guest execution.

### 20.3 Shutdown

The pool receives an explicit shutdown command.

The coordinator joins all workers before returning the world result.

Worker shutdown never depends on guest cleanup.

## 21. JIT contract

The executor boundary is also the future JIT boundary.

The interpreter and JIT implement the same logical operation:

```text
run_slice(ExecutionLease, SliceBudget) -> ExecutionReport
```

Compiled code is immutable and shared by verified function identity.

Compiled code stores no worker identifier or mutable `World` pointer.

No raw guest pointer survives a safepoint.

The JIT materializes frames, values, roots, and program position before each safepoint.

Safepoints include effects, allocation, yield, pause, barrier, replacement, fault, and quantum expiry.

Fuel remains defined in bytecode instructions.

Deterministic mode must charge the same logical fuel under both engines.

A JIT can charge a complete basic block before entry.

It interprets or deoptimizes when the remaining fuel cannot cover that block.

Faster time-based interruption can supplement fuel in parallel mode.

It cannot replace deterministic fuel.

## 22. Crate boundaries

`lm-vm` owns machine extraction, execution, report validation, and commit logic.

`lm-proc` owns scheduler modes, workers, queues, wake indexes, and barriers.

It also owns the reusable worker pool and report routes.

`lm-host` owns external service threads and completion notification.

`lm-cli` owns command-line scheduler selection.

`lm-bytecode` owns the canonical closed-type store representation.

No lower crate depends on `lm-proc`.

`lm-vm` does not depend on an operating-system thread pool.

## 23. Instrumentation before refactoring

Stages 0 and 0.5 record the current workload shape.

The measurements include these counters:

- retired instructions per slice;
- boundary exits per slice;
- sends per million instructions;
- destination-active sends;
- cross-machine graph bytes;
- closed-type derivations and misses;
- heap growth per slice;
- deferred transaction wait time;
- native intrinsic duration;
- per-machine GC duration;
- host completions per slice;
- host parks and wakeups;
- host timeout wakeups;
- shared readiness queue depth;
- coordinator work per slice.

The benchmark set includes these programs:

- independent CPU procs;
- fan-out and fan-in;
- mailbox ping-pong;
- many independent sender pairs;
- one many-sender mailbox;
- a generic collection loop;
- polymorphic recursion;
- mixed host waits;
- output followed by a slow child wait;
- raw terminal input with a sleep source;
- snapshot under load;
- code replacement under load.

These measurements decide two optional optimizations.

They decide detached transfer packets and a segmented type interner.

## 24. Implementation stages

### Stage 0: Measure the current system

Add the counters from section 23.

Record the existing polling and deadlock reproduction.

Do not accept scheduler performance baselines in this stage.

Do not change scheduling behavior in this stage.

### Stage 0.5: Unify host readiness

Give all asynchronous `CliHost` services one shared readiness sender.

Remove their private completion receivers and timeout wait methods.

Add the signal forwarder and its bounded inbox.

Replace the ten-millisecond loop with deadline-based queue waiting.

Add the mixed I/O and child-process deadlock regression.

Add mixed-source ordering and cancellation tests.

Record deterministic runtime, allocation, host latency, and suite baselines.

Update `benchmarks/latest-baseline.md` with the accepted results.

### Stage 1: Extract the executor boundary

Split local machine execution from world commits.

Keep deterministic execution inline.

Return every world operation through an execution report.

Match current deterministic traces exactly.

### Stage 2: Make execution leases thread-safe

Replace shared aggregate counters with budget leases.

Split the closed-type store from machine-local caches.

Publish immutable code, dispatch, and slot views.

Add compile-time `Send` checks for every lease field.

Do not add an `unsafe impl Send`.

### Stage 3: Add the bounded worker pool

Add opt-in parallel mode with one central dispatcher.

Start the pool only when parallel work exists.

Add one combined wake path for reports and host completions.

Prove scaling with CPU-only proc programs.

### Stage 4: Complete world transactions

Route send, spawn, receive, close, and terminal actions through the coordinator.

Route nested VM control and policy edits through the same path.

Add deferred two-machine transactions.

State every race outcome in tests.

### Stage 5: Add parallel barriers

Implement snapshot stop sets and reachability closure.

Implement pause and resume over active leases.

Implement image safepoints for replacement batches.

Keep snapshot bytes canonical.

### Stage 6: Expose mode selection

Add the Rust configuration API and CLI options.

Keep tests deterministic by default.

Change the CLI default only after the performance gates pass.

### Stage 7: Stress and optimize

Run deterministic event scripts for every race class.

Measure coordinator saturation, message delay, and type interner delay.

Add work stealing only when the central dispatcher misses its gate.

Add transfer packets only when resident waits miss their gate.

### Stage 8: Freeze the JIT boundary

Document the final executor interface.

Test interpreter safepoints against the future JIT contract.

Do not implement Cranelift in this initiative.

## 25. Correctness gates

The implementation must pass these gates:

- deterministic trace fixtures stay byte-for-byte equal;
- each machine has at most one active lease;
- stale and duplicate reports reject safely;
- same-sender mailbox order stays exact;
- multi-sender tests accept only documented orders;
- send and close races follow commit order;
- mixed `select` sources commit one winner;
- parent termination and child effects follow commit order;
- pause completes during long pure computation;
- snapshot closure can reach an active task;
- replacement stops every task using one image;
- root termination drains all active leases;
- worker failure never loses a machine silently;
- host completion and worker report races lose no wake;
- output before a slow child wait cannot deadlock;
- compiler, process, and I/O completions wake the same host park;
- a raw-terminal guardian causes no periodic polling;
- signal forwarding preserves cancellation and escalation;
- network retained-byte guards release exactly once;
- deadlock detection waits for active reports;
- old snapshots restore under both modes;
- new snapshots restore under both modes.

Property tests cover the resident and leased state machine.

Stress tests use scripted events instead of timing sleeps.

A host-hub test enumerates every ordered pair of readiness sources.

The real child regression uses a bounded harness timeout.

The synchronization core can use a model checker if normal tests cannot cover an interleaving.

## 26. Performance gates

Deterministic root-only execution must stay within normal benchmark noise.

Deterministic proc execution can regress by at most five percent.

The deterministic workspace suite must stay near its recorded baseline.

The accepted baseline starts after Stage 0.5.

An idle host park performs no periodic ten-millisecond wakeup.

A completion without a timer causes one blocking receive wakeup.

Sleep completion uses its exact remaining deadline.

Parallel root-only execution must use the inline path when no second task can run.

Two independent CPU tasks target at least 1.7 times one-worker throughput.

Four independent CPU tasks target at least 3.0 times one-worker throughput.

These scaling gates require dedicated hardware and several samples.

Coordinator work stays below fifteen percent of one core in the CPU benchmark.

Closed-type miss handling stays below two percent of total runtime.

Message benchmarks report throughput and tail delay.

They cover ping-pong, streams, independent pairs, and many senders.

Snapshot capture reports stop time separately from encoding time.

Every baseline records processor count, worker count, build profile, and scheduler mode.

## 27. Known limitations

Parallel mode cannot reproduce one global interleaving without a replay log.

The first coordinator can limit message-heavy programs before workers reach full use.

Deferred two-machine transactions can wait for one destination quantum.

One global type interner lock can limit generic workloads with many new types.

A stop request waits for at most the remaining quantum instructions.

The quantum does not bound wall-clock time.

A long native intrinsic must provide an internal safepoint or remain below the latency gate.

Per-machine GC can also extend one lease beyond its instruction quantum.

The first implementation does not cancel or move a running GC.

Blocking custom host code can still block the coordinator if it violates `Host::start`.

Parallel floating-point results can differ when a program changes reduction order through messages.

External effects cannot become deterministic without recorded inputs and outputs.

## 28. Design basis

Erlang uses private process heaps and copies messages between processes.

Erlang also preserves signal order from one sender to one receiver.

Its scheduler uses bounded reductions across several scheduler threads.

Go separates runnable work from operating-system threads through logical processors.

Tokio documents the contention cost of one global queue for very short tasks.

Wasmtime separates deterministic fuel from faster time-based interruption.

These systems support Loom's private heaps, bounded slices, and measured central scheduling.

References:

- [Erlang process memory](https://www.erlang.org/docs/17/efficiency_guide/processes.html)
- [Erlang scheduling](https://www.erlang.org/docs/27/apps/erts/erlang.html)
- [Erlang signal ordering](https://www.erlang.org/docs/20/apps/erts/communication)
- [Go runtime scheduler](https://go.dev/src/runtime/HACKING)
- [Tokio scheduler design](https://tokio.rs/blog/2019-10-scheduler)
- [Wasmtime interruption](https://docs.wasmtime.dev/examples-interrupting-wasm.html)

## 29. Final acceptance

The initiative completes only after both scheduler modes pass the full workspace suite.

The release baseline must contain results for both modes.

The deterministic baseline remains the compatibility gate.

The parallel baseline remains the scaling gate.
