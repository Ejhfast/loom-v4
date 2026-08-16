# Post-week-9 admission status

This note records the supplementary correctness work that follows week
9. The worklist and the issue analysis live in `worklist.md`. The
normative design lives in `docs/specs/snapshot-image-admission.md`.

The work runs in three delivery groups. Group A establishes the
snapshot admission boundary. Group B establishes failure and resource
containment. Group C removes the known scaling defects. Each group
writes its own section below.

## Group A: the admission boundary

Group A covers worklist items 1 to 6. It answers issues 1, 2, and 8 of
`worklist.md`.

Group A ran in three rounds. The first round built the boundary. An
independent review found three blockers. The second round fixed them.
The third round added type environment witnesses, which closed the
last false rejections.

### The versions

The operation manifest ABI version moves to 4. The operation identity
now covers every field of one operation definition, and the manifest
rule makes a semantic field change an ABI change.

The snapshot container format version moves to 2. Version 2 states the
initialization of a local slot, and it carries the type table of
section 5.6. Version 1 spelled an uninitialized slot as a unit value.

The bytecode, interface, compiler ABI, and verifier versions hold. No
verifier rule changed, and the instruction set gained no field.

`core/pinned-core-defs.txt` moved, because every definition hash covers
the manifest digest. `core/pinned-hash.txt` holds, because the core
image bytes carry no manifest digest.

## What landed

### The two host states

`Image` is editable snapshot data. `SnapshotImage` is the admitted,
immutable form. The type system now records the admission fact:

- `codec::decode(bytes, limits)` proves container properties. It reads
  no program and returns `Image`.
- `admit(image, module, budget)` proves resolved structure and accurate
  live types. It returns `SnapshotImage`.
- `load_external(bytes, module, limits)` runs both stages and seals the
  bytes it received.
- `SnapshotImage` holds private fields. `into_image` is the one way
  back to editable data.
- `World::restore_image` accepts `&SnapshotImage`. The trusted image
  cache stores `SnapshotImage`.
- The trusted capture constructor stays inside the snapshot module.

`Origin` records the source of the bytes. No code reads `Origin` to
select a path or to grant trust.

### Complete type admission

Admission derives every type from verified code, resolved layouts, and
validated witnesses. It proves these positions:

- every local slot of every frame;
- every operand of every stopped frame;
- every argument of a pending perform;
- every instance field, under the arguments of the edge that named the
  object;
- every closure capture, through the function the closure names;
- every accepted mailbox message;
- every terminal result;
- every typed native value;
- every element of every collection those positions reach.

`lm_verify::ResolvedTypes` replaces `FrameTypes`. It exposes the
extended type universe as a read-only view, and it resolves a whole
frame chain at once. The previous reader mapped every substituted type
to `None`, and admission then skipped those slots. The new reader keeps
every substitution.

The dataflow of one function runs once for each admission. The previous
reader recomputed it for each saved frame.

Admission checks the unit value and the uninitialized marker against
their declared types. The previous rule accepted both at every type.

### Type environment witnesses

The verifier proves one generic body once, with the type variables of
that body opaque. One activation needs the type arguments its call site
applied.

A frame with a caller takes those arguments from the call instruction.
Three positions hold no call site:

- the bottom frame of a machine, when its entry function is generic;
- a closure that outlived the frame that created it;
- a machine past its constructor, because `enter_proc_body` takes the
  proc body closure and a terminal machine holds no frame.

Signature unification recovers no type variable that appears in a
capture list alone. The image therefore carries witnesses.

`crates/lm-bytecode/src/closed.rs` holds the closed type grammar, the
type environment table, and the interning rules. Every node carries a
content digest, so one closed type has one identity in every process.

A frame, a closure, an instance, and a machine each store one index.
Index zero names the empty environment. A monomorphic state stores
zero, allocates nothing, and performs no type work.

The table belongs to one world. Restore re-interns every record of the
image into the table of the target world, and it remaps every index. A
world caps its environment nodes and its closed type nodes, and it
returns a local fault at the cap.

A witness is data. Admission uses a witness where no derivation exists.
Admission checks the witness against the derivation everywhere else.

A witness never enters a guest digest, semantic equality, or the
semantic identity of a value. The two copy paths in
`crates/lm-heap/src/shape.rs` preserve it, so a closure keeps its
creator environment across a boundary.

### Object type coherence

One typed edge proves that edge. Two locals typed `List[Int]` and
`List[Str]` can name one empty mutable list. Each edge passed, because
the list held no element. Verified code then appended an integer
through the first local and read a string through the second local.

Admission now assigns each `(machine, object)` one exact closed type.
An instance edge normalizes through the concrete class of the object. A
closure edge normalizes through the declared type of its function. The
map keeps the most specific type that any edge names, so the order of
the walk does not change the answer.

Class arguments are invariant, so `List[Int]` and `List[Str]` have no
subtype relation and the aliased list rejects. A `Dog` instance reached
from an `Animal` edge admits.

### The operand partition

The first round required an exact operand count for the top frame
alone. Every lower frame took an inequality. An attacker inserted one
value at the base of the top frame and raised that base by one. The
inserted value then matched the type of the first call argument, so
every type rule passed. After the callee returned, the extra value
survived on the stack, and `ListAt` popped an integer where the
verifier proved an object. That reached a trusted assertion in the
interpreter.

Admission now derives the exact retained region of every frame. It
takes the proved stack depth at the point the frame stopped. It
subtracts every operand the suspended instruction consumed. A call
consumes its arguments. A virtual call consumes its receiver. A
`CallValue` consumes its closure. A perform consumes its arguments.

Admission also proves that the bottom frame starts at operand zero, so
the frames partition the arena exactly.

### The operation identity

`identity_of` encodes every field of one operation definition through
one common path. The snapshot classification is one of those fields.
The classification decides whether a pending instance holds live host
state, so a classification-only change is a behavior change.

The previous hash covered the discriminant and the schema text for a
machine control operation. It covered no parameter type and no reply
type. `Vm.SnapshotSelf` declares a reply the verifier reads, so that
reply moved no digest.

The manifest digest covers the identities. The verification hash covers
the manifest digest. A change to any field now invalidates every
verified-code cache entry and every admitted snapshot.

### The trusted interpreter boundary

The audit of the interpreter assertions found these uncovered paths:

- an image named an operation slot the manifest omits, through a call
  token, a fault value, or a stored terminal fault;
- an image held an instance of an abstract enum family;
- the graph copy asserted that no value holds the uninitialized marker;
- `verdict` indexed the root machine of an image with no machine;
- the dump named an operation slot with no bound.

Admission proves the first two. The graph copy returns a local fault.
The last two are total.

`World::restore_image` proves the admission identity in every build.
The previous rule used a debug assertion, so a release build performed
no check and the image named the slots of another module.

Admission rejects a policy table handle that names its own machine, and
a machine handle that names its own machine.

An instance holds the uninitialized marker only while it is the object
under construction. `New` allocates every field as the marker, and the
synthesized construction function holds the object in one local through
the defaults and the initializer. `E1029` forbids `self` from escaping
before the initializer assigns every required field. The rule therefore
reads: some frame of the machine names the object, and the function of
that frame allocates that class.

## The decisions and the rejected alternatives

### A generic frame resolves from its caller, and a witness answers the rest

The first candidate rejected every frame whose function takes a type
argument. A probe over the test suite counted 58 checks of a frame
inside a generic function. Every one of those frames sat above a caller
frame in the same image, and every caller stopped inside `CallG` or
`CallVirtualG`. The candidate therefore refused legal worlds.

The chain resolver reads the substitution from the call site. The
witness answers the three positions with no call site.

### The initialization fact uses the existing marker

Specification 5.3 allows a marker, a bitmap, or explicit slot state.
The container already carries `Uninit`, and an instance field already
uses it. The interpreter filled an unwritten local slot with the unit
value, so the image told the two apart in no way.

The virtual machine now fills such a slot with the marker. The wire
format gains no field. The format version still moves, because a
version-1 image carries another meaning.

### A slot with no proved type keeps its declared type

The verifier merges an initialized path with an uninitialized path into
"no value". The runtime slot can still hold the value the first path
stored, and no verified read reaches it before the next store. A rule
that demanded the marker in every such slot would refuse real captures.

Admission checks the value against the declared slot type. Every store
fits that type, so it bounds every value the slot ever held.

### The mailbox type of a machine with no proc class is `Never`

`sys.proc.run` moves a loaded machine to the scheduler and answers a
`Handle[Never, R]`. No proc class stands behind that machine.

A machine that `Proc.Spawn` did not launch accepts no message, so its
mailbox type is `Never`. The lowering spells `Never` as `Unit`, and the
verified type table starts with `Unit`.

The rule proves no value. `check_state` rejects a queued message on a
machine that is not a proc.

### `is_proc` names the machines a spawn launched

The capture derived `is_proc` from `owner == Scheduler || paused`.
Restore reads the flag and mints the birth grant of specification 18.3.
A forged image therefore took the whole `Proc` group.

`Machine::is_proc` is now a stored field. `Proc.Spawn` sets it where it
mints the grant. A machine that claims the flag must name a proc class
through its witness.

A machine from `sys.proc.run` now records `is_proc = false`, and a
restore grants it no group. That machine holds no mailbox, so the
previous grant gave it authority it never used.

### A faulted machine keeps its frames

A faulted machine never executes again, so its frames are diagnostic
state. Admission checks their structure. It requires no resumable
verifier state for them.

The rule that a terminal machine holds no frame now covers a `Done`
machine alone.

### An `Asked` machine holds no live attachment

`Asked` records a request before any host attachment starts, and the
holder answers it. `Waiting` holds the live attachment, and the capture
refuses that state with `ResourceActive`.

The previous rule rejected every pending request whose operation
suspends. It refused a machine stopped `Asked` on `Io.Print`,
`Io.Error`, `Io.ReadLine`, or `Clock.Sleep`. Two shipped examples
failed at every capture past their first perform.

### `Layout` and `Type` name two failures

`Layout` names a value that carries the wrong shape for a type
admission derived. `Type` names a failure of the type itself:
admission derives no type, or the derived type disagrees with its
target. The split keeps every corruption test resolving to its rule.

### The canonical-order rule runs last

Every earlier rule states a property of one position. An edit that
breaks a type usually drops an object out of the reachable set, so an
order-first pass reported the traversal. The order rule now runs after
the type walk, and each diagnostic names the position the edit broke.

## What the reviews found

An independent review of the first round found three blockers.

- **The operand partition.** A forged frame reached a host panic. The
  section above states the fix. The defect also existed before this
  work.
- **A handle to a finished proc.** `enter_proc_body` takes the proc
  body closure, so a proc past its constructor derived no mailbox type.
  Three shipped proc examples failed. The machine witness closed it.
- **A frame inside an overridden method.** The callee resolver read the
  static receiver type, so it found the statically visible method. A
  real frame runs the override. Admission now reads the concrete class
  of the receiver value, which sits in local slot 0 of the callee
  frame.

The review found four further defects at HIGH: a closure built inside a
generic body, a machine whose entry function is generic, the release
build of the restore identity check, and the operation identity fields.
Every one of them is closed above.

Two rules refused legal worlds before this work. Those are the faulted
frames and the `Asked` machine above. Both failed shipped examples, and
both reproduce against the tree before group A.

## The tests

`crates/lm-testkit/tests/admission.rs` holds 37 cases.
`crates/lm-testkit/tests/witness.rs` holds the witness rules.

Ten cases crafted the known type holes, and all ten admitted against
the tree before this work:

| Case | The hole it closes |
| --- | --- |
| `a_substituted_local_of_the_wrong_shape_rejects` | a substituted slot accepted every value |
| `a_substituted_operand_of_the_wrong_shape_rejects` | the same, on the operand stack |
| `a_unit_value_in_a_proved_local_rejects` | the unit value passed at every type |
| `an_uninitialized_marker_in_a_proved_local_rejects` | the marker passed at every type |
| `a_shared_object_checked_under_a_second_type_rejects` | the walk keyed on the object ordinal |
| `a_generic_instance_field_of_the_wrong_shape_rejects` | a field used its raw layout type |
| `a_machine_handle_that_names_another_result_type_rejects` | `Vm[T]` checked its outer tag |
| `an_empty_machine_handle_that_names_a_loaded_machine_rejects` | `EmptyVm` checked its outer tag |
| `a_terminal_unit_at_another_result_type_rejects` | a terminal unit skipped the result type |
| `a_terminal_uninitialized_marker_rejects` | a terminal marker skipped it as well |

### The positive control

`every_capture_of_every_shipped_program_admits` is the gate of the
acceptance direction. It walks `examples/` recursively and adds 25
crafted sources. It captures each program at every boundary of a
bounded prefix, and it passes the canonical bytes through the external
loader. Every capture must admit.

The gate holds the shapes this work unblocked: a proc handle past the
constructor, a closure a generic body built, a machine whose entry
function is generic, a `sys.proc.run` handle, a closure inside a
closure inside a generic body, a generic class with a generic field,
and polymorphic recursion captured while it runs.

The gate ran with an exclusion list during the second round. That list
named the false rejections the witness round closed, and it is empty.

### The other new cases

The rejection cases name the rules this work added: the exact operand
partition at every frame, the value below the bottom frame base, the
pending argument count, object coherence for a list and for a generic
instance field, the frame that is not the callee of its call site, the
forged proc flag, the self policy table, the self machine handle, the
uninitialized field outside construction, the restore of another
program, the budget of a wide container, and the type environment cap.

The witness cases state the three invariants: a witness enters no guest
digest, the two copy paths preserve a witness, and a witness that
disagrees with its derivation rejects.

Two identity cases state the week-7 open question closed. A
classification-only change moves the operation identity and the
manifest. It also moves the verification hash.

Test count: 783 before this work, 848 after.

## Measurements

`cargo test --workspace` runs 848 tests and exits 0.

### The state sizes

| Item | Before | After |
| --- | --- | --- |
| `Frame` | 32 B | 36 B |
| `Object`, closure and instance | 80 B | 80 B |
| heap `Entry` | 104 B | 104 B |
| `Machine` | 720 B | 736 B |

The `Map` variant fixes the size of `Object`, so the two witnesses cost
nothing there. `Object::cost()` holds, so no heap byte accounting
moved.

### The container sizes

| Shape | Before | After |
| --- | --- | --- |
| wide heap, 10k list elements | 90 440 B | 90 425 B |
| deep chain, 5k instances | 115 423 B | 125 415 B |
| machine world, three machines | 914 B | 843 B |

The deep chain grows 8.7 percent. It carries one witness ordinal for
each instance and for each closure. The machine world shrinks, because
the machine record drops a 32-byte result-type digest and gains two
small fields.

### The engine benchmarks

Debug figures swing about 20 percent between builds, so the release
column carries the signal.

| Entry | Release before | Release after |
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

Every entry except `proc_send_receive_20k` sits inside the noise of the
measurement. The open question below records that entry.

The performance targets hold. A monomorphic call copies index zero and
allocates nothing. A repeated generic call reuses one cached index. No
dynamic type check runs during execution. No value carries a resolved
type vector. Capture and admission each expand once under a cap.

## Open questions

### The cost of `proc_send_receive_20k`

That entry costs about 10 percent more in release and about 27 percent
more in debug. The cause is unknown.

These experiments explain none of it: the program performs no generic
call, so it reads and writes the environment table in no place; padding
experiments in the base tree reproduce none of the cost; and boxing the
world table removes none of it. Moving the three generic instructions
out of the inlined instruction body recovered about ten points of the
debug figure.

The entry is a constant factor on one workload. Worklist items 7, 11,
and 12 hold the scaling defects, so this entry waits.

### A core enum instance records the empty environment

The kernel builds `Option`, `Result`, `RunResult`, `StepEvent`,
`DriveEvent`, `Recv`, and `ProcResult` instances outside `New` and
`NewG`. Their class arguments follow from the operation manifest reply
types, and `lm-vm` reads no manifest typing, so the kernel records the
empty environment.

Admission accepts an empty witness at any class, and it rejects a
non-empty witness that disagrees with its edge. The instance witness is
evidence for no admission rule, so this weakens no proof. A later
reflection query on a core enum value would read its arguments from the
edge instead.

Two answers exist: carry the manifest reply types inside `lm-vm`, or
derive the witness at the perform.

### The nested container decodes on each admission

A `Snapshot[T]` value decodes its nested container during admission, to
read one header field. A world with many nested snapshots decodes each
one for each admission. The aggregate budget bounds the work. A cache
keyed by container hash would remove the repeat.

### The effect of a closed row names its module string slot

The container names an effect by its module string slot inside a closed
row, as a literal names its pooled string. The digest names it by text.
A content-addressed wire form is possible.

### The type environment cap reuses one fault code

`FaultCode::BoundaryLimit` carries the type environment cap. A
dedicated code would move the fault table for one internal cap.

### `Vm[T]` accepts a subtype result

A `Vm[T]` reads the terminal value of its target, so admission accepts
a target whose result type is a subtype. A `Handle[M,R]` sends and
receives its message type, so its mailbox type must match exactly.
Section 5.4 of the sidecar specification records the reasoning.

## Deferred work

- Worklist items 7 to 12 stay for groups B and C.
- `AdmissionBudget` carries a conservative default limit and a
  container byte limit. Item 10 sizes it beside a `DecodeBudget`,
  shares one ledger with nested containers, and adds the compact-input
  expansion tests.
- The decode stage still uses `LoadLimits` and per-list caps. Item 10
  replaces them with one aggregate ledger and fallible reservations.
- Admission builds one `ResolvedTypes` for each call, so a repeated
  admission of one module repeats the structural verifier pass. A
  per-module cache belongs with the verified-code cache.
- Interfaces, conformance, dispatch, and a `Type[T]` guest surface stay
  outside this work. Section 14 of the sidecar specification records
  what the closed type table leaves in place for them.

## Maintenance

`checkpoints/asked-tree.lms` and `tests/fuzz-regressions/*.lms` match
`*.lms` in `.gitignore`, so no commit carries them. A fresh checkout
regenerates both:

```sh
nix-shell --run "cargo run -p lm-cli -- snapshot save --allow Proc,Vm,Clock \
  checkpoints/asked-tree.lm checkpoints/asked-tree.lms"
nix-shell --run "cargo test -p lm-testkit --test fuzz regenerate_fuzz_corpus -- --ignored"
```

`docs/notes/week9.md` calls `checkpoints/asked-tree.lms` a checked-in
container. That statement is wrong, and this note corrects it.

## Specification edits this work needs

`docs/notes/week9.md` states that the container carries no type table.
The container now carries one. The format table there needs the fifth
section and the renumbered heap and machine sections. The header
result-type field is now a closed-type content digest.

`docs/specs/build-order.md` week 9 must use `SnapshotImage` for the
admitted host state and `Image` for the editable decoded state.
Specification section 12 of the sidecar asks for both edits.
