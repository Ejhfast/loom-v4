# Post-week-9 admission status

This note records the correctness work after week 9. `worklist.md`
holds the worklist and the issue analysis.
`docs/specs/snapshot-image-admission.md` holds the normative design.

The work has three delivery groups:

- group A separates decoding from admission, and proves live types;
- group B contains failures and resources;
- group C removes the known scaling defects.

## Group A

Group A covers worklist items 1 to 6, and answers `worklist.md` issues
1, 2, and 8.

Group A landed in three parts. The first part separated decoding from
admission. An independent review then found three serious defects. The
second part fixed those defects. The third part added type environment
witnesses.

### Versions

`lm_abi::ABI_VERSION` moves from 3 to 4. `identity_of` now hashes every
field of an `OpDef`, and a change to any operation field is an ABI
change.

`lm_vm::snapshot::FORMAT_VERSION` moves from 1 to 2. A version-2
container marks an uninitialized local slot, and carries a closed type
table.

The bytecode format version, the interface format version, the compiler
ABI version, and the verifier version keep their week-9 values.

`core/pinned-core-defs.txt` changed, because every definition hash
covers `manifest_digest()`. `core/pinned-hash.txt` keeps its value,
because the core image encoding covers source content alone.

## What landed

### Decoding and admission are separate calls

`codec::decode(bytes, limits)` reads the bytes and the limits, and
returns `Image`. `Image` has public fields and permits any edit.

`admit(image, module, budget)` proves resolved structure and accurate
live types, and returns `SnapshotImage`. `SnapshotImage` has private
fields.

`load_external(bytes, module, limits)` calls both, then stores the
canonical bytes beside the admitted `Image`.

`World::restore_image` takes `&SnapshotImage`. The trusted image cache
in `World` stores `SnapshotImage`. `codec::from_trusted_capture` builds
a `SnapshotImage` for `World::capture_snapshot`, and stays private to
`crates/lm-vm/src/snapshot/`.

`SnapshotImage::into_image` returns an editable `Image` copy. That copy
needs `admit` again before a restore.

`Origin` records whether trusted capture or `load_external` produced
the bytes. `lm inspect` and the diagnostics read `Origin`. Both origins
give the same guarantees.

### Admission proves every typed position

`admit` derives each expected type from verified code, from a resolved
class or function layout, or from a validated witness. It proves:

- every local slot of every frame;
- every operand of every stopped frame;
- every argument of a pending perform;
- every instance field;
- every closure capture;
- every message in a mailbox queue;
- every stored terminal result;
- every native value that carries a type parameter;
- every collection element those positions reach.

`lm_verify::ResolvedTypes` replaces `lm_verify::FrameTypes`.
`FrameTypes::operands_at` returned `None` for a type the verifier built
by substitution, and `check_types` then skipped that slot.
`ResolvedTypes` returns every substituted type.

`ResolvedTypes` runs the dataflow of one function once for each
`admit` call. `FrameTypes` recomputed that dataflow for each saved
frame.

`admit` proves `Value::Unit` and `Value::Uninit` against the declared
type of their slot. `check_shape` accepted both values at every type.

### Type environment witnesses

The verifier proves a generic function body once, with its type
variables opaque. Admission needs the type arguments that the call site
applied.

`ResolvedTypes::resolve_chain` reads those arguments from the call
instruction of the frame below. Three positions have no frame below,
or lose the frame that held the arguments:

- the bottom frame of a machine, when its entry function is generic;
- a closure that outlived the frame that built it;
- a machine past its constructor. `World::enter_proc_body` calls
  `take()` on `Machine::start_body`, and a terminal machine drops every
  frame.

A capture list can name a type variable that the closure signature
omits, so unification of the signature against the position type fails
at that position.

`crates/lm-bytecode/src/closed.rs` holds `ClosedType`, `TypeEnv`, and
`TypeEnvs`. Each `ClosedType` node has a content digest, so one closed
type has one identity in every process.

`Frame`, `Object::Closure`, `Object::Instance`, and `Machine` each
store one `TypeEnvId`. `TypeEnvId(0)` is the empty environment. A
monomorphic call copies `TypeEnvId(0)` and allocates nothing.

`TypeEnvs` belongs to one `World`. `restore_image` re-interns each
record of the image into the `TypeEnvs` of the target world, and
rewrites each stored `TypeEnvId`. `TypeEnvs::new` takes a node cap and
an environment cap, and `intern` returns `TypeEnvFull` at either cap.

`admit` reads a witness at the three positions above. `admit` compares
the witness against the derived value everywhere else, and rejects a
disagreement.

`lm_graph::digest` and `deep_equal` skip the `TypeEnvId` of an object.
`Object::shell` and `Object::remap` in `crates/lm-heap/src/shape.rs`
copy it, so a closure that crosses a machine boundary keeps its
environment.

### One object has one exact type

`Image` can hold one empty `List` object, one local declared
`List[Int]`, and one local declared `List[Str]`, both naming that
object. Each declared type passed alone, because the list was empty.
Verified code then pushed an `Int` through the first local, and read a
`Str` through the second local.

`check_coherence` now maps each `(machine, object)` pair to one exact
closed type. An instance position resolves through the concrete class
of the object. A closure position resolves through the declared type of
its function.

`check_coherence` stores the most specific type that any position
names, so every traversal order gives the same result.

`docs/specs/language-spec.md` section 6 makes class arguments
invariant, so `is_subtype` relates `List[Int]` and `List[Str]` in
neither direction, and the shared empty list rejects. One `Dog`
instance named by a `Dog` local and by an `Animal` local admits.

### Frames partition the operand arena exactly

`check_operands` required an exact operand count for the top frame,
and an inequality for every lower frame.

An attacker inserted one `Int` at `frames[top].base_operand`, and
raised that base by one. The `Int` matched the declared type of the
first call argument, so every type rule passed. The `Int` stayed on the
stack after the callee returned. `ListAt` then popped it where the
verifier proved an object, and reached
`unreachable!("verified operand type")` in `Machine::pop_obj`.

`check_operands` now computes the exact retained region of each frame.
It reads the proved stack depth at the stop point, then subtracts the
operands that the suspended instruction consumed:

- `Call` and `CallG` consume their arguments;
- `CallVirtual` and `CallVirtualG` also consume the receiver;
- `CallValue` also consumes the closure;
- `Perform` consumes its arguments.

`check_state` also requires `frames[0].base_operand == 0`.

### The operation identity covers every field

`identity_of` hashes each `OpDef` field through one common path,
including `OpDef.snapshot`. `OpDef.snapshot` decides whether a pending
instance holds live host state, so a change to it changes behavior.

`op_identity` hashed two items for an `OpKind::VmControl` operation:
the kind tag and `OpDef.schema`. A change to `OpDef.reply` of
`Vm.SnapshotSelf` kept the same hash. `ResolvedTypes::pending_call_types`
and the verifier rule for `Instr::AsCall` both read `OpDef.reply`.

`manifest_digest()` covers each operation identity, and
`verification_hash` covers `manifest_digest()`. A change to any `OpDef`
field now invalidates each verified-code cache entry and each admitted
snapshot.

### The interpreter assertions

The audit found five reachable assertions:

- an image named an operation slot past `OP_COUNT`, through a call
  token, a fault value, or a stored terminal fault;
- an image held an `Object::Instance` of an abstract enum family;
- `lm_graph::mode::copy_value` asserted that a value carries a payload,
  and a local slot can hold `Value::Uninit`;
- `verdict` indexed `image.machines[0]` for an image with zero
  machines;
- `dump` indexed the operation manifest with a stored slot.

`admit` rejects the first two images. `copy_value` returns
`FaultCode::BoundaryViolation`. `verdict` and `dump` handle every
input.

`restore_image` compares `SnapshotImage::identity().module_semantic`
against the running program in every build. That comparison was a
`debug_assert_eq!`, so a release build restored an image admitted
against another program.

`admit` rejects an `Object::NativeTable` that names its own machine. It
rejects an `Object::NativeVm` that names its own machine.

An `Object::Instance` holds `Value::Uninit` during construction alone.
`Instr::New` fills each field with `Value::Uninit`. The synthesized
construction function in `crates/lm-hir/src/lower.rs` holds the object
in one local through the field defaults and the initializer. `E1029`
requires the initializer to assign each required field before `self`
escapes. `admit` therefore requires one frame of the machine to name
the object in a local or an operand, and requires the function of that
frame to allocate that class.

## Decisions

### A generic frame reads its arguments from its caller

The first candidate rejected each frame whose function takes a type
argument. A probe over the test suite counted 58 such frames. Each one
sat above a caller frame in the same image, and each caller stopped
inside `CallG` or `CallVirtualG`. The candidate refused legal worlds.

`resolve_chain` reads the arguments from the call instruction. The
witness answers the three positions that have no call instruction.

### The uninitialized marker states the initialization fact

Specification 5.3 permits a marker, a bitmap, or explicit slot state.
`Value::Uninit` already exists, and `Instr::New` already writes it.

`Machine::load_frame` and `Machine::push_frame` filled a slot past the
parameters with `Value::Unit`, so an image spelled an empty slot and a
real unit value the same way. Both now write `Value::Uninit`.

The wire format keeps its field list. `FORMAT_VERSION` still moves,
because a version-1 container gives `Value::Unit` the other meaning.

### A slot the verifier leaves unproved keeps its declared type

The verifier merges an initialized path and an uninitialized path into
one unknown state. The runtime slot can still hold the value that the
first path stored. A rule that required `Value::Uninit` in each such
slot would reject real captures.

`admit` checks that value against the declared local type. Each store
fits that type, so the declared type bounds each value the slot held.

### A machine outside the proc set has the mailbox type `Never`

`sys.proc.run(vm)` moves a loaded machine to the scheduler, and returns
`Handle[Never, R]`. A proc class stands behind a spawned proc alone.

`mailbox_type` derives the message type from the proc class of a
spawned machine. It returns `BcType::Unit` for every other machine.
`crates/lm-hir/src/lower.rs` lowers `Never` to `BcType::Unit`, and
`verify_structure` requires the type table to start with `BcType::Unit`.

`check_state` rejects a queued message on a machine outside the proc
set, so a proc class governs each stored message.

### `is_proc` names the machines that `Proc.Spawn` launched

`machine_record` in `crates/lm-vm/src/snapshot/write.rs` derived
`is_proc` from `owner == Ownership::Scheduler || paused`.
`restore_state` reads `is_proc` and grants the `Proc` group of
specification 18.3, so an edited image took that group.

`Machine::is_proc` is now a stored field, and `World::proc_spawn` sets
it beside the grant. `check_machine_witness` requires a machine with
`is_proc` to name a proc class through its witness.

A machine from `sys.proc.run` now stores `is_proc = false`, and a
restore gives it the fresh default-deny table alone. Such a machine has
the mailbox type `Never`, so the earlier grant gave it unused
authority.

### A faulted machine keeps its frames

A faulted machine stops for good, so its frames are diagnostic state.
`admit` checks the structure of those frames, and requires a resumable
verifier state for a live machine alone.

The rule that a terminal machine drops each frame now covers a
`ImageState::Done` machine alone.

### `Asked` holds a request that the holder answers

`ImageState::Asked` records a request before the host attachment
starts. `MachineState::Waiting` holds the live attachment, and
`World::snapshot_preflight` refuses `Waiting` with
`SnapshotFail::ResourceActive`.

`check_state` rejected each pending request whose operation has
`suspends() == true`, so it refused a machine stopped `Asked` on
`Io.Print`, `Io.Error`, `Io.ReadLine`, or `Clock.Sleep`.
`examples/04-effects/blocked.lm` and
`examples/04-effects/manual-drive.lm` failed at every capture past
their first perform.

### `ImageReason::Layout` and `ImageReason::Type`

`ImageReason::Layout` names a value whose shape disagrees with a type
that `admit` derived. `ImageReason::Type` names a failure of the type
itself. The evidence for the type is missing, or the derived type
disagrees with its target.

### `check_order` runs after the type rules

Each type rule states a property of one position. An edit that breaks a
type usually drops an object out of the reachable set, so `check_order`
reported the traversal first. `check_order` now runs last, and each
diagnostic names the position the edit broke.

## What the reviews found

The independent review of the first part found three serious defects:

- **The operand partition.** A forged frame reached
  `unreachable!("verified operand type")`. The defect predates group A.
- **A handle to a finished proc.** `enter_proc_body` calls `take()` on
  `start_body`, so `mailbox_type` failed for a proc past its
  constructor. `examples/07-procs/worker.lm`,
  `examples/07-procs/mailbox-handle.lm`, and
  `examples/07-procs/barrier.lm` failed. The machine witness fixed it.
- **A frame inside an overridden method.** `call_substitution` called
  `find_method` with the static receiver class, so it found the
  statically visible method. A real frame runs the override.
  `ResolvedTypes` now reads the concrete class of the receiver value,
  which sits in local slot 0 of the callee frame.

The review found four further defects: a closure built inside a generic
body, a machine whose entry function is generic, the `debug_assert_eq!`
in `restore_image`, and the `OpDef` fields that `op_identity` omitted.
The sections above fix all four.

Two rules refused legal worlds before group A. Those rules are the
faulted frames and the `Asked` machine above. Both reproduce against
the tree at commit `c87f9de`.

## Tests

`crates/lm-testkit/tests/admission.rs` holds 37 cases.
`crates/lm-testkit/tests/witness.rs` holds the witness cases.

Ten cases build a forged image for a known type hole. All ten admitted
against the tree at commit `c87f9de`:

| Case | The hole it closes |
| --- | --- |
| `a_substituted_local_of_the_wrong_shape_rejects` | a substituted slot accepted every value |
| `a_substituted_operand_of_the_wrong_shape_rejects` | the same hole, on the operand stack |
| `a_unit_value_in_a_proved_local_rejects` | `Value::Unit` passed at every type |
| `an_uninitialized_marker_in_a_proved_local_rejects` | `Value::Uninit` passed at every type |
| `a_shared_object_checked_under_a_second_type_rejects` | the traversal keyed on the object ordinal |
| `a_generic_instance_field_of_the_wrong_shape_rejects` | a field used its raw layout type |
| `a_machine_handle_that_names_another_result_type_rejects` | `Vm[T]` checked its object tag |
| `an_empty_machine_handle_that_names_a_loaded_machine_rejects` | `EmptyVm` checked its object tag |
| `a_terminal_unit_at_another_result_type_rejects` | a terminal `Unit` skipped the result type |
| `a_terminal_uninitialized_marker_rejects` | a terminal marker skipped the result type |

### The acceptance test

`every_capture_of_every_shipped_program_admits` compiles each `.lm`
file under `examples/` and 25 crafted sources. It captures each program
at each bytecode boundary of a bounded prefix, then passes the
canonical bytes through `load_external`. Each capture must admit.

That test listed six known failures while the witness work was open.
Type environment witnesses removed every false rejection in that list.
The test now admits each capture of each program, with no exclusion.

The list held these shapes:

- a proc handle past the constructor;
- a closure built inside a generic body;
- a machine whose entry function is generic;
- a `sys.proc.run` handle;
- a closure inside a closure inside a generic body;
- a generic class with a generic field;
- polymorphic recursion, captured while it runs.

### The rejection cases

The other cases state the rules group A added: the exact operand region
of each frame, a value below `frames[0].base_operand`, the pending
argument count, one object under two element types, one generic
instance under two class arguments, a frame that is not the callee of
its call site, a forged `is_proc`, a self policy table, a self machine
handle, `Value::Uninit` outside construction, a restore against another
program, `AdmissionBudget` exhaustion, and `TypeEnvs` exhaustion.

`crates/lm-testkit/tests/witness.rs` states three invariants: `digest`
skips a `TypeEnvId`, `shell` and `remap` copy a `TypeEnvId`, and a
witness that disagrees with its derived value rejects.

Two cases close the week-7 open question. A change to `OpDef.snapshot`
moves `identity_of`, `manifest_digest()`, and `verification_hash`.

Test count: 783 at commit `c87f9de`, 848 now.

## Measurements

`cargo test --workspace` runs 848 tests and exits 0.

### Sizes

| Item | `c87f9de` | Now |
| --- | --- | --- |
| `Frame` | 32 B | 36 B |
| `Object`, closure and instance | 80 B | 80 B |
| heap `Entry` | 104 B | 104 B |
| `Machine` | 720 B | 736 B |

`Object::Map` fixes the size of `Object`, so the two `TypeEnvId` fields
fit inside the existing padding. `Object::cost()` keeps its values, so
the heap byte accounting keeps its values.

### Container sizes

| Shape | `c87f9de` | Now |
| --- | --- | --- |
| wide heap, 10k list elements | 90 440 B | 90 425 B |
| deep chain, 5k instances | 115 423 B | 125 415 B |
| machine world, three machines | 914 B | 843 B |

The deep chain grows 8.7 percent, and carries one witness ordinal for
each instance and each closure. The machine world shrinks, because its
machine record drops a 32-byte result-type digest and gains two small
fields.

### Benchmarks

Debug figures vary about 20 percent between builds, so this table holds
the release figures.

| Entry | `c87f9de` | Now |
| --- | --- | --- |
| `alloc_gc_100k` | 8.32 ms | 7.66 ms |
| `freeze_chain_50k` | 6.54 ms | 6.87 ms |
| `transfer_graph_20k` | 5.12 ms | 5.00 ms |
| `digest_graph_20k_plus_1k_cached` | 4.05 ms | 3.87 ms |
| `mark_sweep_100k_under_256k` | 8.81 ms | 9.34 ms |
| `proc_send_receive_20k` | 8.03 ms | 8.86 ms |
| `proc_spawn_500` | 2.31 ms | 2.47 ms |
| `proc_pause_resume_5k` | 1.75 ms | 1.84 ms |
| `proc_terminal_200x200` | 3.42 ms | 3.41 ms |

Eight entries vary inside the measurement noise. The open question
below records `proc_send_receive_20k`.

The performance targets hold. A monomorphic call copies `TypeEnvId(0)`
and allocates nothing. A repeated generic call reuses one cached
`TypeEnvId`. Execution reads static types alone. A `Value` carries its
payload alone. Capture and `admit` each expand the tables once, under a
cap.

## Open questions

### The cost of `proc_send_receive_20k`

That benchmark costs about 10 percent more in release, and about 27
percent more in debug. The cause stays unknown.

Three experiments failed to find the cause. The benchmark program runs
monomorphic code, so it reads and writes `TypeEnvs` at no point.
Padding experiments at commit `c87f9de` reproduce a flat cost. A boxed
`TypeEnvs` keeps the same cost.

One change helped. Moving `CallG`, `CallVirtualG`, and `NewG` out of
the inlined instruction body recovered about ten points of the debug
figure.

The benchmark shows a constant factor on one workload. Worklist items
7, 11, and 12 hold the scaling defects, so this benchmark waits.

### A core enum instance stores `TypeEnvId(0)`

`World::build_host_value` builds `Option`, `Result`, `RunResult`,
`StepEvent`, `DriveEvent`, `Recv`, and `ProcResult` instances outside
`Instr::New`. Their class arguments follow from the `OpDef.reply` types,
and `lm-vm` reads the manifest as data alone, so those instances store
`TypeEnvId(0)`.

`admit` accepts `TypeEnvId(0)` at any class, and rejects a non-empty
witness that disagrees with its position. The instance witness serves
reflection, so each admission proof holds without it. A later
reflection query on a core enum value reads its arguments from the
position instead.

Two answers exist. The first carries the `OpDef.reply` types inside
`lm-vm`. The second derives the witness at the perform.

### A nested container decodes once for each `admit` call

A `Snapshot[T]` value decodes its nested container during `admit`, to
read the declared root type from its header. A world with many nested
snapshots decodes each one for each `admit` call. `AdmissionBudget`
bounds that work. A cache keyed by container hash would remove the
repeat.

### A closed row names an effect by module string slot

The container names an effect by its module string slot inside a
`ClosedRow`, as a literal names its pooled string. `ClosedType::digest`
names that effect by text. A content-addressed wire form is possible.

### `TypeEnvs` exhaustion reuses `FaultCode::BoundaryLimit`

A dedicated fault code would move the fault table for one internal cap.

### `Vm[T]` accepts a subtype result

A `Vm[T]` reads the terminal value of its target, so `admit` accepts a
target whose result type is a subtype. A `Handle[M,R]` sends and
receives its message type, so `admit` requires an exact mailbox type.
Specification section 5.4 records the reasoning.

## Deferred work

- Worklist items 7 to 12 belong to groups B and C.
- `AdmissionBudget` carries a default limit of `1 << 24` units and a
  container byte limit. Worklist item 10 sizes it beside a
  `DecodeBudget`, shares one ledger with nested containers, and adds
  the compact-input expansion tests.
- `decode` uses `LoadLimits` and per-list caps. Worklist item 10
  replaces them with one aggregate ledger and fallible reservations.
- `admit` builds one `ResolvedTypes` for each call, so a repeated
  admission of one module repeats `verify_structure`. A per-module
  cache belongs beside the verified-code cache.
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

## Specification edits this work needs

`docs/notes/week9.md` states that the container omits a type table. The
container carries one now. Its format table needs the fifth section,
and needs the renumbered heap and machine sections. Its header
result-type field is now a `ClosedType` content digest.

`docs/specs/build-order.md` week 9 must name `SnapshotImage` as the
admitted host state, and `Image` as the editable decoded state.
Specification section 12 asks for both edits.
