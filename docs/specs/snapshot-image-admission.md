# Snapshot Image Admission

Status: normative for the work it describes. The current implementation
does not yet separate decoded and admitted host states.

This document refines `language-spec.md` sections 17.1, 17.8, and
17.9. It also refines Week 9 in `build-order.md`.

## 1. Purpose

Snapshot state has two uses. Tools need editable state, while restore
needs state that matches the interpreter invariants.

The current decoder combines container decoding, structure checks,
state checks, and type checks. It returns `Image`, although `Image`
has public fields and can lose every checked property after mutation.

The current `SnapshotImage` already wraps an image with canonical
bytes. Its documented role says that it contains a verified image.

This document makes that boundary exact:

> **`Image` is editable snapshot data. `SnapshotImage` is its admitted,
> immutable form.**

Do not add a public type named `VerifiedImage`. `SnapshotImage` already
has that role in Loom and in the host API.

## 2. Terms

### 2.1 Snapshot container

A snapshot container is the canonical byte representation from
language specification 17.9. A container can come from trusted capture
or from an external source.

Container bytes carry no host trust. Every external container passes
decoding and admission before it becomes `SnapshotImage`.

### 2.2 Image

`Image` is the decoded host data model for one captured machine world.
It is not a Loom guest type.

`Image` supports inspection, editing, encoding, tests, and diagnostic
tools. It makes no promise about reference resolution, machine state,
or type accuracy.

An edit can create a dangling reference or a wrong-typed value. The
host representation must keep such data memory-safe and inspectable.

No restore path accepts `Image`. An `Image` never backs a guest
snapshot value.

### 2.3 SnapshotImage

`SnapshotImage` is the admitted host representation. It also backs the
Loom guest type named `SnapshotImage`.

A `SnapshotImage` owns these items:

- It owns one immutable admitted `Image`.
- It owns the canonical container bytes.
- It owns the container hash.
- It owns the exact program and ABI admission identity.
- It owns any nested snapshot containers, kept opaque until their own
  restore.

Its fields stay private. Read-only views do not remove its admission
status.

Origin does not change the invariant. Trusted capture and external
loading produce the same `SnapshotImage` guarantees.

An origin flag can support diagnostics. It must not grant trust or
select a weaker restore path.

### 2.4 Snapshot[T]

`Snapshot[T]` is a typed guest view over one `SnapshotImage`. It does
not own another image and does not add another admission state.

`SnapshotImage.cast_result[T]` compares the admitted root result type
with `T`. Success creates the typed view without copying the image.

### 2.5 Resolved type

A resolved type contains every generic substitution. It contains no
unresolved type variable and no missing type-table entry.

Resolved types are internal verifier values. They are not guest
`TypeView` values and need no serialized proof form.

### 2.6 Admission identity

The admission identity contains the module `VerifiedKey`, snapshot
format version, and runtime ABI version.

`VerifiedKey` binds the exact verifier input, compiler ABI, and verifier
version. Its `VerificationHash` includes the operation manifest digest.

The container hash identifies bytes. It does not replace the admission
identity.

## 3. State transitions

The host uses these transitions:

```text
external bytes -> decode -> Image -> admit -> SnapshotImage -> restore

verified VM cut ------------ trusted capture ------------> SnapshotImage

SnapshotImage -> edit copy -> Image -> admit -> SnapshotImage
```

`load_external` composes decode and admission. A caller that needs no
editor receives `SnapshotImage` directly.

The lower-level decoder returns `Image`. Editors and corruption tests
can use that entry point.

Admission consumes an `Image` or takes exclusive ownership. No caller
can mutate the admitted state afterward.

## 4. Container decoding

Decoding protects the host from the byte stream. It does not establish
the interpreter invariant.

The decoder enforces these rules:

- The magic and format version are supported.
- Every integer uses the canonical representation.
- Every section lies inside the container.
- Sections do not overlap and contain no trailing bytes.
- The container hash matches the canonical prefix.
- Every wire tag has a representation in `Image`.
- Every count fits its host integer type.
- Every allocation consumes one aggregate decode budget.
- Every count is checked before its allocation.

The decoder stores references as data. It does not dereference them
through unchecked indexing.

The decoder does not establish these properties:

- It does not prove referenced machines or objects exist.
- It does not prove frames name reachable program points.
- It does not prove machine lifecycle records agree.
- It does not prove values match program-derived types.
- It does not prove native designators match their targets.

An editor can create the same invalid states without a container.
Admission therefore checks every required property again.

## 5. The admission rule

Use this rule for every promotion:

> **An `Image` becomes `SnapshotImage` only when its structure resolves
> and every live declared type is accurate.**

"Declared type" includes types inferred by the bytecode verifier at a
saved program point. It never means only a type label from the image.

Admission reads one exact verified module. It rejects an admission
identity mismatch.

The operation identity covers every semantic operation field. This
includes the snapshot classification.

### 5.1 Structural resolution

Structural resolution enforces these rules:

- The image contains one root machine.
- Every machine ordinal names one captured machine.
- Every object ordinal names one object in the correct heap.
- Every function, class, type, and operation identity resolves.
- Every code identity matches verified code.
- Every frame names a reachable instruction boundary.
- Frame bases partition the local and operand arenas exactly.
- Every object has the required field or element count.
- Every closure context names a compatible closure object.
- Every literal entry names its exact program literal.
- Every parent chain terminates inside the captured world.
- Every machine reference stays inside the captured world.
- Every lifecycle variant has its required records.
- Pending, terminal, mailbox, block, pause, and gate records agree.

Structural resolution checks relationships that trusted runtime paths
use without recovery. It does not require useful or reachable future
behavior.

### 5.2 Type accuracy

Admission derives expected types from verified code and resolved
layouts. An image type claim never serves as sole evidence.

Admission checks these typed positions:

- It checks every initialized local.
- It checks every operand owned by each stopped frame.
- It checks every closure capture.
- It checks every initialized instance field.
- It checks every pending operation argument.
- It checks every accepted mailbox value.
- It checks every terminal result.
- It checks every typed native value.
- It checks every reachable collection element.

Operand types come from verifier state at the saved instruction
boundary. Pending argument types come from the state before the
pending perform.

The terminal value matches the exact declared machine result type.
`Unit` receives no exception from this rule.

### 5.3 Initialization state

An uninitialized slot contains no typed value. Admission distinguishes
that state from a real `Unit` value.

An uninitialized marker is legal only where verifier state or layout
state permits no initialized value. A live slot cannot use that marker
as a type wildcard.

The representation can use a marker, a bitmap, or explicit slot state.
The admission rule does not select one representation.

### 5.4 Generic and shared graphs

Admission applies every generic substitution before it checks a value.
It never converts a missing substitution into an unchecked slot.

The graph walk uses `(machine, object, resolved type)` as its visited
key. One object can require checks under several resolved types.

The walk is iterative and bounded. Cycles terminate without using the
Rust call stack.

Admission charges every visited pair and resolved type to one aggregate
admission budget.

#### Object type coherence

One typed edge proves that edge alone. A shared object that satisfies
two different types at two different edges still breaks the interpreter
invariant.

An edited image can point one `List[Int]` local and one `List[Str]`
local at the same empty mutable list. Each edge passes, because the
list holds no element. Verified code then appends an integer through
the first local and reads a string through the second local. A generic
instance with an empty `List[T]` field carries the same defect.

Type accuracy must stay true after every verified mutation, so
admission proves one exact type for each object:

- Admission builds one map from `(machine, object)` to one exact closed
  type. Objects never cross a machine boundary, so the machine ordinal
  completes the key.
- An instance position normalizes through the concrete class of the
  object.
- A mutable object takes the type of its first edge. Every later edge
  must name that exact type.
- A frozen object holds no write path, so a later edge may name a
  supertype of the exact type.

The mutable rule is invariance. A mutable list at exact type
`List[Dog]` reached from a `List[Animal]` edge accepts a `Cat` through
the second edge, so equality is the only sound rule there.

The same principle governs the relational types of section 5.5. A
`Vm[T]` reads the terminal value of its target, so a subtype result is
sound. A `Handle[M,R]` sends and receives its message type, so the
mailbox type must match exactly.

The map collapses the visited key to `(machine, object)` for a mutable
object, because one exact type is legal there. Admission charges the
map to the aggregate admission budget.

### 5.5 Native relational types

Some native types depend on another machine or another record. Their
outer object tag does not establish type accuracy.

Admission checks these relationships:

- `Vm[T]` matches the target machine result type.
- `Handle[M,R]` matches the target mailbox and result types.
- `PendingCall[A,R]` matches its target operation and reply types.
- `Snapshot[T]` matches the declared root result type of its nested
  container.
- `SnapshotImage` carries a nested snapshot container as an opaque
  sealed value.

Admission derives every in-world relational type from its target. It
never reads a type identity from the image. A `Vm`, `Handle`, or
`PendingCall` names a target inside the captured world, so its type
resolves from that target.

Admission resolves every machine result and mailbox type in one first
pass. It then checks relational values against those resolved types.
This order lets a handle name a target machine that admission has not
walked yet.

Admission keeps a nested snapshot opaque. It checks that the nested
container is well formed. It checks that the declared root result type
of the container matches the outer `Snapshot[T]`. It does not admit the
nested body. The nested snapshot passes full admission at its own
restore.

Handle liveness and authority are not type properties. Runtime rules
continue to enforce them.

## 6. What admission does not prove

Admission does not prove these properties:

- It does not prove termination or progress.
- It does not prove useful values or useful control state.
- It does not prove scheduler fairness or performance.
- It does not prove request-token provenance beyond structural references.
- It does not prove external authority or policy grants.
- It does not prove target-world resource availability.
- It does not prove future allocation success.
- It does not prove host service availability.
- It does not admit a nested snapshot body. Nested admission runs at
  nested restore.

A strange but structurally valid typed state remains legal. Runtime
rules and restore limits govern the excluded properties.

## 7. Construction paths

### 7.1 External loading

External loading performs these stages once:

1. Decode the container into `Image` under one aggregate budget.
2. Admit the `Image` against the exact verified module.
3. Seal the admitted image with canonical bytes and its hash.
4. Return `SnapshotImage`.

No external marker can skip admission. Serializing a `SnapshotImage`
does not transfer its in-process admission status.

Loading the resulting bytes in another process repeats admission.

### 7.2 Trusted capture

Trusted capture can construct `SnapshotImage` without a second full
graph check. The stopped VM world already maintains the admission
invariant.

This path trusts the independent bytecode verifier, VM transitions,
transfers, and native boundaries. Compiler provenance alone is not
sufficient.

The trusted constructor stays private to snapshot capture. General
host code cannot promote an arbitrary `Image` through it.

Snapshot tests can admit trusted output again. Those tests check that
capture preserves the invariant.

### 7.3 Editing

An arbitrary edit demotes `SnapshotImage` to `Image`. The edited value
requires admission before restore.

A narrow edit can preserve `SnapshotImage` when its implementation
preserves every invariant. Such an edit exposes no unrestricted
mutable reference.

An incremental verifier can later track dirty regions. Its successful
result is still `SnapshotImage`.

### 7.4 Reuse

One `SnapshotImage` can support many restores. Restore repeats no
structure or type check.

Clones can share the immutable image and canonical bytes. A cache can
key that state by container hash and exact admission identity.

## 8. Restore boundary

Restore accepts `SnapshotImage`, never `Image`. It can trust structural
resolution and type accuracy.

Restore still performs target-specific work:

- Reserve machines and aggregate resources.
- Apply target resource ceilings to all live state.
- Allocate detached heaps and machine records.
- Relocate machine and object references.
- Create fresh default-deny policy tables.
- Bind allowed external runtime services.
- Commit the complete world atomically.

A restore failure exposes no partial world. It returns every temporary
reservation and leaves the target machine unchanged.

These checks do not weaken `SnapshotImage`. They answer questions that
depend on the selected restore target.

## 9. Error stages

Decode errors report malformed or excessive container data. Admission
errors report unresolved structure or inaccurate types.

Keep the two stages distinct in the host API. The guest loader can map
both stages into `SnapshotError`.

Every diagnostic names its stage, machine, and relevant path. A deep
graph error reports a bounded path.

## 10. Host API shape

The exact function names can follow Rust conventions. The state
boundary has this logical shape:

```rust
pub fn decode(
    bytes: &[u8],
    budget: &mut DecodeBudget,
) -> Result<Image, DecodeError>;

pub fn admit(
    image: Image,
    module: &LoadedModule,
    budget: &mut AdmissionBudget,
) -> Result<SnapshotImage, ImageAdmissionError>;

pub fn load_external(
    bytes: &[u8],
    module: &LoadedModule,
    budgets: LoadBudgets,
) -> Result<SnapshotImage, SnapshotLoadError>;

pub(crate) fn from_trusted_capture(
    image: Image,
    byte_limit: usize,
) -> Result<SnapshotImage, SnapshotFail>;
```

`SnapshotImage` exposes immutable inspection and canonical bytes. It
can expose an explicit conversion back into editable `Image`.

Do not expose a public unchecked promotion. Encoding an `Image` does
not admit it.

## 11. Current tree mapping

The current implementation maps to this design as follows:

| Current item | Required role |
| --- | --- |
| `Image` | Keep it as the editable decoded model. |
| `codec::decode` | Keep container checks and return `Image`. |
| `check_machine` | Move it into admission. |
| `check_types` | Replace it with complete resolved-type admission. |
| `check_world` | Move structural relationships into admission. |
| `SnapshotImage` | Keep the name and make it the only admitted wrapper. |
| `codec::seal` | Restrict trusted promotion to capture code. |
| `restore_image(&Image)` | Accept admitted state from `SnapshotImage`. |
| trusted image cache | Store admitted state, not bare `Arc<Image>`. |
| `externally_checked` | Keep only as optional provenance. |

The split removes semantic checks from decoding. It also prevents a
mutable `Image` from acting as trusted state.

## 12. Specification edits

- Language specification 17.1 must state that guest `SnapshotImage`
  always has admitted host backing.
- Language specification 17.8 must separate decoding from admission.
- Language specification 17.8 must define the exact admission rule.
- Language specification 17.9 must keep canonical bytes independent
  from in-process admission status.
- Week 9 must use `SnapshotImage` for the admitted host state.
- Week 9 must use `Image` for the editable decoded state.

## 13. The rule this design delivers

> **Images remain editable data. Only an immutable `SnapshotImage` can
> restore, and admission proves only resolved structure and accurate
> live types.**
