# Week 9 Fixes

This note states the snapshot and virtual machine design after week 9.
`docs/specs/sidecar/snapshot-image-admission.md` holds the normative design.

The work ran in three parts:

- decoding separates from admission, and the virtual machine hardens
  against a wrong-typed value;
- restore and the world contain their failures and their resources;
- the scheduler drops its machine scans.

One task stays open. "Deferred work" below states it.

## Admission and the hardened virtual machine

### The two host states

`Image` is editable snapshot data. It has public fields and permits any
edit. `SnapshotImage` is the admitted, immutable form.

- `codec::decode(bytes, limits)` reads the bytes and the limits alone,
  and returns `Image`.
- `admit(image, module, budget)` proves structure and returns
  `SnapshotImage`.
- `load_external(bytes, module, limits)` calls both, then stores the
  canonical bytes beside the admitted `Image`.
- `World::restore_image` takes `&SnapshotImage`. The trusted image
  cache stores `SnapshotImage`.
- `codec::from_trusted_capture` builds a `SnapshotImage` for
  `World::capture_snapshot`. It stays private to
  `crates/lm-vm/src/snapshot/`.
- `SnapshotImage::into_image` returns an editable copy. That copy needs
  `admit` again before a restore.

`Origin` records whether trusted capture or `load_external` produced
the bytes. It serves diagnostics. Both origins give the same
guarantees.

### What admission proves

Admission proves structure:

- the image holds one root machine;
- every machine ordinal, object ordinal, function, class, type, and
  operation identity resolves;
- every code identity matches verified code;
- every frame names a reachable instruction boundary;
- every frame environment ordinal resolves;
- frame bases fill the local arena exactly, the bottom frame starts the
  operand arena, and no later frame lowers the operand base;
- every object holds its required field or element count;
- every literal entry names its exact program literal;
- the parent graph is a forest;
- every machine reference stays inside the captured world;
- every lifecycle record agrees with its state;
- the closed type table holds no free variable, stays acyclic, and each
  arity matches its declaration.

Admission proves the type of no stored value.

### How a type stays honest

Two mechanisms carry type honesty. Neither reads a type from the image.

**The interpreter tests each tag.** Every reader of a typed value tests
the tag and raises a machine fault on a mismatch. A wrong-typed value
in a restored machine faults that machine.
`crates/lm-vm/ASSERTIONS.md` lists each assertion that became a fault,
each assertion that stays, and the rule that carries the ones that
stay.

**The world checks each VM boundary.** A value that crosses a VM
boundary is checked against the type the receiving code expects. The
expected type comes from the `reply_ty` field of `Perform` and
`PerformValue`, substituted through the type environment of the
performing frame. The verifier proves that `reply_ty` agrees with the
type at that program point.

The boundaries are the terminal result read, the mailbox receive, the
pending call reply, the spawn argument, the mock reply, and the restore
that returns `Vm[T]` or `Snapshot[T]`. The check runs in
`World::install_value_reply` and `World::check_frame_args`.

The check descends every element and every field. A closure also meets
the verified closed signature of its function. The comparison includes
parameters, mutation markers, the result, and the effect row.

A native handle takes a shape test alone, because its arguments name
another machine or operation. Each handle later produces a value that
crosses a boundary of its own.

The graph copy and the type check use separate bounded walks. A copy
visits each object once. The type check visits each object and
expected-type pair, because sharing can give one object several expected
types.

The check has full force where the performing frame is live. Where the
performing frame is restored, its type environment came from the image,
so the expected type is one an editor chose. The interpreter tag tests
are the guarantee there.

Two states stay representable. An empty container reached under two
argument lists satisfies both, and a restored frame can carry any
environment. A read of either takes a fault.

**A world that restored nothing checks no boundary.** Ordinary
execution builds every value through verified code, and the verifier
proved the type of each one at the program point that built it. A
restore is the one path that states a value the verifier never saw, so
`World::restored_any` turns the check on when a restore commits.

The flag names the world rather than one machine. A machine-level rule
must follow the source of each value, and a value reaches a boundary
from a heap, a mailbox, or a host reply, so each source needs its own
record. A per-machine rule that reads the receiving machine is wrong: a
restored machine that spawns a child passes its values into a machine
no restore built.

### The type environment

`crates/lm-bytecode/src/closed.rs` holds `ClosedType`, `TypeEnv`, and
`TypeEnvs`. Each `ClosedType` node carries a content digest, so one
closed type has one identity in every process.

`Frame`, `Object::Closure`, `Object::Instance`, and `Machine` each
store one `TypeEnvId`. `TypeEnvId(0)` is the empty environment, so a
monomorphic call copies zero and allocates nothing. `call_generic`
builds an environment with `envs.derive(module, parent, app)`, and the
table caches each derived environment by its parent and its
application.

`TypeEnvs` belongs to one `World`. `restore_image` re-interns each
record of the image into the target world and rewrites each stored
identifier.

`lm_graph::digest` and `deep_equal` skip the `TypeEnvId` of an object.
`Object::shell` and `Object::remap` copy it, so a closure keeps its
creator environment across a boundary.

The environment carries the substitution for a boundary check inside a
live generic frame. A later `Type[T]` surface reads the same table.

### Limits

| Limit | Value | Where |
| --- | --- | --- |
| type nesting depth | 128 | `MAX_TYPE_DEPTH`, `MAX_CLOSED_DEPTH` |
| closed type nodes per world | 65536 | `DEFAULT_MAX_CLOSED_TYPES` |
| type environments per world | 65536 | `DEFAULT_MAX_TYPE_ENVS` |
| admission work | 1 << 24 units | `AdmissionBudget` |
| decoded allocation cost | 1 GiB | `DecodeBudget` |
| machine records per proc tree | 4096 | `WorldLimits` |
| live heap bytes per proc tree | 1 GiB | `WorldLimits` |
| live heap objects per proc tree | 1 << 24 | `WorldLimits` |
| live host resources per proc tree | 1 << 16 | `WorldLimits` |
| instructions per proc tree | 1 billion | `WorldLimits` |
| trace events per proc tree | 1 << 20 | `WorldLimits` |
| admitted-image cache per proc tree | 256 MiB | `WorldLimits` |

The depth limit is 128 for now. The cost of a type walk stays
acceptable far above that, so a later change can raise it. The deepest
type in the test suite is about 80, in
`crates/lm-testkit/tests/complexity.rs`.

Every walk over a type is iterative, and stays iterative. A depth limit
is a second line, not a replacement.

Polymorphic recursion is legal, so a program can deepen a closed type
as it runs. Such a program takes a local fault at the depth limit or at
the node cap.

### Failure and resource containment

Restore uses a detached `RestorePlan`. Preparation builds every heap,
machine record, relocation table, and effective limit before commit.

Commit installs prepared machines and type entries without an
allocation. A failed plan leaves the target machine unchanged.

Request tokens use one monotone ordinal. Admission rejects an ordinal
at or above the target counter. Runtime checks still reject stale tokens.

Ordinal exhaustion faults the requesting machine. Mailbox metrics
saturate at their maximum values.

The root VM and all spawned procs share one `WorldBudget`. Local
`VmConfig` limits remain local ceilings and carry no aggregate balance.

The admitted-image byte limit controls cache retention only. It does
not reject an image. Eviction makes a later restore repeat admission.

The heap and resource registries charge shared ledgers. Dropped plans,
garbage collection, closed resources, and terminal proc compaction release charges.

The decoder charges all decoded vectors and byte copies to one
`DecodeBudget`. Admission charges all structural records and graph edges.

Nested snapshot bytes stay opaque. Their own restore starts a new load
and new budgets when the trusted cache has no admitted image.

Container hashing streams the domain and prefix. The external loader
uses the validated stored hash and does not hash the prefix again.

### The scheduler indexes its tasks

`crates/lm-vm/src/schedule.rs` holds the scheduler view. A ready index
names the tasks that can run. A blocked index names the tasks that wait
on each wake source: a mailbox message, mailbox capacity, or a terminal
result. `ScheduleEvents` coalesces the state changes one execution
slice produced, so a completion wakes the tasks that completion
affects.

The scheduler scanned every machine for a runnable or blocked proc
before this change, and it allocated one vector for each scan. A
terminal record stayed in the scan set.

Each task runs one bounded quantum and requeues. A host wait stays
outside the scheduler, so one proc waiting on a host completion leaves
the others runnable.

The indexes pay for themselves once a world holds many procs.
`proc_spawn_500` runs 31 percent faster. A world of two or three procs
pays the index maintenance and skips no scan worth avoiding, so
`proc_send_receive_20k` and `proc_pause_resume_5k` each cost about 10
percent more. The crossing point between the two is not measured.

### Versions

| Item | Value |
| --- | --- |
| bytecode format | 14 |
| interface format | 5 |
| compiler ABI | 8 |
| verifier | 8 |
| operation manifest ABI | 4 |
| snapshot container format | 2 |

`identity_of` hashes every field of an `OpDef`, including
`OpDef.snapshot`. `manifest_digest()` covers each operation identity,
and `verification_hash` covers `manifest_digest()`.

`Perform` and `PerformValue` carry `reply_ty`, which moved the bytecode
format.

Verifier version 8 fixes generic parent subtype and join rules. It also
makes shared type DAG walks visit each node or pair once.

`core/pinned-core-defs.txt` follows `manifest_digest()`.
`core/pinned-hash.txt` covers source content alone.

`crates/lm-abi/src/fault.rs` holds `TypeMismatch` and `MalformedState`.
Both appear in the stable table of language specification 12.3.

### Tests

`cargo test --workspace` runs the full workspace suite and exits 0.

`every_capture_of_every_shipped_program_admits`
(`crates/lm-testkit/tests/admission.rs`) compiles each `.lm` file under
`examples/` and 25 crafted sources, captures each program at each
bytecode boundary of a bounded prefix, and passes the canonical bytes
through `load_external`. Each capture must admit. The crafted sources
include a proc handle past its constructor, a closure a generic body
built, a machine whose entry function is generic, a `sys.proc.run`
handle, a closure inside a closure inside a generic body, a generic
class with a generic field, and polymorphic recursion captured while it
runs.

`mutated_snapshot_images_never_panic_the_runtime`
(`crates/lm-testkit/tests/fuzz.rs`) mutates a decoded `Image`
structurally, re-canonicalizes the heap, then admits, restores, and
drives each mutant. Of 3200 mutants, 2468 admit and restore, and 1198
execute instructions. Zero mutants panic. Each case runs under a 1 MiB
heap, 20000 fuel, and a 10-second clock, so the case covers a hang as
well. `mutated_snapshot_containers_never_panic_the_loader` mutates the
bytes and reseals each mutant.

Four type-confusion cases fault `TypeMismatch` after restore: a local
typed `[Int]` that names a list of strings, an instance of one class at
a position of another, a `Vm[T]` that names a machine of another result
type, and a handle that names a machine with another mailbox type.

Two cases state the depth limit: a type past it rejects, and a type at
it walks on a 256 KiB stack.

`a_fallible_read_keeps_the_value_size` pins the size of a fallible
read. `FaultCode` is 1 byte, `Value` is 16 bytes, and the tag of
`Value` holds a niche, so `Result<Value, FaultCode>` is 16 bytes.

### Measurements

State sizes: `Frame` is 36 bytes, `Machine` is 736 bytes, and `Object`
is 80 bytes. The two `TypeEnvId` fields fit inside the padding of
`Object::Map`, so `Object::cost()` and the heap byte accounting hold.
`a_fallible_read_keeps_the_value_size` pins each size.

Container sizes: a wide heap of 10000 list elements is 90425 bytes, a
deep chain of 5000 instances is 125415 bytes, and a machine world of
three machines is 843 bytes.

Benchmarks: `cargo test --release -p lm-testkit --test bench_smoke --
--nocapture --test-threads=1`.

Read the method before the figures. A benchmark of this suite varies by
5 to 50 percent between passes of one unchanged binary, and one build
directory can run 24 percent faster than another for the same commit.
A comparison of two commits therefore holds only when it alternates
between them inside one build directory, in one session. Every figure
below comes from five alternating rounds of that shape, reported as the
median of each side.

Against `c87f9de`, the tree before this work:

| Entry | Before | After |
| --- | --- | --- |
| `proc_spawn_500` | 2.06 ms | 1.36 ms |
| `virtual_call_100k` | 8.23 ms | 8.29 ms |
| `transfer_graph_20k` | 4.95 ms | 4.95 ms |
| `proc_pause_resume_5k` | 1.50 ms | 1.77 ms |
| `perform_group_pass_20k` | 1.32 ms | 1.54 ms |
| `perform_exact_pass_20k` | 1.29 ms | 1.52 ms |
| `perform_mock_5k` | 2.17 ms | 2.51 ms |
| `alloc_gc_100k` | 7.88 ms | 8.68 ms |
| `drive_interception_5k` | 2.50 ms | 3.05 ms |
| `mark_sweep_100k_under_256k` | 8.43 ms | 9.89 ms |
| `proc_terminal_200x200` | 3.14 ms | 3.64 ms |
| `proc_send_receive_20k` | 8.39 ms | 11.20 ms |

`proc_spawn_500` runs 31 percent faster, from the scheduler indexes.
The interpreter core holds: `virtual_call_100k` and `literal_loop_200k`
stay where they were, so the fallible value readers cost nothing
measurable. `transfer_graph_20k` returns to its baseline.

Three costs remain, and each has a known cause:

- the type environment adds about 18 percent to a message-passing
  program. `Frame`, `Object`, and `Machine` each carry one identifier,
  and a generic call derives one;
- the shared heap ledger adds 10 to 17 percent to allocation and
  collection. Each allocation charges the ledger, and each sweep
  releases it;
- the scheduler indexes add about 10 percent to a world of two or three
  procs, which pays the index maintenance and skips no scan.

`proc_send_receive_20k` carries all three, so it stands 33 percent
above its baseline.

## Open questions

### A core enum instance stores `TypeEnvId(0)`

`World::build_host_value` builds `Option`, `Result`, `RunResult`,
`StepEvent`, `DriveEvent`, `Recv`, and `ProcResult` instances outside
`Instr::New`. Their class arguments follow from the `OpDef.reply`
types, and `lm-vm` reads the manifest as data alone, so those instances
store `TypeEnvId(0)`.

The boundary check reads no instance witness, so no proof depends on
this. A later reflection query on a core enum value would read its
arguments from the position instead.

Two answers exist. The first carries the `OpDef.reply` types inside
`lm-vm`. The second derives the witness at the perform.

### `Vm.FromObject` closes its argument types through the closure

`World::check_frame_args` closes the declared parameter types through
the environment of the closure. For a restored program value that
environment is image data. The live case is exact.

### An `EmptyVm` lifecycle rule left admission

`Vm.Run` on an empty machine faults `InvalidVmState` in the kernel, so
the case is contained rather than rejected.

## Future optimizations

**Restore the operand depth rule and make `Machine::pop` infallible.**
`pop` carries 7 to 9 percent on three benchmarks, and it covers one
case: an operand arena that is completely empty. A pop that reads past
its own frame into a lower frame is contained by the typed readers
instead.

The verifier dataflow computes stack depth beside the types it proves,
and depth is a structural fact. An admission rule that proves the arena
holds exactly the operands the program point requires would make an
empty arena unrepresentable, and `pop` could return `Value` again.

**Raise the type depth limit.** 128 is conservative. A later change can
raise it well above that.

**Cache `ResolvedTypes` per module.** The structural verifier pass runs
once for each `admit` call.

**Reduce the `Ctx::join` cost.** The join tests the subtype relation at
each level, so it costs the square of the type depth. The depth limit
bounds it.

## Deferred work

### Consolidate the snapshot capture work

This task is the one item of this effort that nothing implements.

`World::capture_snapshot` walks the machine references of a world
several times. It repeats the object traversal during ordering,
preflight, and encoding. Several lookups use `Vec::contains` and
`Vec::position`, so a wide machine world approaches quadratic work.

The task:

- add one reusable `CapturePlan`;
- record the deterministic machine order once, and store the
  machine-to-ordinal index;
- record each machine object order once, and store the
  object-to-ordinal index;
- reuse the preflight facts during encoding;
- reserve the final container once;
- stream the section hashing where the format allows it;
- charge the capture work to one aggregate budget;
- confirm that no linear ordinal search remains.

The measured cost today sits in the write column of the snapshot
benchmarks. `snapshot_deep_chain_5k` writes 5001 objects, and a wide
machine world is the shape that suffers, so the task needs a benchmark
with many machines before it can show a result.

### Other deferred items

- Nested snapshots use lazy admission and independent load budgets.
- Interfaces, conformance, dispatch, and a guest `Type[T]` stay
  outside this work. Specification section 14 records what
  `ClosedType` leaves in place for them.

## Maintenance

`checkpoints/asked-tree.lms` and `tests/fuzz-regressions/*.lms` match
`*.lms` in `.gitignore`, so the commits leave them out. A fresh
checkout regenerates both:

```sh
nix-shell --run "cargo run -p lm-cli -- snapshot save --allow Proc,Vm,Clock \
  checkpoints/asked-tree.lm checkpoints/asked-tree.lms"
nix-shell --run "cargo test -p lm-testkit --test fuzz regenerate_fuzz_corpus -- --ignored"
```

`docs/notes/week9.md` calls `checkpoints/asked-tree.lms` a checked-in
container. That statement is wrong, and this note corrects it.

`docs/notes/week9.md` states that the container omits a type table.
Container formats 2 and 3 carry one.

Its format table there still lists four sections. The current format
holds five sections.

Format 3 also stores nested control edges and routed requests.
