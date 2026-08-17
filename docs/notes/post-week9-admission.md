# Post-week-9 admission status

This note states the snapshot and virtual machine design after week 9.
`worklist.md` holds the worklist. `docs/specs/snapshot-image-admission.md`
holds the normative design.

The work has three delivery groups:

- group A separates decoding from admission, and hardens the virtual
  machine against a wrong-typed value;
- group B contains failures and resources;
- group C removes the known scaling defects.

Group A is complete. Groups B and C hold worklist items 7 to 12.

## Group A

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

### Group B containment

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
--nocapture --test-threads=1`. Each figure is the median of seven
passes. The spread column is the range of those seven passes, as a
percentage of the median, so a reader can tell a real movement from
measurement noise.

| Entry | Median | Spread |
| --- | --- | --- |
| `alloc_gc_100k` | 8.64 ms | 11% |
| `async_wait_50` | 0.60 ms | 42% |
| `build_2_modules` | 1.45 ms | 14% |
| `digest_graph_20k_plus_1k_cached` | 3.65 ms | 18% |
| `drive_interception_5k` | 3.23 ms | 14% |
| `freeze_chain_50k` | 6.23 ms | 18% |
| `list_push_100k` | 4.75 ms | 20% |
| `literal_loop_200k` | 5.72 ms | 4% |
| `many_class_load_300` | 1.51 ms | 21% |
| `map_insert_4000` | 0.85 ms | 52% |
| `map_insert_32000` | 5.50 ms | 33% |
| `map_put_300` | 0.31 ms | 51% |
| `mark_sweep_100k_under_256k` | 9.84 ms | 12% |
| `nested_vm_run_40` | 0.40 ms | 55% |
| `perform_block_300` | 1.14 ms | 50% |
| `perform_exact_pass_20k` | 1.59 ms | 12% |
| `perform_group_pass_20k` | 1.56 ms | 23% |
| `perform_mock_5k` | 2.32 ms | 7% |
| `proc_pause_resume_5k` | 2.49 ms | 13% |
| `proc_send_receive_20k` | 13.57 ms | 14% |
| `proc_spawn_500` | 1.62 ms | 9% |
| `proc_terminal_200x200` | 2.92 ms | 18% |
| `transfer_graph_20k` | 5.58 ms | 3% |
| `virtual_call_100k` | 7.95 ms | 14% |

The spread of several entries passes 30 percent, so a single pass of
this suite states little. Read a movement of one entry against its own
spread.

One entry moved past its spread when the shared heap ledger landed.
`mark_sweep_100k_under_256k` rose about 7 percent, and its spread is
12 percent, so the movement is near the edge of the measurement. The
sweep releases each object into the ledger, which is the same shape as
the charge on each allocation.

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

- Worklist items 11 and 12 belong to group C.
- Nested snapshots use lazy admission and independent load budgets.
- Interfaces, conformance, dispatch, and a guest `Type[T]` stay outside
  group A. Specification section 14 records what `ClosedType` leaves in
  place for them.

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
Container format 2 carries one. Its format table there still lists four
sections; format 2 holds five, and the heap and machine sections moved
to kinds 4 and 5.
