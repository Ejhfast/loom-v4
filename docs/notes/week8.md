# Week 8 Status

This note records the week 8 work. It covers:

- what landed;
- the inheritance decisions and the rejected alternatives;
- the proc launch decision and its type rule;
- the scheduler and barrier decisions;
- the simplifications inside the slice;
- the changed tests, the new tests, the measurements, the gates, the
  open questions, and the deferred work.

Bytecode format version 12 carries the parent type arguments of a
generic parent and the `Handle` type. The interface format is version
4. The compiler ABI version is 6 and the verifier version is 5,
because the class identity encoding and the class rules both changed.
The operation manifest ABI version is 2: it gained the eight proc
operations of specification 23.6, and the manifest rule makes a
membership change an ABI change. The core image pin moved to
`84abcf62fbaa8607941f26acd272495fd1a9f791f40536d8dd44c62da0b8a769`
and `core/pinned-core-defs.txt` holds thirty-six definition hashes:
the twenty of week 7 plus the four proc families and the proc class.

## Landed

### A module class may inherit a core class

Nothing before week 8 inherited a class of the pinned core image, and
the proc classes need that path. It landed first, with its own tests.

The core sources now register before the module sources, so a core
parent always keeps a lower class index than its subclass. The
verifier, the dispatch builder, and the linker all read that order.
A parent name resolves like every other type name: a module type
first, then a core type the prelude names.

### Generic-parent inheritance

`class Worker < Proc[Work]` is the named checker extension. The parent
type arguments flow into every inherited field type, method signature,
and super call. Three parent shapes exist, and one walk covers all
three:

| Parent | Arguments | Rule |
|---|---|---|
| An enum case | implicit identity | the family shares the arity |
| A declared generic parent | closed types | a generic class declares no parent |
| Every other parent | none | the walk carries nothing |

Because a generic class still declares no parent, a recorded argument
list never holds a type variable, and the ancestor walk never
substitutes.

The class entry carries the arguments, so the verifier reads them from
the class table. The override rules, the field-layout rule, and the
virtual dispatch rule all apply the arguments before they compare, and
no call site can claim another instantiation.

### The proc manifest and the core proc image

The manifest declares `Proc.Run`, `Proc.Spawn`, `Proc.Send`,
`Proc.Close`, `Proc.Recv`, `Proc.Done`, `Proc.Pause`, and
`Proc.Resume`, each with the generic schema of specification 23.6.

Every proc operation is machine state. A blocked proc call waits on
another machine of the same machine world, never on live host state,
and the record that carries the block holds machine identifiers and
ordinals only. A snapshot copies the machines and rebuilds the
scheduler from them.

`core/proc.lm` adds `Recv[M]`, `SendResult`, `ProcResult[R]`,
`ProcError`, and the class `Proc[M]`. `Proc[M].receive` is an ordinary
source body that performs `Proc.Recv`. The prelude names the five
definitions, and `Handle[M, R]` and `Never` are type names.

### Mailboxes

A mailbox is part of the receiving machine's own state: a bound, a
FIFO queue of accepted messages in that machine's heap, a close flag,
a barrier freeze flag, and two counters. The queue is a collection
root of the machine, so an accepted message stays alive until
`receive` delivers it.

The rules the model tests state:

- accepted messages reach `receive` in host acceptance order;
- `close` prevents later acceptance, preserves the queue, and
  `Closed` arrives after the drain;
- a successful `close` returns `Sent`, and a repeat returns `Closed`;
- a send to a terminal proc or a stale reference returns
  `Fault(DeadProc)`;
- the bound is checked before the copy, so a refused message never
  enters the target heap;
- a mutable message faulted the sender at its own boundary. The
  decision of 2026-08-16 at the end of this note replaced that rule:
  the message copies.

### The proc launch

`Class.spawn(args...)` is checker sugar. It checks the class against
`Proc[M]`, checks `on_spawn`, checks the arguments against the class
`init`, and infers `Handle[M, R]`. It lowers to what a user would
write: the construction function of the proc class, the `on_spawn`
function, the typed argument tuple, and one `Proc.Spawn` perform.

The proc instance is constructed inside its own machine
(specification 18.1). The launch therefore runs two frames: the
constructor, then `on_spawn` over the constructed value.

The spawner charges `Proc.Spawn` only. The constructor and the proc
body run inside the child, so their rows resolve through the child
table and the birth grant. The birth grant carries the `Proc` group
and nothing else; additional grants use the explicit machine path of
specification 18.2.

### Execution ownership

`Proc.Run` and `Proc.Spawn` transfer execution ownership to the
scheduler. The original `Vm` handle becomes dormant: execution and
inspection through it fault with `InvalidVmState` until `pause()`
returns ownership. Table edits stay legal, so revocation still works,
and one test revokes the birth grant through a paused machine.

`pause` returns `Result[Vm[R], ProcError]` and `resume` returns
`Result[(), ProcError]`. The four `ProcError` arms are `Dead`,
`NotPaused`, `AlreadyPaused`, and `InUse`.

### The scheduler

The new crate `lm-proc` sits above `lm-vm`, where section 1 of the
build order puts it. It owns the scheduling policy and nothing else:
the run order, the progress rule, the deadlock rule, and the barrier
algorithm. It depends on `lm-abi` and `lm-vm` only, so a scheduler
record cannot name a guest object at all. One test reads the crate
manifest and states that.

The loop drives the root stack first. When a machine blocks on
another machine of this world, its whole activation stack stops and
the world stores it, keyed by the machine the stack started from. The
scheduler completes every block that can complete, then drives one
runnable proc, then resumes the stored stack where it stopped. A
holder-driven nested machine may therefore block on a proc, and one
test proves it.

The deterministic mode drives one machine at a time in ascending
identifier order. It reads no clock and holds no randomness, so two
runs of one program produce the same interleaving, the same trace, and
the same counters.

`lm run` builds, drives, and drops the whole world inside one worker
thread with an 8 MiB stack. Only the rendered outcome comes back, so
no guest reference crosses the thread boundary. That is the
thread-backed baseline of specification 22.12 at the level this
architecture supports today; the simplification below states the gap.

### The barrier

`lm_proc::Barrier` runs the consistent cut of specification 17.3 in
order:

1. it stops the root and every reachable machine, which are already at
   an instruction boundary because the scheduler drives one machine at
   a time;
2. it closes the set over the machine references it finds in the
   stopped state, through the canonical snapshot traversal over the
   collection roots, which cover the frames, the locals, the operands,
   the pending arguments, the terminal result, and the accepted
   mailbox queue. Five native shapes name a machine: a machine
   handle, a proc handle, a policy-table handle, a request token, and
   a typed call token. The walk reports all five;
3. it freezes mailbox acceptance for the whole set at one cut marker;
4. it records the machine states;
5. it preflights the host attachments through the resource registry;
6. week 8 encodes nothing;
7. it resumes the original world after success and after failure.

A barrier that finds a machine another barrier holds reports the
overlap and takes nothing, so barriers over overlapping worlds
serialize and barriers over disjoint worlds proceed. A frozen mailbox
accepts no message: the sender blocks instead, so the accepted queue
at the cut is exactly what an encoder would copy.

### Tracing, metrics, and dumps

`World::trace_procs` turns on an ordered proc trace: spawn, send,
receive, close, block, unblock, pause, resume, and terminal. Every
record names a machine by identifier and generation.
`World::mailbox_metrics` reports the bound, the queue length, the
accepted count, the delivered count, the close flag, and the freeze
flag.

Every new format has a readable dump. `World::dump_trace` prints one
line per event, `World::dump_mailboxes` prints one line per proc, and
the class listing of `lm disasm` shows the parent instantiation the
class table records. All three repeat exactly.

### Parent lifetime

A child table passes through the live parent table. Parent death
removes the pass-through, and a later request fails closed
(specification 18.6). One test runs the root to a terminal result and
then drives the surviving proc: its next receive faults with
`PolicyDenied`.

### Runnable outputs

```text
$ lm run --show-result examples/07-concurrency/worker.lm --allow Proc
Done(Ok(42))

$ lm run --show-result examples/07-concurrency/mailbox-handle.lm --allow Proc
Done((Ok(1), Ok(12)))

$ lm run --show-result examples/07-concurrency/barrier.lm --allow Proc
Done(Ok(5))
```

The barrier example is the closed-set gate in source form. The root
never binds the helper handle: it sends the handle straight into the
worker mailbox, so a barrier from the root reaches the helper only
through that accepted message.

## The inheritance decisions

### The core registers first

The first design kept the module classes at the first indices, as
week 7 left them, and relaxed the verifier rule that a parent must
precede its child. It was rejected. The rule is a well-foundedness
invariant that the verifier, the dispatch builder, and the linker all
read, and replacing it with an explicit acyclicity check would have
traded one cheap structural fact for a new pass in a
security-relevant component. Registering the core first keeps the
invariant and matches the layering: the core image is the base, and
module code builds on it.

The cost is renumbering. Module classes now follow the core classes,
so three tests read a class, a selector, or a function index from the
module instead of a constant.

### The parent clause carries type arguments

The alternative was a type-descriptor value passed to `Proc.Run`, as
specification 23.6 spells that operation. It was rejected for the
mailbox type: a descriptor is an ordinary value, so a call site could
pair a descriptor of one type with a machine of another and the
verifier would have no way to refuse. The class table cannot be
forged that way, so both `Proc.Spawn` and `Proc.Recv` read the mailbox
type from it.

### A generic parent with a defaulted field rejects

A subclass copies the default expressions of its parent. A default
whose type names a class parameter would arrive with the parameter
free, and the verifier would reject the emitted `<new>` function with
a message about a variable outside the class arity. This slice rejects
that declaration at the checker instead, with a diagnostic that names
the field. Rewriting the default expression tree is the alternative,
and it waits.

## The launch decision

`Proc.Spawn` takes three operands: the constructor function, the proc
body function, and the argument tuple.

The alternative was one composed function that constructs the instance
and calls `on_spawn`. It was rejected because the verifier could not
recover the mailbox type from it: a composed function has the argument
types and the result type, and neither names the proc class. With the
constructor separate, the verifier reads the proc class off the
constructor result and takes the mailbox type from the class table.

The kernel runs the two frames in order, which is what specification
18.1 describes: the proc instance is constructed inside its VM.

## The deadlock decision

A send to a full mailbox blocks the sender, a receive on an empty open
mailbox blocks the proc, and `done` on a live proc blocks the holder.
Specification 18.6 says two blocked procs may deadlock, and that
supervision converts the condition into a policy-specific result.

The deterministic scheduler is the supervisor here. When the root is
blocked and no machine is runnable, it faults every blocked machine.
No run hangs, and the root fault becomes the program result. The fault
code is `HostFault`, which is the week-7 convention for "the host
cannot serve this"; the open question below asks for a better one.

## Simplifications inside the slice

- **The thread-backed mode runs one worker for the whole world, not
  one task per proc.** Specification 22.12 puts one proc on one
  scheduler task. `World` owns every machine in one table, and a
  transfer needs two machines at once, so a per-proc task would have
  to ask the owner of the table for every send, spawn, and terminal
  publication. That is a `World` redesign, not a scheduler mode. The
  worker thread that landed gives the isolation boundary that matters
  today: guest execution leaves the host thread, runs on a bounded
  stack, and returns text. It changes no semantics, and one test runs
  one program both ways and compares.
- **`Proc.Run` implements no `Type[M]` parameter.** The manifest
  schema carries it, and specification 18.2 says the mailbox-bearing
  form takes a `MailboxType[M]` that proc-class lowering creates. The
  mailbox-bearing launch in this slice is `Proc.Spawn`, so
  `sys.proc.run(vm)` always chooses `M = Never`. A type-descriptor
  value arrives with the reflection surface.
- **`Never` has no bytecode encoding.** It lowers to `()`, as every
  earlier week lowered it, so `Handle[Never, R]` encodes as
  `Handle[(), R]`. The rule that such a handle has no `send` method is
  therefore a checker rule, not a verifier rule.
- **A terminal proc keeps its record and its heap.** `done()` is
  idempotent and returns the stored result, so the record cannot be
  retired while any handle may name it. The child budget bounds the
  count, as it bounds every other machine.
- **The barrier has no guest entry point.** Week 8 defines no snapshot
  byte format, so `lm_proc::Barrier` is the Rust-level entry that the
  week-9 encoder calls. It reports the closed set, the cut marker, and
  the preflight object count.
- **A proc slot is never reused.** The generation is therefore always
  zero for a proc today. The rule is still enforced on every handle
  use, and a retired mock slot takes a new generation, so the defense
  is live and tested at the record level.
- **A self send proves the boundary rule without a copy.** A proc may
  hold its own handle, because a handle is sendable data. The copy
  path needs two distinct machines, so a same-heap send runs the
  standalone frozen check instead.
- **`pause` on a blocked proc returns `InUse`.** A blocked machine
  holds the execution references of its suspended activation stack,
  and the pause rule reads that count. Handing the holder a machine
  with a stored stack needs a rule for what `run` does to it, and
  this slice does not define one.
- **A proc that outlives its spawner keeps its mailbox and loses its
  pass-through.** The mailbox is machine state, so `send` and `close`
  still work. The next `receive` resolves through the dead parent
  table and fails closed with `PolicyDenied`, which is the rule of
  specification 18.6. One test states it, because the shape surprises
  a reader who expects the handle alone to keep the proc alive. The
  open question below carries the reviewer's argument against the
  rule.

## Changed tests

- `crates/lm-testkit/tests/checker.rs`, two cases read the entry
  function index, the module class index, and the selector index from
  the module instead of a constant.
- `crates/lm-cli/tests/cli.rs`, the disassembly case reads the entry
  index from the dump.
- `crates/lm-testkit/tests/corruption.rs` and `fixes.rs`, five cases
  find a class or a type by name instead of by index.
- `crates/lm-testkit/tests/identity_linking.rs`, one case picks a
  different function index by arithmetic instead of by position.
- `crates/lm-testkit/tests/week7_graph.rs`, the shape count is fifteen
  and the checked digest output moved with the compiler ABI version.
- `crates/lm-testkit/tests/fuzz.rs` takes `examples/07-concurrency` into the
  mutation seed corpus, raises the corpus floors, and reads the core
  role table at its new size.
- `crates/lm-heap/src/shape.rs`, the sample list, the tag table, and
  the dump case cover the `Handle` shape.
- `core/pinned-hash.txt` and `core/pinned-core-defs.txt` regenerated.
- `tests/fuzz-regressions/*.lmbc` regenerated for container version
  12. The layer-specific rejection assertions are unchanged.

## New tests

- `crates/lm-testkit/tests/week8_inherit.rs`, 18 cases: seven for
  core-class inheritance and eleven for generic-parent inheritance,
  including the arity rules, the unbound argument, the override rules,
  the recorded class entry, and the class listing.
- `crates/lm-testkit/tests/week8_procs.rs`, 38 cases: the mailbox
  model rules, the handle rules, the spawn rules, ownership, pause and
  resume, parent death, revocation, determinism, the deadlock rule,
  the nested block, the birth grant, the child budget, the fuel
  budget, the readable dumps, and the three example outputs.
- `crates/lm-testkit/tests/week8_barrier.rs`, 9 cases: the closed set,
  the mailbox cut, the frozen mailbox, overlapping and disjoint
  barriers, the live host attachment, the run set, the closed-set gate
  stated directly, and the closure over all five machine-reference
  shapes.
- `crates/lm-testkit/tests/week8_worker.rs`, 3 cases: the worker
  thread against the host thread, deep guest recursion on the bounded
  stack, and a bad grant reported instead of a panic.
- `crates/lm-proc/tests/records.rs`, 2 cases: the crate names no heap
  or value crate, and every published record is plain data.
- `crates/lm-testkit/tests/corruption.rs`, 6 new cases for the parent
  type arguments, the `Proc` core role, the handle message type, and
  the receive receiver.
- `crates/lm-vm/src/world.rs`, 4 cases for the self send, the stale
  generation, and the retired slot generation.
- `crates/lm-graph/src/mode.rs`, 2 cases for the standalone sendable
  check against the standalone frozen check.
- `crates/lm-testkit/tests/bench_smoke.rs`, four proc entries.
- `tests/ui/`, 5 new pairs for the spawn, send, and receive rules.
- `tests/run-pass/core-subclass.lm`, one pair for core-class
  inheritance.
- `tests/fuzz-regressions/`, 2 new source seeds for the generic parent
  and the proc surface.
- `crates/lm-cli/tests/cli.rs`, 3 new cases: the worker example twice
  through the tool, the same program from its artifact, and a proc
  package through the build loop.

Test count: 620 before, 710 after.

## Measurements

`cargo test -p lm-testkit --test bench_smoke -- --nocapture --exact
<name>`, debug profile, one entry per process.

| Entry | Week 7 | Week 8 |
|---|---|---|
| `alloc_gc_100k` | 79.6 ms | 80.0 ms |
| `perform_mock_5k` | 17.6 ms | 19.2 ms |
| `list_push_100k` | 70.1 ms | 69.0 ms |
| `freeze_chain_50k` | 50.1 ms | 50.1 ms |
| `transfer_graph_20k` | 30.1 ms | 29.1 ms |
| `digest_graph_20k_plus_1k_cached` | 33.8 ms | 27.4 ms |
| `mark_sweep_100k_under_256k` | 106.7 ms | 107.6 ms |
| `proc_send_receive_20k` | — | 78.6 ms |
| `proc_spawn_500` | — | 13.3 ms |
| `proc_pause_resume_5k` | — | 10.5 ms |
| `proc_terminal_200x200` | — | 33.1 ms |

No week-7 entry regressed. The proc entries set the baseline: one send
and one receive plus the scheduler slice that carries them cost about
four microseconds, one spawn about twenty-seven microseconds, and one
pause and resume pair about two microseconds.

## The gates

| Gate | State |
|---|---|
| Message and result types never erase to `Any` | Met. `no_proc_operation_erases_its_types` reads the eight manifest schemas, and `Handle[M, R]` carries both types through the checker, the bytecode, and the verifier. |
| Handle transfer preserves the exact proc reference | Met. `a_transfer_keeps_the_proc_identifier_and_generation` and the `mailbox-handle` example. |
| The barrier set is closed | Met. `every_handle_in_the_set_targets_a_member_of_the_set` walks the whole set after the barrier. |
| FIFO acceptance, close/drain, pause/resume, parent death, revocation, and dead-peer behavior have deterministic model tests | Met. Six cases in `week8_procs.rs`, all on the deterministic scheduler. |
| Mailbox limits are checked before copy and acceptance | Met. `a_full_mailbox_blocks_the_sender_before_the_copy` runs a bound of one and reads the trace and the metrics. |
| One VM never executes concurrently | Met. `one_machine_runs_at_a_time` drives the loop by hand and reads the run set at every step. |
| The barrier records one consistent mailbox cut | Met. `the_barrier_records_one_mailbox_cut` and `a_frozen_mailbox_blocks_the_sender_instead_of_accepting`. |
| Barrier failure resumes every paused machine | Met. `a_live_host_attachment_blocks_the_barrier_and_resumes` and `overlapping_barriers_serialize`. |
| Scheduler records contain no guest heap reference | Met. `the_crate_depends_on_no_heap_or_value_crate` and `every_record_is_plain_data`. |
| Proc send/receive, spawn, pause, and terminal publication benchmarks are committed | Met. Four entries above. |

## The self-review pass

### A self send reached an assertion

`World::transfer` splits the machine table into two mutable halves and
asserts the two identifiers differ. A proc may hold its own handle,
because a handle is sendable data, so `h.send(x)` inside the target
proc reached that assertion.

The fix is `World::send_copy`. A send between two machines copies, as
before. A send inside one machine keeps the value where it is,
because the codec has no second heap to copy into.

The first version of that path ran the frozen check alone, and the
independent review found the gap: a transfer demands two things, the
frozen bit and a sendable shape, and the frozen check answers only
the first. A machine handle is born frozen and holder local, so the
frozen check would have let one into a mailbox that a copy refuses.
No guest program reached the gap, because a mailbox proc carries the
`Proc` group alone and can mint no holder-local native, but the two
paths must not differ.

`lm_graph::verify_sendable` closes it. The standalone walk carries the
same `CopyCheck` visitor the copy carries, so the two answers cannot
drift. Four record-level cases state the rule: a scalar, a frozen
sendable graph, a mutable graph, and a holder-local value both at the
top of a message and inside a frozen container.

### A machine of a suspended stack stayed in the run set

The first run set admitted every scheduler-owned machine whose
execution reference count was zero. A machine whose activation stack
is suspended still holds those references, so a blocked proc never
became runnable again and the world reported a deadlock instead.

The fix reads the suspended table: a machine is runnable when its own
stack is the stored one, or when it holds no execution reference at
all. A running machine is one that a suspended stack left mid flight,
so only its own base activation may pick it up again.

### Two spawn shapes reached the verifier instead of a diagnostic

A proc class that inherits its `on_spawn` and a `spawn` inside a
generic callable both compiled and then failed in the verifier. Both
now work.

An inherited `on_spawn` declares its own class as the receiver, and
the constructed instance is a subclass of it. The spawn rule compares
by subtyping now, and the checker records the declaring class so the
right closure type reaches the module type table.

The closure rule said a closure body must keep the enclosing generic
arity. That is right for a closure body, which shares the generic
scope of the function that creates it. It is wrong for a target that
declares no generic parameter at all: such a signature holds no free
variable, because the signature rule already bounds every variable by
the target's own arity. The rule now admits a closed target from any
scope, which is what the `spawn` sugar needs.

### The barrier set missed three machine reference shapes

The first walk followed machine handles and proc handles. A
policy-table handle, a request token, and a typed call token also
name a machine, and each can be the only reference a heap holds. The
walk follows all five now, and one test builds a world where a table
handle alone names a machine.

### The manifest version did not move

The manifest rule states that a membership change increments the ABI
version, and the week added eight members. The bump to version 2 moved
`manifest_digest`, and therefore every definition hash, the core pin,
and the checked digest output.

## The independent review pass

A second reviewer read the whole diff. It found no correctness or
safety defect, confirmed the ten gates, and validated the self-review
findings. It asked for one fix and two records, and it stated a
position on two of the open questions.

### The self-send path admitted a holder-local value (fixed)

The finding and the fix are in the self-review section above. The
reviewer noted that the gap is unreachable today and named the two
surfaces that would open it: attenuated send-only handle views, and a
proc launched through the explicit machine path with more than the
`Proc` group.

### A receiver heap limit faults the sender (recorded)

`World::send_copy` copies the message into the receiving heap. A
`HeapLimit` there faults the *sender*, because the whole copy is one
operation of the sender. The open question below asks whose fault it
is.

### The barrier set over-approximates through mock closures (recorded)

The carry-forward below states it for the week-9 encoder.

## Open questions

### The build-order example omits the freeze (decided 2026-08-16)

The project owner decided this question. Boundary crossings adopt copy
semantics, and the build-order example stands as written. The decided
section at the end of this note holds the record.

The build order writes `h.send(Double(21))`. Specification 10.3 checks
frozenness at proc send, and specification 16.2 rejects a mutable
graph with `UnsendableValue`. A freshly constructed enum case is
mutable, so the two texts disagree.

The implementation follows the specification: the example writes
`h.send(Double(21).freeze())` and a test states that the mutable form
faults the sender. The alternative is an implicit freeze at send,
which specification 10.2 rules out ("there is no silent mutable deep
copy"). The question is whether the build-order example is shorthand
or a claim about the send rule.

The reviewer's position: specifications 10.2, 10.3, and 16.2 are
right, and the build-order example is the text that must move. This
note records the position; the decision belongs to the project owner,
and neither `docs/specs` nor the build order changed this week.

### Who owns the fault when a receiver heap limit stops a message

`send` copies the message into the receiving heap, and that heap has
its own cap. A copy that exceeds it faults the sender with
`HeapLimit` today, because the copy is one operation of the sender
and every other boundary failure of `send` faults the sender the same
way. `SendResult` has no arm for it, and `Fault(fault)` names "a dead
target, cancellation, or another target/host supervisory fault",
which reads like a target-side condition.

The specification does not say. The two readings are:

- the sender asked for something the world could not do, so the
  sender faults, which is the codebase convention and what landed;
- the target could not accept the message, so the sender receives
  `SendResult.Fault` and keeps running, which lets a supervisor
  survive an overloaded peer.

The second reading is attractive for supervision, and it needs the
copy to become failure-atomic from the sender's point of view. The
transfer is already failure-atomic for the destination heap, so the
change is small; the semantics decision is not.

### Should a proc still receive after its parent dies (decided 2026-08-16)

The project owner decided this question. Uniform pass-through stays.
The decided section at the end of this note holds the record.


Specification 18.6 says a child table passes through the live parent
table, and that parent death removes those pass-throughs so future
requests fail closed. `Proc.Recv` resolves through that chain like
every other operation, so an orphaned proc cannot receive. Its
mailbox still accepts, because a mailbox is machine state, so a
holder can send and close and then read `Fault(PolicyDenied)` from
`done()`. The behavior follows the text, and one test states it.

The reviewer's position: `Proc.Recv` reads the proc's own mailbox and
reaches no authority outside the machine, so it is unlike `Io.Write`.
The specification should anchor it to a root grant, or to the birth
grant itself, so a proc can drain its mailbox and finish after its
parent dies. Under the current rule a supervisor cannot hand work to
a proc and then exit, which is a common shape.

The counter-argument is that the birth grant is an ordinary table
entry and the pass-through rule is uniform: making one operation
special would put a second resolution rule beside the first. This
note records the position; the decision is open, and it is a
specification change.

### No stable fault code names a deadlock

Specification 12.3 has no code for "no machine can make progress". The
scheduler uses `HostFault`, on the week-7 reading that the host cannot
serve the request. A dedicated code, for example `Deadlock`, is the
better answer, and it is a specification change this week did not
make.

The same week-7 gap is still open for a refused resource budget: a
spawn past the child budget faults `InvalidVmState`.

### A generic parent that declares a defaulted field

The checker rejects a subclass of a generic parent when an inherited
field default names a class type parameter. The rejection is precise
and the diagnostic names the field, but the rule is a limitation of
the lowering, not of the language. Whether the default expression tree
should be rewritten with the parent arguments, or whether the
specification should forbid the declaration, is open.

### Is a blocked proc call machine state?

Every proc operation is classified machine state, because a blocked
call waits on another machine of the same world and the block record
holds identifiers only. Specification 17.2 lists blocked receive state
inside snapshot contents, which supports the reading. It also lists
pending host operations outside them, and a blocked `send` or `done`
is neither: it is a pending request whose completion the scheduler
supplies. The classification says the scheduler is part of the world,
which the specification does not state in those words.

### The classification is still outside the manifest digest

The week-7 question stands: `OpDef.snapshot` is manifest content, and
`manifest_digest` does not cover it.

## Carry-forwards for week 9

### The barrier set over-approximates through mock closures

`World::machine_references` and `World::snapshot_preflight` both walk
`Machine::gc_roots` (`crates/lm-vm/src/machine.rs`, the mock-handler
loop near the end of `gc_roots`). That root set includes every
`Action::Mock` closure a policy table holds, because the collector
must keep those closures alive.

Specification 17.2 excludes policy tables from snapshot contents, so
those closures are not snapshot content. The barrier therefore
over-approximates: a machine that only a table-held mock closure
names is stopped, frozen, preflighted, and counted in the closed set.

The over-approximation is harmless this week. A larger stopped set is
still a consistent cut, and the barrier resumes everything it stopped.
It is not harmless for the week-9 encoder:

- the encoder must not treat table-only reachability as snapshot
  content, or a restored world would carry policy the specification
  says restore never carries (17.5, fresh default-deny tables);
- the machine ordinal assignment must come from a walk over snapshot
  content, not from the barrier set, or the ordinals would name
  machines the bytes do not hold.

The clean split is two root sets: the collection roots, which keep
everything alive, and the snapshot roots, which are the collection
roots minus the policy-table entries. `World::machine_references` and
`World::snapshot_preflight` should read the second one.

### The self-send and cross-heap paths must stay in step

`World::boundary_copy` runs `lm_graph::copy_within` for a same-heap
send and `lm_graph::transfer` for a cross-heap send. Both carry the
`CopyCheck` visitor of `crates/lm-graph/src/mode.rs`, which is what
keeps the two answers equal. A new boundary rule must go into that
visitor, not beside it.

The decision of 2026-08-16 renamed the path and gave the same-heap
send a real copy. The rule above is unchanged: one visitor serves both
paths.

## Deferred work

- One scheduler task per proc, with the `World` split that a per-proc
  task needs. The worker thread that landed is the isolation boundary,
  not the concurrency model.
- The snapshot byte format, restore, and the encoder that consumes the
  barrier report. Week 9 lands them.
- `Type[M]` descriptors and the `Proc.Run` mailbox form.
- Retiring a terminal proc record. The child budget bounds the count;
  nothing bounds the retained heap of a finished proc.
- Attenuated send-only handle views, which specification 18.5 defers.
- Rewriting an inherited field default with the parent type arguments.
- A `Never` encoding in the bytecode type table, which would move the
  "no `send` on a `Never` mailbox" rule into the verifier.
- Distribution, which specification 18.7 does not require in version
  0.2.
- Committed benchmark distributions, `cargo-fuzz` targets, Miri, and
  CI workflow files stay deferred as before.

## Decided after the week (2026-08-16)

The project owner decided three questions after the week closed. Each
item records the decision, the repealed rule, and the reason.

### Boundary crossings copy the value

The rule is one sentence. A crossing of a machine boundary copies the
value. Sharing inside one crossing is preserved. Nothing is shared
across a boundary, and identity does not cross. A mutable graph copies
as a mutable graph.

The frozen requirement at boundaries is repealed. The repealed
principle is the last clause of specification 10.3: "there is no
silent mutable deep copy". The copy is no longer silent, because the
specification now states it as the boundary rule.

Freeze keeps its other work without change:

- a map key must be frozen and digestible at insertion;
- `digest()` and `deep_equal` need a frozen graph;
- a program may freeze a graph to make it immutable on purpose.

Two categories still refuse to cross, with the errors of week 8:

- a holder-local control handle: `Vm`, `PolicyTable`, `Request`, and
  `PendingCall`;
- a live host attachment.

A proc handle still crosses as a reference, because send rights are
shared on purpose. A machine still forks only through an explicit
snapshot, never through a send.

The reasons for the decision:

- a snapshot already copies a whole machine world, so the copy rule
  makes one boundary story instead of two;
- freeze becomes an opt-in guard rather than a toll on every send;
- frozen sharing and copy elision stay available, because the
  specification says an implementation may elide a copy that no
  program can observe.

The implementation follows in `crates/lm-graph/src/mode.rs`. The
`CopyCheck` visitor reads shapes alone, and the copy preserves the
frozen bit of each source object. `lm_graph::copy_within` copies
inside one heap, so a self send copies as well. Both send paths still
run one visitor, which the week-8 carry-forward asked for.

One new static rule joins the send rule: a mailbox message type must
not name a holder-local native class. The checker rejects the proc
class declaration with `E1056`. `Handle[M,R]` stays a legal message
type.

The rule reads the whole message type. It walks composite types,
generic type arguments, and the declared fields of every class it
reaches, enum arms included. A message carries the whole graph, so a
holder-local field is a holder-local message. A class graph may hold a
cycle, so the walk visits each class once. The pass runs after class
resolution, because it reads the field types.

### The receiver-heap fault attribution stays open

The question above ("Who owns the fault when a receiver heap limit
stops a message") is unchanged and still open. It now covers mutable
copies as well, and those copies are larger than the frozen graphs of
week 8, so a receiver heap limit is easier to reach.

### Uniform pass-through stays

A perform of a proc resolves through the live parent table. Parent
death removes the pass-through, and the next request fails closed. The
reviewer's position, that `Proc.Recv` must survive parent death, was
considered and declined.

The discipline is that a spawner outlives its workers. The
spawn-and-hand-off pattern is wrong by design, and no grant survives
the death of the parent.

The semantics do not change. Only the failure text changes: the
`PolicyDenied` message now names the cause, for example "the operation
Proc.Recv lost its pass through: the parent machine is gone". The
pinned test reads the message from the orphan machine.

The week-17 API review revisits this rule with corpus experience.

### `close` is the end-of-stream signal, not boilerplate

`close` is the typed end-of-stream signal of a streaming flow. A proc
that drains a loop needs it, because the loop exits on `Closed`.

A request-and-reply flow needs `send` and `done` alone. The proc
receives one message and terminates, so a close adds a third call and
teaches boilerplate. The narrative examples in `examples/07-concurrency`
follow this rule now, and the streaming example carries one comment
that states why its close is there.

Week 11 queues a `finish` convenience for `std/proc`: one library call
that closes and then waits. The primitives stay separate.

## Open questions after the decision

### Does the copy rule reach the link and compile envelope

Specification 3.6 requires one frozen compatible value per import
slot, and linking deep-freezes the result. Specification 16.1 lists
linking and imports as codec contexts, and a codec context now copies.
The two texts read differently, and this note does not resolve them.

### The builders are holder-local shapes

Correction. An earlier version of this item said that the reference
implementation stores a `Bytes` value in a `ByteBuffer`. That is
wrong, and the correction matters for week 9.

Version 0.2 has no `Bytes` value at all. `lm-types` declares no
`Bytes` type, the shape table of `crates/lm-heap/src/shape.rs`
declares no `Bytes` shape, and a byte literal rejects at the scanner
with `E0009`. Specification 16.2 lists bytes among the sendable
values, so the specification is ahead of the implementation. There is
no shape to reclassify today.

Week 9 needs the reverse direction: a snapshot image must cross as
bytes. The new test `every_shape_declares_its_boundary_column` states
the boundary column of all fifteen shapes, so the week-9 shape makes a
deliberate choice and any later change shows in one diff. A `Bytes`
shape must be sendable machine state, which matches 16.2.

`StringBuilder` and `ByteBuffer` stay holder-local. They hold growable
private buffers and produce immutable outputs (22.9), and no canonical
encoding names them. Under copy semantics a builder could copy like
any other mutable graph, because the copy rule needs no frozen bit.
The reasons to keep them holder-local are the missing canonical
encoding and the risk of a silent large copy, not the repealed frozen
rule. Whether a builder becomes sendable is a future owner call.

The new mailbox rule follows the shape table, so `Proc[ByteBuffer]`
and `Proc[StringBuilder]` reject as well. A decision that makes a
builder sendable changes the shape table, and the mailbox rule follows
it without further work.
