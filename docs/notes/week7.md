# Week 7 Status

This note records the week 7 work. It covers:

- what landed;
- the crate-split decision, its justification, and the rejected
  alternatives;
- the canonical digest and its hash seam;
- the classification and reservation decisions;
- the simplifications inside the slice;
- the changed tests, the new tests, the open questions, and the
  deferred work.

Bytecode format version 11 carries the `Digest` type and the three
digest instructions. The interface format is version 3. The compiler
ABI version is 5 and the verifier version is 4, because the
instruction set and its typing rules both changed. The operation
manifest ABI version stays 1. The core image pin moved to
`e3c4259141f594e7e732452db2fe874a13174723508ac1603409033cc0ef7053`
and `core/pinned-core-defs.txt` holds the twenty regenerated core
definition hashes: the compiler ABI version enters every definition
hash, so a new instruction moves them all.

## Landed

### Two new crates below the VM

The heap, the native shapes, and the graph engine now sit below
`lm-vm`, in the order section 1 of the build order names.

- `lm-heap` holds the object table, the allocation pages, the native
  shape descriptors, the reusable graph work tables, and the digest
  cache. It decides no reachability at all.
- `lm-graph` holds one non-recursive traversal engine and the modes
  that run on it.
- `lm-vm` re-exports `Heap`, `Object`, `ShapeDesc`, `GraphLimits`, and
  the fault codes, so no caller outside the workspace changed.

The fault codes moved to `lm-abi`. The heap and the graph engine name
them, and specification 25.1 lists `faults.abi` as manifest content,
so the code set belongs beside the operation manifest. Two codes
joined: `BoundaryLimit` and `BoundaryViolation`.

### One shape declaration

`ShapeDesc` is the single declaration point specification 25.5 asks
for. Each of the fourteen shapes states its display name, whether it
holds references, whether it is born frozen, its canonical child
order, its boundary policy, its digestibility, and its snapshot
classification. `Object::children` is the one shape walker, and every
mode reads reachability and order from it.

`lm inspect --shapes` prints the table.

### One traversal engine

`lm_graph::walk` is the only traversal. It owns:

- reachability and child order, through `Object::children`;
- the identity table and the canonical traversal ordinals;
- the object, edge, byte, and work limits;
- an explicit work list, so depth never reaches the Rust stack.

A mode adds only its own result state, through the `Visitor` trait.
The eight modes are mark, deep freeze, frozen verification, boundary
transfer, structural copy, canonical digest, detached inspection, and
snapshot traversal.

The identity table is a per-heap epoch table over the object slots.
`seen[slot]` holds the walk epoch that last reached the slot, so a new
walk needs no clearing pass and no hashing. The heap lends the table
to the engine for the length of one walk and takes it back afterwards,
so a walk started inside another walk allocates its own table instead
of sharing one.

### The migrated paths

- **Mark** replaced the header `marked` bit with the epoch table. The
  heap now exposes `sweep`, and the graph engine decides what to keep.
- **Deep freeze** validates the whole reachable graph against the
  limits before it sets one bit, so a rejected freeze changes nothing.
- **Transfer** kept its three passes and its observable behavior, and
  it became failure-atomic. A rejected copy frees every shell it
  allocated, so the destination keeps its earlier live count and its
  earlier byte count.

The six cyclic and shared-subgraph transfer oracles landed before the
migration started and passed unchanged after it. Every earlier test
passed unchanged as well.

### The canonical digest

`value.digest()` returns a frozen `Digest`, as specification 24.8
declares. The encoder:

- walks in canonical order, which is a preorder over the declared
  child order of each shape;
- assigns an ordinal at the first encounter of an object and writes a
  back-reference for every later encounter, so sharing and cycles are
  part of the encoding;
- writes a map in insertion order, key before value, and never reads
  the derived lookup index;
- names an operation by its manifest identity hash;
- names a class and a function by the definition hash the identity
  layer proved, never by a numeric slot of one linked program;
- rejects a graph that is not frozen with `UnsendableValue`, and a
  nondigestible shape with `BoundaryViolation`.

A frozen object never changes, so the heap caches the digest of a
digested root, keyed by slot and generation.

`==` on two digests compares by value, through the new `EqDigest` and
`NeDigest` instructions. Reference identity would have said that two
equal digests in different slots differ, which contradicts
specification 6.4.

### Classification and the resource registry

Every native shape declares `MachineState` or `HostAttachment`. No
heap shape is a host attachment today: the week-7 shapes are data,
code, descriptors, and holder-local designators, and every live host
state sits outside the guest heap.

Every operation in the manifest declares the same classification.
`Io.Print`, `Io.Error`, `Io.ReadLine`, and `Clock.Sleep` may suspend
and are host attachments. Everything else must complete inside the
host call. The classification has a production consumer: a host that
suspends an operation declared machine state breaks its contract, and
the machine faults with `HostFault` instead of waiting.

Each machine owns a host-side resource registry outside its guest
heap. It records the resource kind, the owning machine, the scope
identity, the pending operation ordinal, and the cleanup state. One
kind exists this week: a pending suspending operation, whose scope
identity is the host completion token. The completion closes the
record, and machine termination closes every live record without
invoking a guest callback and without replacing a stored fault.

`World::snapshot_preflight` reads the registry and the guest graph, as
specification 25.5 requires, and runs the canonical snapshot
traversal.

### The `VmState` split

`Machine` now holds `VmState` plus the four kinds of state a snapshot
never copies:

| Field | Kind | Snapshot |
|---|---|---|
| `vm: VmState` | serializable machine state | copied |
| `table: PolicyTable` | policy | excluded (17.2) |
| `active: u32` | execution ownership | excluded |
| `resources: ResourceRegistry` | active host work | excluded |
| `config`, `children` | resource control | excluded |

The host completion token moved out of `Pending` into the registry, so
`Pending` holds only the operation slot, the arguments, and the
ordinal. Specification 17.2 lists pending requests inside snapshot
contents and live host callbacks outside them, and the split now
matches that line exactly.

### Parent resource reservation

`Vm.New` reserves the child from the parent child budget before it
creates any record. A refused reservation creates no machine, allocates
no handle, and charges nothing. The child receives the rest of the
parent budget, so a machine tower can never grow deeper than the
budget the root minted.

### Brace closures and trailing closures

`{ |x| ... }` is the second closure spelling. A left brace followed by
a pipe starts a brace closure; every other left brace stays a map
literal, and `{}` stays the empty map. The scanner opens a statement
block for a brace closure, so its body takes newline separators, and a
right brace ends a one-expression body.

A closure that starts on the same line as the end of a call becomes
the final argument of that call, in either spelling. A call accepts at
most one trailing closure, and no postfix suffix may follow one.

Both spellings reach one typed HIR node and one bytecode form. Three
tests compare the encoded bytes of the `do` form, the brace form, and
the plain argument form.

### Runnable outputs

```text
$ lm run --show-result examples/06-graphs/cycle-digest.lm
Done(2de09e7c78b3e348f0e0d98f56b5ebbb44a9018abaa4d8596550e51fff0f8930)

$ lm run --show-result examples/06-graphs/brace-closure.lm
Done(42)
```

The digest example builds one ring, allocates spare objects, then
builds an equal ring. The second ring uses other heap slots, and the
checked hexadecimal output is the cross-process stability gate: a
digest that read a slot number would move the checked line.

## The crate-split decision

### What moved

`crates/lm-vm/src/heap.rs` split into `crates/lm-heap/src/lib.rs` and
`crates/lm-heap/src/shape.rs`. `World::transfer` and `Heap::freeze` and
`Heap::collect` moved into `crates/lm-graph/src/mode.rs`. The fault
codes moved into `crates/lm-abi/src/fault.rs`.

### Why two crates and not one

The build order names `lm-heap` and `lm-graph` separately. One crate
would have been less churn, and the separation buys one real property:
the graph engine cannot reach into heap internals. It sees the object
table only through the public heap API, and the heap decides no
reachability. That boundary is what keeps a future mode from growing a
private shape table.

### Rejected: a trait over `Object` inside `lm-vm`

The first design kept `Object` in `lm-vm` and gave `lm-graph` a trait
the VM implements. It was rejected because the mode visitors need the
concrete shapes, so every mode would have stayed in `lm-vm` and the
new crate would have held one generic loop and nothing else. The
"one child-order contract per shape" gate would then have had no
single home.

### Rejected: a hash map identity table

The first engine used one `HashMap<u32, u32>` per walk, as the old
transfer pass did. It was rejected for the mark mode: the collector
retires one lookup per reachable object, and the old mark path used a
header bit with no hashing at all. The epoch table over the object
slots keeps the mark cost where it was and gives every other mode the
ordinals for free.

### Rejected: keeping the mark bit and adding a table beside it

Two visited sets would have been two answers to one question. The
epoch table replaced the bit, so the header lost a field.

## The digest hash seam

Specification 10.3 names BLAKE3-256. The workspace takes no
crates.io dependency and hand-rolls its hashes: `lm-abi` carries a
SHA-256 implementation, and every identity hash in the project uses
it.

Week 7 puts the hash behind one function, `lm_graph::digest::hash`,
and calls the existing SHA-256 there. The canonical encoding, the
ordinal assignment, the back-references, and the digest cache are the
real work, and none of them reads the hash. Changing the function
changes one line and every checked digest output.

The open question below records the three ways to close the gap.

Float normalization is not implemented, because `Value` has no float
variant yet. The rule enters with floats.

## Simplifications inside the slice

- **Detached inspection and snapshot traversal have no guest entry
  point.** Both are engine modes with their own visitors, limits, and
  Rust-level tests. Week 7 adds no operation that reads out of another
  heap beyond `call.args()`, which must stay on the transfer contract,
  and it defines no snapshot byte format. `World::snapshot_preflight`
  is the Rust-level entry the week-9 snapshot work will call.
- **Frozen verification is a standalone mode and a fused check.** The
  production transfer path runs the frozen test inside its copy
  visitor, in the same walk, instead of walking twice. The standalone
  mode exists for the preflight paths and is tested on its own.
- **The interpreter still runs as a method of `Machine`, not of
  `VmState`.** Allocation needs the policy table roots as well as the
  `VmState` roots, so `execute(vm: &mut VmState, ...)` of
  specification 14.12 would need the root set passed in. The field
  split landed; the entry-point shape did not.
- **The mark mode publishes unbounded limits.** The heap cap already
  bounds the object table, and a mark that refused to finish would
  turn a full heap into a crash instead of a collection. Every other
  mode uses the published defaults from `VmConfig.graph`.
- **The child budget bounds tower depth per branch, not the total
  machine count.** Full transitive accounting of fuel and heap bytes
  needs the proc scheduler and waits for week 8.
- **`World::show_value_inner` is still recursive.** It is display
  only, and it has a depth cap of 32 and an ancestor list. It is the
  one recursive graph walker left, and it is deliberately not a mode:
  it prints a shared child twice and a cyclic child as `<cycle>`,
  which the canonical order would change. Every test that reads a
  dump is an oracle for that behavior.

## Changed tests

- `crates/lm-vm/src/heap.rs` unit tests split. The storage cases moved
  to `crates/lm-heap/src/lib.rs`; the reachability cases moved to
  `crates/lm-graph/src/mode.rs` and `crates/lm-graph/src/engine.rs`.
- `crates/lm-testkit/tests/gc.rs` calls `lm_graph::collect` where it
  called `Heap::collect`.
- `crates/lm-testkit/tests/fuzz.rs` takes `examples/06-graphs` into
  the mutation seed corpus and raises the corpus floor to eleven.
- `tests/fuzz-regressions/*.lmbc` regenerated for container version
  11. The layer-specific rejection assertions are unchanged.
- `core/pinned-hash.txt` and `core/pinned-core-defs.txt` regenerated.
  The container hash moved with the format version, and every
  definition hash moved with the compiler ABI version.
- `crates/lm-testkit/tests/bench_smoke.rs` gained four graph entries.

## New tests

- `crates/lm-testkit/tests/week7_graph.rs`, 15 cases: the six migration
  oracles for cycles and shared subgraphs, five guest digest cases,
  two semantic-identity gates, the shape-table dump, and the two
  example outputs.
- `crates/lm-testkit/tests/week7_resources.rs`, 6 cases: the pending
  operation registry and its cleanup, the host suspension contract,
  the fail-atomic child reservation, budget inheritance, the snapshot
  preflight, and the nested sandbox example on the production path.
- `crates/lm-testkit/tests/week7_closures.rs`, 11 cases: both
  spellings encoding identically, every closure part in brace form,
  trailing closures on every call form, map disambiguation, and five
  negative rules.
- `crates/lm-graph/src/engine.rs`, 4 cases: canonical ordinals against
  allocation order, cycle and sharing termination, each limit
  rejecting on its own, and a 100,000-deep chain on a 256 KiB stack.
- `crates/lm-graph/src/mode.rs`, 16 cases: one per mode plus the
  failure-atomic transfer, the digest properties, the limit rejection
  of every mode, and every mode on a 50,000-deep chain.
- `crates/lm-heap/src/shape.rs`, 5 cases: the tag table, the child
  order agreeing with the reference flag, holder-local shapes never
  digestible, map child order, and the dump.
- `crates/lm-heap/src/lib.rs`, 6 storage cases including the free
  rollback and the digest cache generation rule.
- `crates/lm-vm/src/resource.rs`, 3 cases for the registry.
- `crates/lm-abi/src/lib.rs` and `fault.rs`, 3 cases for the
  classification table and the code names.
- `tests/ui/`, 5 new pairs for the closure rules.
- `tests/run-pass/`, 2 new pairs for the closure spellings.
- `tests/fuzz-regressions/`, 2 new source seeds for the new parser
  surface.

Test count: 541 before, 604 after.

## Measurements

`cargo test -p lm-testkit --test bench_smoke -- --nocapture`, debug
profile, one run each:

| Entry | Before | After |
|---|---|---|
| `alloc_gc_100k` | 77.7 ms | 78.1 ms |
| `list_push_100k` | 69.8 ms | 68.7 ms |
| `freeze_chain_50k` | — | 51.7 ms |
| `transfer_graph_20k` | — | 43.0 ms |
| `digest_graph_20k_plus_1k_cached` | — | 46.4 ms |
| `mark_sweep_100k_under_256k` | — | 79.0 ms |

The first migration measurement showed `alloc_gc_100k` at 91.9 ms, an
eighteen percent regression. The cause was the digest-cache lookup the
sweep ran for every freed slot. Most heaps hold no digest, so the
sweep now skips the lookup when the cache is empty, and the path
returned to its earlier cost.

## Open questions

### BLAKE3-256 against the hand-rolled SHA-256

Specification 10.3 names BLAKE3-256 for the value digest. The
implementation uses the SHA-256 of `lm-abi` behind one function. The
three ways to close the gap:

1. **Hand-roll BLAKE3-256.** It keeps the zero-dependency rule and
   costs roughly the size of `lm-abi/src/sha.rs` plus a tree mode. It
   also puts a security-relevant primitive under this project's
   maintenance.
2. **Vendor one dependency.** It is the least code and the most
   accurate, and it breaks the rule the workspace has held for six
   weeks. The rule is not written down as normative anywhere; it is a
   practice.
3. **Amend the specification to name SHA-256.** Every other identity
   hash in the project is SHA-256 already, so this would make the
   document match the implementation and remove one primitive. It
   also loses the speed BLAKE3 was chosen for.

This is a specification-versus-implementation disagreement, not a
documentation defect. The specification is unchanged.

### No stable fault code names a resource budget

Specification 12.3 has `HeapLimit`, `StackLimit`, and `BoundaryLimit`.
None of them means "the parent has no child budget left". Week 7 uses
`InvalidVmState` for the refused reservation, because minting a child
without budget is an illegal control call for that machine state. The
better answer is a new stable code, for example `ResourceLimit`, and
that is a specification change the week did not make.

The same gap appears in the resource registry: a machine that reaches
its live-resource cap faults `HostFault`, because the host cannot
serve one more resource.

### The classification is outside the manifest digest

`OpDef.snapshot` is manifest content, and `manifest_digest` does not
cover it. Specification 25.2 lists the identity inputs as ABI version,
group name, member name, the parameter and result encoding, and the
semantic revision. The classification is none of those, and including
it would move every definition hash and the core image pin. The
question is whether a classification change is an ABI change. It
changes what a host may do, so the argument for including it is real.

### A trailing closure after a labeled argument

Specification 6.1 says labeled arguments follow positional arguments,
and it says a trailing closure becomes the final argument. The two
rules conflict when a call already carries a label:
`f(x, mode: 1) { |v| v }` reaches the argument arranger as a
positional argument after a label and rejects with E1006. The
specification does not say which rule wins. The implementation
rejects, which is the conservative reading.

### `execute` over `VmState`

Specification 14.12 names one internal entry point,
`execute(vm: &mut VmState, mode: StopMode) -> VmExit`. The field split
landed, and the entry point did not: allocation inside the
interpreter needs the policy-table roots, which are outside `VmState`
by the 17.2 rule. Either the root set becomes a parameter, or the
mock-handler roots move into `VmState`, which would put policy content
into snapshot bytes.

## Deferred work

- The snapshot byte format, the consistent cut, and restore. Week 7
  lands the ordinal-assigning walk and the preflight rule only, as
  the build order states.
- A guest entry point for detached inspection. `std.value.inspect` of
  specification 24.8 arrives with the standard library.
- `deep_equal`. The digest is the fast reject it needs, and the
  cycle-safe structural comparison behind it is not written.
- Float normalization inside the digest encoding, with floats.
- A `Digest` value in the typed-HIR oracle. The oracle has no heap and
  no code identity, so it rejects a digest program instead of taking a
  second, weaker encoder. A digest case therefore stays out of the
  differential corpus and lives in `week7_graph.rs`.
- Transitive accounting of fuel and heap bytes across a machine
  tower. The child budget bounds depth only.
- Resource kinds beyond a pending operation. Files, sockets, and
  timers arrive with week 10.
- A `Digest` method surface: `to_bytes`, `to_text`, and ordering. The
  type carries value equality and display only.
- Committed benchmark distributions, `cargo-fuzz` targets, Miri, and
  CI workflow files stay deferred as before.
