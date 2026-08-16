# Post-week-9 admission status

This note records the supplementary correctness work that follows week
9. The worklist and the issue analysis live in `worklist.md`, and the
normative design lives in
`docs/specs/snapshot-image-admission.md`.

The work runs in three delivery groups. Group A establishes the
snapshot admission boundary, group B establishes failure and resource
containment, and group C removes the known scaling defects. Each group
writes its own section below.

## Group A: the admission boundary

Group A covers worklist items 1 to 6. It answers issues 1, 2, and 8 of
`worklist.md`.

The operation manifest ABI version moves to 4: the operation identity
now covers the snapshot classification, and the manifest rule makes a
semantic field change an ABI change. The snapshot container format
version moves to 2: a local slot with no value now spells the
uninitialized marker, and version 1 spelled it as a unit value. The
bytecode, interface, compiler ABI, and verifier versions do not move,
because no verifier rule and no container layout changed.

### What landed

#### The two host states

`Image` is editable snapshot data, and `SnapshotImage` is its admitted,
immutable form. The type system now records the admission fact:

- `codec::decode(bytes, limits)` proves container properties alone and
  returns `Image`. It reads no program.
- `admit(image, module, budget)` proves resolved structure and accurate
  live types, and returns `SnapshotImage`.
- `load_external(bytes, module, limits)` composes the two and seals the
  bytes it received.
- `SnapshotImage` has private fields, `into_image` is the one way back
  to editable data, and the trusted capture constructor stays inside
  the snapshot module.
- `World::restore_image` accepts `&SnapshotImage`, and the trusted
  image cache stores `SnapshotImage`.

The origin of an admitted image is provenance alone. `Origin` records
whether a consistent cut or the external loader produced the bytes, and
no code reads it to select a path.

#### The resolved-type view

`lm_verify::ResolvedTypes` replaces `FrameTypes`. It exposes the
extended type universe read-only, and it resolves a whole frame chain
at once. The verifier proves a generic body once with its type
variables opaque, so one activation needs the substitution its call
site applied:

- the bottom frame of a machine carries no call site, so its function
  must take no type argument;
- every other frame takes its substitution from the call instruction
  the frame below stopped inside: `Call`, `CallG`, `CallVirtual`,
  `CallVirtualG`, or `CallValue`;
- the frame must name the callee of that call site, which is a new
  structural rule;
- a slot type that still holds a variable after substitution has no
  decidable value set, and the resolver rejects the chain.

The dataflow of one function now runs once per admission and serves
every saved frame of that function. The previous reader recomputed it
for each frame.

#### Complete type admission

Admission derives every type from verified code and resolved layouts:

- every local slot of every frame. A slot the verifier proves
  initialized carries its proved type. A slot it proves uninitialized
  carries the marker, or a value one path of a merge left behind; the
  declared slot type bounds that value;
- every operand of every stopped frame, and every argument of a pending
  perform, from the verifier state at the saved point;
- every instance field, under the type arguments of the position that
  named the object;
- every closure capture, through the function the closure names;
- every accepted mailbox message, every terminal result, and every
  reachable collection element.

The unit value and the uninitialized marker are values, not wildcards.
The graph walk visits `(machine, object, resolved type)` triples, so
one shared object is proved under every type that reaches it. The walk
is iterative and bounded, and it charges one aggregate
`AdmissionBudget`.

#### Native relational admission

A native value that names another machine or record takes its type from
that target, in a first pass that runs before the graph walk:

- `Vm[T]` matches the declared result type of the target machine;
- `EmptyVm` names a machine with no loaded program;
- `Handle[M,R]` matches the mailbox type and the result type of the
  target proc;
- `PendingCall[A,R]` matches the argument view and the reply type of
  the operation it names;
- `Snapshot[T]` matches the declared root result type of its nested
  container;
- a nested snapshot stays opaque. Admission decodes its container and
  reads its header. The nested body passes full admission at its own
  restore.

A target with no derivable type rejects. Admission never reads a
relational type from the image.

#### The operation identity

`op_identity` now covers every semantic field of one operation
definition, including the snapshot classification. That field decides
whether a pending instance holds live host state, so a
classification-only change is a behavior change. The manifest digest
covers the identities, and the verification hash covers the manifest
digest, so such a change now invalidates every verified-code cache
entry and every admitted snapshot.

`identity_of`, `manifest_digest_of`, and `verification_hash_with` take
their inputs, so a test states the movement without a second manifest.

#### The trusted interpreter boundary

The audit of the interpreter assertions found four uncovered paths:

- an image could name an operation slot the manifest has not, through a
  call token, a fault value, or a stored terminal fault. Admission now
  proves every one of them, and the encoder reports an unknown slot
  instead of indexing the manifest out of range;
- an image could hold an instance of an abstract enum family. No
  verified program allocates one. Admission rejects it;
- the graph copy asserted that no value is the uninitialized marker. A
  local slot may now hold the marker, so the copy returns a local fault
  instead;
- `verdict` indexed the root machine of an image with no machine, and
  the dump named an operation slot without a bound. Both are total now.

Every other assertion stays. `Machine::pop` states the two facts that
carry them: the verifier proves the type at each program point, and
admission proves that a restored frame carries exactly those types.

### The decisions and the rejected alternatives

#### A generic frame resolves from its caller, and does not reject

The first candidate rejected every frame whose function takes a type
argument. It is sound and simple, and the corpus disproves it. A probe
over the whole test suite counted 58 checks of a frame inside a generic
function: 23 inside `choose`, 16 inside `Option.value_or`, and 19
inside the generated `Option` constructors. Every one of those frames
sat above a caller frame in the same image, and every caller stopped
inside `CallG` or `CallVirtualG`.

The chain resolver reads the substitution from that call site. It
rejects only where no call site supplies one: a generic bottom frame, a
closure call with a generic body, and a capture list that still holds a
variable. No capture of the corpus reaches those cases.

#### The initialization fact uses the existing marker

Specification 5.3 allows a marker, a bitmap, or explicit slot state.
The container already carries `Uninit`, and an instance field already
uses it. The interpreter filled an unwritten local slot with the unit
value instead, so the image could not tell the two apart.

The VM now fills such a slot with the marker. The wire format gains no
field, and the format version still moves, because the meaning of a
version-1 image differs. The alternative, a per-frame initialization
bitmap, adds a field that the marker already carries.

#### A slot with no proved type keeps its declared type

The verifier merges an initialized path with an uninitialized path into
"no value". The runtime slot can still hold the value the first path
stored, and no verified read reaches it before the next store. A rule
that demanded the marker in every such slot would reject real captures.

Admission therefore checks the value against the declared slot type,
which bounds every value the slot ever held, because every store fits
it. The marker is legal there as well.

#### The result type of a machine comes from its body function

A proc runs its constructor first, so the recorded result-type digest
names the proc instance type during construction. `Proc.Spawn` types a
handle from the body closure, so the two disagree.

The relational rules read the body function of a machine: the proc body
closure when the machine keeps one, and the recorded digest otherwise.
The terminal rule keeps the recorded digest, because a terminal machine
holds the digest of the type its stored value carries.

#### The canonical-order rule runs last

Every earlier rule states a property of one position. An edit that
breaks a type usually drops an object out of the reachable set as well,
so an order-first pass reported the traversal instead of the value. The
order rule now runs after the type walk, and each diagnostic names the
position the edit broke.

#### `Layout` and `Type` name two halves of one failure

`Layout` names a value that carries the wrong shape for a type
admission derived. `Type` names the other half: admission cannot derive
the type, or a derived type does not match its target. The split keeps
every corruption test resolving to the rule it broke.

#### Decoding keeps its ordinal range checks

Specification section 4 permits a decoder to store a reference as data.
The decoder still range-checks the ordinals it can check from the
container itself, because the check costs one comparison and it bounds
the decoded data. Admission repeats every one of those checks, because
an editor can break them with no container behind it.

### Simplifications

- One admission pass replaced `check_machine`, `check_types`,
  `check_operands`, `check_pending_args`, `mailbox_type`,
  `check_shape`, `extends`, `check_parent_forest`, `check_world`, and
  `check_order` inside the decoder. The decoder lost its dependence on
  the loaded module and now takes bytes and limits alone.
- `FrameTypes::operands_at` returned a fresh vector per call and
  recomputed the function dataflow each time. `resolve_chain` computes
  the chain once for the whole machine.
- The type walk replaced two separate passes: one over the typed roots
  and one over every instance and closure of the heap. Every object is
  reachable from a root, so the walk covers the same set through the
  types that reach it.
- `dump` split into `dump` for an admitted image and `dump_image` for
  editable data, so the container header stays with the container.

### Changed tests

- `crates/lm-testkit/tests/week9_image.rs`: `reject` and `accept` now
  run decoding and admission, so a case states one rule whichever
  stage owns it. Three cases changed the rule they name:
  - the zero-machine header now leaves the heap section outside the
    sections the header names, so decoding reports `Trailing`. The
    zero-machine world is an admission rule, and `admission.rs` states
    it;
  - the root machine ordinal is a canonical-encoding rule, so decoding
    reports `SectionBounds`;
  - `a_reordered_heap_rejects_as_non_canonical` became
    `a_swapped_capture_context_rejects`. The swap it performs breaks
    the new capture-context rule first, and `admission.rs` states the
    canonical-order rule with a rotation that breaks nothing else.
- `crates/lm-testkit/tests/week9_snapshot.rs`: restore takes an
  admitted image, and the origin replaces `externally_checked`.
- `crates/lm-testkit/tests/fuzz.rs`: the resealing snapshot fuzzer
  decodes, admits, and restores. It counts an admitted mutant, not a
  decoded one.
- `crates/lm-testkit/tests/week7_graph.rs`: the checked digest of
  `examples/06-graphs/cycle-digest.lm` moved with the manifest ABI
  version.

### New tests

`crates/lm-testkit/tests/admission.rs` holds 19 cases. Ten of them
crafted the known holes and failed against the tree before this work:

| case | the hole it closes |
| --- | --- |
| `a_substituted_local_of_the_wrong_shape_rejects` | a substituted slot accepted every value |
| `a_substituted_operand_of_the_wrong_shape_rejects` | the same, on the operand stack |
| `a_unit_value_in_a_proved_local_rejects` | the unit value passed at every type |
| `an_uninitialized_marker_in_a_proved_local_rejects` | the marker passed at every type |
| `a_shared_object_checked_under_a_second_type_rejects` | the walk keyed on the object ordinal alone |
| `a_generic_instance_field_of_the_wrong_shape_rejects` | a field used its raw layout type |
| `a_machine_handle_that_names_another_result_type_rejects` | `Vm[T]` checked its outer tag alone |
| `an_empty_machine_handle_that_names_a_loaded_machine_rejects` | `EmptyVm` checked its outer tag alone |
| `a_terminal_unit_at_another_result_type_rejects` | a terminal unit bypassed the result type |
| `a_terminal_uninitialized_marker_rejects` | a terminal marker bypassed it as well |

The other nine cases state the rules this work added:
`a_nested_snapshot_of_another_root_type_rejects`,
`a_world_with_no_machine_rejects`,
`a_rotated_heap_rejects_as_non_canonical`,
`an_admission_budget_that_runs_out_rejects`,
`an_operation_slot_past_the_manifest_rejects`,
`an_instance_of_an_abstract_class_rejects`,
`a_container_of_an_older_build_rejects`,
`an_admitted_image_records_its_admission_identity`, and
`every_captured_world_of_the_corpus_admits`.

The last case admits every capture of the whole corpus, so the negative
cases state a rule and not a broken helper.

Two identity cases state the week-7 open question closed:
`a_classification_only_change_moves_the_identity_and_the_manifest` in
`lm-abi`, and
`a_snapshot_classification_change_moves_the_verification_hash` in
`crates/lm-testkit/tests/identity_linking.rs`.

### Measurements

- `cargo test --workspace` runs 804 tests and exits 0. The baseline
  before this work was 783.
- `bench-smoke snapshot_machine_world_3`: size 914 B, machines 3, write
  629 us, load 1.01 ms, restore x20 1.43 ms. The load column now covers
  decoding and admission together.
- `bench-smoke snapshot_wide_heap_10k`: size 90440 B, load 2.39 ms,
  restore x4 762 us.
- `bench-smoke snapshot_deep_chain_5k`: size 115423 B, load 17.0 ms,
  restore x4 10.5 ms.
- The load column has no measured value from before this work, so the
  cost of admission against the previous checks is not stated here.
- The default aggregate admission budget is `1 << 24` units. One unit
  covers one checked value, one visited graph pair, or one resolved
  frame slot. The deep-chain case above admits well inside it.

### Open questions

#### The nested container decodes on every admission

`Snapshot[T]` and `SnapshotImage` values decode their nested container
during admission, to read one header field. A world with many nested
snapshots therefore decodes each of them once per admission. The
aggregate budget bounds that work, and item 10 shares one budget with
the nested containers. A cache keyed by container hash would remove the
repeat; nothing implements one.

#### A substituted snapshot type has no serialized name

`Snapshot[T]` matches its nested container by the semantic digest of
`T`. A digest exists for a module type entry alone, so a `T` the
verifier created by substitution has no name to compare. Admission
rejects such a value instead of accepting it unchecked. No program of
the corpus builds one, because the type of a held machine comes from a
function type the module table already carries.

#### A closure a generic body created has no admitted capture list

A closure that a generic function creates carries capture types that
name the type variables of that function. The closure value carries no
substitution, so admission has no evidence for those captures and
rejects. No program of the corpus builds one. The two candidate
answers are a capture-type field in the closure object and a
unification of the declared function type against the resolved position
type. Neither is implemented.

#### The frame chain resolves a closure call by its function type

A `CallValue` frame names its function, and admission proves that the
declared function type fits the call site. It does not prove that the
frame closure is the value the call site popped. The capture-context
rule proves that the frame closure is a closure of the frame function,
which is weaker. A stronger rule needs the operand the call site
consumed, and the image no longer holds it.

#### `Vm[T]` and `Handle[M,R]` accept a subtype result

Admission accepts a target whose result type is a subtype of the type
the handle declares, and it requires an exact mailbox type. The
covariant reading matches the consumer, which reads the terminal value
at the declared type. An invariant reading would reject a legal capture
of a subclass result. The project owner decides whether the rule stays
covariant.

#### Week 9 of the build order still names the old states

Specification section 12 asks for two edits in
`docs/specs/build-order.md`: week 9 must use `SnapshotImage` for the
admitted host state and `Image` for the editable decoded state. This
group had no mandate to commit a file under `docs/specs/` except
`language-spec.md`, so those two edits are open.

### Deferred work

- Worklist items 7 to 12 stay for groups B and C.
- `AdmissionBudget` carries a conservative default limit and a
  container byte limit. Item 10 sizes it beside a `DecodeBudget`,
  shares one ledger with nested containers, and adds the compact-input
  expansion tests. This group added the type, its charge points, and
  one negative test.
- The decode stage still uses `LoadLimits` and per-list caps. Item 10
  replaces them with one aggregate ledger and fallible reservations.
- Admission builds one `ResolvedTypes` per call, so a repeated
  admission of the same module repeats the structural verifier pass.
  A per-module cache belongs with the verified-code cache.
- The operand rule for a caller frame proves that the retained region
  is a prefix of the proved stack. The frame chain now knows the exact
  call site, so an exact count rule is possible.
