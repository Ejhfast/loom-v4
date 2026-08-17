# Snapshot Image Admission

Status: normative. The implementation follows it.

Section 5.2 replaced an earlier design that made admission prove every
declared type. `docs/notes/fixes-post-week9.md` records why that
design failed and what replaced it.

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

> **An `Image` becomes `SnapshotImage` only when its structure
> resolves.**

Admission proves structure. It proves the type of no stored value.
Section 5.2 states where type honesty comes from instead.

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
- Every frame environment ordinal resolves.
- Frame bases fill the local arena exactly.
- The bottom frame starts the operand arena, and no later frame lowers
  the operand base.
- Every object has the required field or element count.
- Every closure context names a compatible closure object.
- Every literal entry names its exact program literal.
- Every parent chain terminates inside the captured world.
- Every machine reference stays inside the captured world.
- Every request token ordinal is below its target counter.
- Every lifecycle variant has its required records.
- Pending, terminal, mailbox, block, pause, and gate records agree.

Structural resolution checks relationships that trusted runtime paths
use without recovery. It does not require useful or reachable future
behavior.

### 5.2 Type honesty at run time

An earlier design made admission prove every declared type. That proof
derived each expected type from the image, which the attacker controls.
The derivation failed repeatedly, and two of its rules rejected legal
programs. This section states what replaced it.

Two mechanisms carry type honesty. Neither one reads a type from the
image.

**The interpreter tests each tag.** Every reader of a typed value tests
the tag of that value, and raises a machine fault on a mismatch. A
wrong-typed value in a restored machine therefore faults that machine.
It reaches no host assertion. `crates/lm-vm/ASSERTIONS.md` records each
assertion that remains, with the rule that carries it.

**The world checks each VM boundary.** A value that crosses a VM
boundary is checked against the type the receiving code expects. The
expected type comes from the `reply_ty` field of the performing
instruction, substituted through the type environment of the performing
frame. The verifier proves that `reply_ty` agrees with the type it
proves at that program point, so both inputs come from verified code.

The boundaries are the terminal result read, the mailbox receive, the
pending call reply, the spawn argument, the mock reply, and the restore
that returns `Vm[T]` or `Snapshot[T]`.

The check descends every element and every field. It compares a closure
with the verified closed signature of its function. That comparison
includes parameters, mutation markers, the result, and the effect row.

A native handle takes a shape test alone, because its arguments name
another machine or operation. Each handle later produces a value that
crosses a boundary of its own, and that read carries the check.

The graph copy and the type check use separate bounded walks. A copy
visits each object identity once. The type check visits each object and
expected-type pair. Sharing can give one object more than one expected
type, so one object walk cannot replace the type walk.

The check has full force where the performing frame is live. Where the
performing frame is itself restored, its type environment came from the
image, so the expected type is one the attacker chose. The interpreter
tag tests stay the guarantee inside a restored world.

One state stays representable. An empty container reached under two
different argument lists satisfies both, because it holds no value that
contradicts either. A guest that reads an element of such a container
takes a fault at the read.

### 5.3 Initialization state

The virtual machine writes an uninitialized marker into a local slot
that holds no value. It writes the same marker into an instance field
before the first assignment. The marker is a value with its own tag.

Every reader faults on the marker. `LoadField` raises
`UninitializedField`. A graph copy and a digest raise
`BoundaryViolation`. A typed reader raises `TypeMismatch`.

Admission proves nothing about where the marker appears.

### 5.6 Type environment witnesses

The verifier proves one generic body once, with the type variables of
that body opaque. One activation of the body needs the type arguments
its call site applied.

A frame with a caller takes those arguments from the call instruction.
Three positions hold no such evidence:

- the bottom frame of a machine has no call site below it;
- a closure outlives the frame that created it, and a capture type can
  name a type variable that the closure signature does not hold;
- a machine past its constructor holds no proc body and no entry frame,
  so its mailbox type has no derivation.

Signature unification cannot recover a type variable that appears in a
capture list alone. The image therefore carries witnesses.

#### A witness is a runtime carrier

A witness gives a live frame the concrete types of its activation. The
boundary check of section 5.2 reads it to substitute `reply_ty`, and a
later `Type[T]` surface will read it for reflection.

Admission checks a witness structurally alone. Each ordinal lies in
range, each entry holds no free type variable, the table is acyclic,
and each arity matches its declaration.

Admission proves nothing about whether a witness is the one execution
would have produced. A restored frame therefore carries a type
environment that the attacker chose, and section 5.2 states why the
interpreter tag tests stay the guarantee there.

#### The witness sites

| State | Witness | Admission |
| --- | --- | --- |
| Frame | closed type and effect arguments | substitutes the verified local and operand types |
| Closure | the creator type environment | substitutes the signature and the capture types |
| Instance | the concrete class arguments | compares them with the arguments of the edge |
| Machine | closed result type and mailbox type | checks handles, messages, and terminal values |

An instance witness is evidence for no admission rule, because the edge
that reaches the object already supplies `Inst(class, args)`. Admission
checks it because the image carries it, and section 14 states why the
image carries it.

`Snapshot[T]` carries no witness. The closed type table gives `T` a
canonical identity, and admission compares that identity with the
declared root type of the nested container.

#### The closed type table

One canonical table holds every closed type expression. No entry holds
a free type variable. Each entry has a canonical content digest, so one
closed type has one identity in every process.

A frame, a closure, an instance, and a machine store one small table
index. Index zero names the empty environment, so a monomorphic state
stores zero and performs no type work.

The table belongs to one world. An untrusted restore must never grow
shared module state. Restore re-interns the records of the image into
the table of the target world, and it remaps every stored index.

#### Runtime retention

The virtual machine retains a witness before any capture. Capture
cannot rebuild the environment of a closure that escaped its creator
frame.

A generic call derives or copies one index. A monomorphic call copies
zero. The world caches a derived environment by its parent environment
and its application, so a repeated call reuses one index.

#### Bounds

The language permits polymorphic recursion. A call to `grow[[T]]`
inside `grow[T]` passes the checker, so a program can create
environments without bound.

The world caps its environment nodes and its closed type nodes. It
returns a local resource fault at the cap.

#### Witnesses and value semantics

A witness is provenance. A witness never enters a guest digest,
semantic equality, or the semantic identity of a value. Two values with
equal structure stay equal when their witnesses differ.

A copy preserves the witness, because a closure keeps its creator
environment across a boundary. The two copy paths reconstruct an object
in `crates/lm-heap/src/shape.rs`, so the rule lands there. `CopyCheck`
rejects holder-local shapes and reconstructs no value, so the rule does
not land there.

#### The container

The canonical container carries the witness records and the closed type
table. The container hash covers their serialized form.

Decoding charges every witness record to the decode budget. Admission
charges every resolved closed type node to the admission budget.

## 6. What admission does not prove

Admission does not prove these properties:

- It does not prove the type of any stored value. Section 5.2 states
  the two mechanisms that carry type honesty instead.
- It does not prove that a witness is the type environment execution
  would have produced.
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

A strange but structurally valid state remains legal, and a
wrong-typed one does as well. Runtime rules and restore limits govern
the excluded properties.

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
resolution. It reads the type of no stored value.

Restore maps admitted type and environment ordinals without a fallback.
External admission checks those references. Trusted capture constructs
them from VM tables.

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

One `WorldBudget` covers the root VM and every proc it spawns. It
limits machine records, heap storage, host resources, fuel, and traces.

The image-cache byte limit controls retention only. It never rejects
an admitted image. Eviction makes a later restore repeat admission.

Each `VmConfig` remains a local ceiling. A child never receives a new
aggregate balance.

A terminal proc keeps its generation and reachable result. It releases
dead execution state, resources, and heap objects.

These checks do not weaken `SnapshotImage`. They answer questions that
depend on the selected restore target.

## 9. Error stages

Decode errors report malformed or excessive container data. Admission
errors report unresolved structure.

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
    limits: LoadLimits,
) -> Result<Image, ImageError>;

pub fn decode_with_budget(
    bytes: &[u8],
    limits: LoadLimits,
    budget: &mut DecodeBudget,
) -> Result<Image, ImageError>;

pub fn admit(
    image: Image,
    module: &LoadedModule,
    budget: &mut AdmissionBudget,
) -> Result<SnapshotImage, ImageAdmissionError>;

pub fn load_external(
    bytes: &[u8],
    module: &LoadedModule,
    limits: LoadLimits,
) -> Result<SnapshotImage, ImageError>;

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
| `check_types` | Delete it. Section 5.2 replaces it. |
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
> restore. Admission proves resolved structure. The interpreter tests
> each tag, and the world checks each VM boundary.**

## 14. Future use of the type environment

This section is not normative. It records why section 5.6 builds one
general mechanism instead of a patch for closure capture types.

The closed type table is the carrier that several later features need.
Build the carrier generously now, and build no solver.

### 14.1 Type descriptors and reflection

Language specification 17.1 defers the guest forms of
`SnapshotImage.cast_result` and `SnapshotImage.result_type`, because
version 0.2 has no `Type[T]` descriptor. Language specification section
9 already calls `type_descriptor[T]()` a witness.

A canonical closed type with a content digest is the value a `Type[T]`
descriptor holds. The same table therefore answers a dynamic cast and a
reflection query.

A generic instance stores its concrete arguments for this reason.
Admission needs no instance witness, because the edge supplies the
arguments. A reflection query has no edge to read, so the object must
answer for itself.

### 14.2 Interfaces

Version 0.2 has no traits. The cost is visible: `std/fmt` pins one
implementation for each core type, `std/math` spells out one function
for each numeric type, and collections carry eager methods instead of
one iterator protocol.

Loom is nominal for user data. A nominal interface with one conformance
for each type makes the concrete type decide the implementation, so a
closed type plus a module conformance table answers every dispatch
question. A method with a receiver already dispatches on the class tag
of the object, so it needs no witness at all.

The witness answers the calls with no receiver to dispatch on: a
constructor, a static member, and a conformance of a type with no class
tag.

No conformance record travels through a call. A design that passes one
is a design where the type alone cannot decide the implementation, for
example an orphan conformance, a second conformance of one type, or a
structural conformance. A nominal language with unique conformance
needs none of them.

Add interface types to the closed type grammar when interfaces land.
Derive every conformance from the verified module.

### 14.3 What this asks of section 5.6 now

- Give every closed type node a canonical content digest. A later
  conformance table keys on that identity.
- Store one index in a frame, a closure, an instance, and a machine.
  A later entry can hold more without moving a stored field.
- Keep the resolved conformance of a generic call inside the
  environment node when interfaces land. A cached conformance is an
  optimization of the module lookup, and it changes no call.
- Keep the witness free of selected behavior while version 0.2 lasts.
  A witness that selects behavior becomes semantic data, and a guest
  digest must then cover it.
