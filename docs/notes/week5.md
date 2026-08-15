# Week 5 Status

This note records what landed in week 5, the exact hashing domains,
the cache trust boundary, the known simplifications, the changed
tests, and the deferred work. Bytecode format version 6 carries the
sectioned container. The core pin moved to
`cb9d97929eb9f2429fbc8cb015680872be909699e3906ffe56b71365ebef03dd`,
and `core/pinned-core-defs.txt` now pins the twenty core definition
hashes.

## Landed

### Surface amendments

- Callable `sys` members are snake_case: `sys.io.print`,
  `sys.io.error`, `sys.io.read_line`, `sys.clock.now`,
  `sys.clock.monotonic`, `sys.clock.sleep`, `sys.rand.int`. The
  machine constructor `sys.vm.Vm()` keeps the one capitalized
  member. The member mapping is mechanical (`read_line` maps to
  `ReadLine`). A capitalized callable member rejects with the exact
  snake_case rewrite. Descriptors are unchanged everywhere: rows,
  policy targets, `--allow`, and the manifest identities.
- `use` is a keyword. A `use` line binds the last segment of one
  fixed `sys` path: `use sys.vm` binds `vm`, `use sys.vm.Vm` binds
  `Vm`, and `use sys.io.print` binds `print`. Use lines come first
  in a module, one dotted path per line. The bindings resolve below
  locals and module definitions, in the value namespace. A `use` of
  any other root, an unknown group or member, a bare `use sys`, a
  four-segment path, a duplicate binding, or a misplaced line
  rejects with `E1052`; the non-`sys` diagnostic says module imports
  arrive with packages. A `use` alias never grants authority and
  never changes a row: tests prove an aliased perform still charges
  the row (`E1046`) and still needs policy (`PolicyDenied`).
- `Request.as_call` takes an exact `Operation` descriptor:
  `q.as_call(Io.Print)`. The compiler supplies the typed signature
  from the manifest. A group descriptor and a machine control
  operation reject with `E1004`; the old callable-argument form
  rejects with `E1004` and names the exact rewrite.

### The sectioned container (format version 6)

- The container is: the `LMBC` magic and version header, a section
  table with three `(offset, length)` pairs, the semantic region,
  the export section, and a reserved (empty) debug section.
- The semantic region holds the strings, types, selectors,
  applications, classes, functions, and the entry index. The
  definition names left the semantic region: the semantic bytes of a
  definition do not contain its own name. The export section holds
  the class names and the function names in definition index order.
- The decoder validates the section table with plain arithmetic
  before it reads any section: the sections must be contiguous, in
  order, and cover the input exactly. A wrong offset, an overlap, a
  gap, a size past the end, or a truncation at any boundary rejects
  with `BadSectionTable` before any allocation is sized from the
  table. Export counts must equal the semantic definition counts.
- `lm build` writes the artifact and the interface atomically: a
  temporary file in the same directory, then a rename.

### Definition and module identity (specification 3.7)

Definition hashes are computed over a dedicated canonical encoding,
never over raw section bytes. Every module-global index is replaced:

- function and class references become the referenced definition
  hash, or the member ordinal for a same-component reference;
- string-pool indices become the inline string content;
- type-table indices become a structural type digest with class
  references by definition identity;
- application indices become an application digest;
- selector indices become the selector name;
- lifted closure bodies embed through a body digest and take their
  own identity from the parent hash plus the occurrence index;
- local slots, block indices, argument counts, manifest operation
  slots, and table-edit operands stay raw little-endian values
  (function-local or manifest-dense, both order-stable).

The canonical instruction encoder is an exhaustive match with no
wildcard arm, so a future instruction with a new index operand fails
to compile until its canonical form is decided.

Strongly connected components come from Tarjan's algorithm in
iterative form with an explicit work stack (the definition graph is
untrusted input). Traversal order is pinned: roots in ascending
node index, successors in ascending reference order. Tarjan emits
components callees-first, and that emission order is the hash
schedule. The graph nodes are the classes, functions, types, and
applications. An abstract enum parent references its case classes,
so a family is one component: the closed arm set is part of the
family identity, and the name-ordered member ordinals separate
same-named arms of families whose arm sets differ (`StepEvent`
versus `DriveEvent`: `Ran` and `Waiting` stay distinct because the
two arm sets differ). Two families with identical arm names,
fields, and methods share one hash: definition hashes are purely
structural by design, and nominal distinctness lives in the export
table and the module-local class indices that the runtime uses.

The hashing domains, written out:

- `lm-type-v1`: structural type digest — tag byte, class identities,
  child type digests, mut markers, canonical row bytes.
- `lm-app-v1`: application digest — type digests plus row bytes.
- `lm-closure-body-v1`: closure body digest — the canonical function
  bytes of a lifted closure body.
- `lm-def-component-v1`: component hash — the compiler ABI version,
  the operation manifest digest, the member count, and each member's
  kind, length, and canonical bytes in canonical member order
  (sorted by name, then kind, then index).
- `lm-def-member-v1`: definition hash — the component hash plus the
  member ordinal.
- `lm-def-closure-v1`: closure definition hash — the parent
  definition hash plus the occurrence index of its first
  `MakeClosure` site.
- `lm-def-closure-cyclic-v1`: the fallback for a hand-built
  `MakeClosure` cycle — the component hash plus the function index.
- `lm-module-sem-v1`: module semantic hash — the format version, the
  compiler ABI version, the manifest digest, the explicit empty
  import set (a zero count, which week 6 extends), the export table
  as name-sorted `(kind, name, definition hash)` triples over every
  class and every non-closure function, and the entry definition
  hash.
- The container hash is plain SHA-256 over the exact container
  bytes.

`COMPILER_ABI_VERSION` lives in `lm_bytecode::identity`. Bump rules:
increment on any change to instruction semantics, the canonical
identity encoding, the hash domains, or the lowering ABI. The
operation manifest is covered separately: every definition hash
includes `lm_abi::manifest_digest()`.

Tests prove: a comment edit changes no hash; a body edit changes
only that definition and the module hash; reordering definitions
that share string literals and generic instantiations renumbers the
pools and changes no definition hash and no module hash; renaming a
definition changes no definition hash (its own or any caller's) and
changes the module hash through the export table; a mutually
recursive component hashes deterministically with distinct member
hashes; a three-thousand-definition call chain hashes on a 256 KiB
Rust stack.

### The comment-edit behavior

The compiler stores no spans, no source, and no debug content, and
the emitted debug section is empty. A comment-only edit therefore
produces a byte-identical container: the semantic hashes are
unchanged and the container hash is unchanged too. Demonstration
with `lm build examples/01-basics/factorial.lm` before and after a
comment edit, both builds print:

```text
semantic  76baa91d1100af1d7cbea5fd608f2a626105bd65d98073191b87f2c98a70c96c
container 1ac47deb924fc797bd822dd0f92959c9b55c99e62f5b9260d01ae7e470e11d8a
```

The container hash starts to move independently when debug content
(source maps) arrives.

### Hash linking

`lm_bytecode::corelink` is deleted. `core/pinned-core-defs.txt` pins
the definition hashes of the twenty core classes the runtime needs
(`Option`, `Result`, `IoError`, `RunResult`, `StepEvent`,
`DriveEvent`, and their arms). At load, the module identity is
computed and every class whose definition hash equals a pinned hash
fills its slot in one `CoreLayout` table. The verifier and the VM
share that one table through `LoadedModule`. The lookup key is the
hash; the labels in the pin file only name the slots. No name-based
and no positional core lookup survives in `lm-verify` or `lm-vm`
(grep-level check), and tests prove a corrupted embedded core arm no
longer resolves and the module rejects. The per-module positional
core copy still exists physically (modules embed the core bodies
until week-6 interfaces allow sharing), but every reference resolves
through its hash.

A module can hold two classes with one pinned hash, for example a
user enum family that is structurally identical to the core
`Option`. The last class index wins, which selects the embedded core
copy. Structural equality means semantic equality here; only the
display name differs, and the display name comes from the export
section.

### The verified-code cache

The cache keys on the verification hash, never on the semantic hash.
The two hashes answer different questions:

- the semantic hash answers "do these bytes mean the same program?".
  It replaces every module-global index with content, so two modules
  that differ only in an index share it.
- the verification hash (`lm-module-verify-v1`) answers "did the
  verifier approve this exact representation?". It covers the
  semantic region with every index preserved, plus the operation
  manifest digest.

`lm_vm::VerifiedCache` is an in-process set keyed by (verification
hash, `COMPILER_ABI_VERSION`, `VERIFIER_VERSION`).
`load_cached`/`load_bytes_cached` skip the verifier on a hit; a
counter proves the skip. The trust boundary, exactly:

- The loader computes the key from the decoded content on every
  load. No hash stored in an artifact enters the key; the container
  stores no hash at all, so a forged stored hash cannot exist, and
  tampered bytes always miss the cache and meet the full verifier.
- The decoder's structural checks and the identity preflight run on
  every load, cached or not.
- The key fixes every verifier input, so a hit skips every verifier
  pass, not only the per-function dataflow. A hit certifies: a module
  with these exact verifier inputs passed the whole verifier before.
  The core layout and the dispatch rows are pure functions of the
  same inputs, so they stay valid. The remaining assumption is
  SHA-256 collision resistance.
- The manifest digest is in the hash because the row and signature
  rules read the manifest, and the container does not store it. The
  semantic hash covered it through every definition hash; an
  index-preserving hash over the container does not.
- Definition names and debug content stay out of the hash. The
  verifier reads neither, so a rename and a debug edit keep the
  cache hit. Tests prove both directions: a rename holds the
  verification hash, and a duplicate selector or a dead pool entry
  moves it.
- A rejected module never enters the cache.

Two rules stay in the verifier for their own sake, not for the cache.
The canonical identity encoding replaces an index with content, so a
table whose index is also a runtime key must map indices to content
one-to-one. The selector table is such a table: the identity encoding
carries the selector name, and the dispatch row uses the selector
index. The structural pass therefore rejects a duplicate selector
name. A test sweeps the selector, string, application, type, and
class tables and proves that a cached load and an uncached load
always agree on admission.

### Interfaces and the CLI

- `lm build file.lm` writes `build/debug/<name>.lma` and
  `<name>.lmi` and prints the semantic and container hashes. The
  build directory is created relative to the current working
  directory.
- The `.lmi` interface holds the ABI versions, the module semantic
  hash, and the export table: kind, name, rendered full signature
  (rows included), and definition hash, for every top-level class,
  enum, enum arm, and function, in declaration order. The bytes are
  deterministic; corruption tests cover truncations, trailing
  bytes, and a bad kind tag. `lm inspect file.lmi` dumps it.
- `lm run file.lma` executes a prebuilt artifact through the decoder,
  the cache, and the verifier; `lm run file.lm` keeps working.
  `lm disasm` accepts both source and artifacts. `lm inspect
  file.lma` prints the hashes and the table sizes; `--live` keeps
  the run-and-dump behavior.
- Determinism gates: building the same file twice is byte-identical
  for the artifact and the interface; the recompiled core image
  matches the new pin.

## Simplifications inside the slice

- `use` binds fixed `sys` paths only; module and package imports are
  week 6. All bindable names are lowercase group or member names
  plus `Vm`, so no `use` binding can collide with a prelude name;
  the position of the `use` layer relative to the prelude is
  therefore unobservable this week.
- The module-hash export table excludes lifted closure bodies. Their
  content reaches the module hash through their parents' definition
  hashes, and their generated names stay out of identity.
- Structurally identical definitions share a definition hash by
  design. Nominally distinct enum families stay distinct through the
  family component rule above.
- A rename inside a cyclic component changes the canonical member
  order and therefore the component's hashes; the specification
  orders cyclic members by exported name. The rename-invariance
  guarantee applies to definitions outside a cycle. A closure inside
  a component with named definitions orders by its generated ID.
- A class hash covers its kind, arity, parent identity, fields,
  method table, and (for an enum parent) the arm list. The
  synthesized constructor function `<new C>` and an `init` body are
  separate definitions with their own hashes; a caller of the
  constructor covers them transitively.
- Rows keep the canonical-name text encoding inside artifacts. The
  manifest digest inside every definition hash pins their meaning; a
  binary row encoding can come with the week-6 interface work.
- The interface signature text is a provisional rendering (`$0` for
  type parameters); week 6 defines the binary import-slot form on
  top of the same table.
- The cache is module-level and in-process; the CLI uses it inside
  one invocation only. A persistent content-addressed build cache is
  week-6 work.

## Changed tests

Existing expectations changed only where the surface amendments or
the new rejection order reach them:

- The casing sweep rewrote the `sys` member spelling in
  `examples/04-effects/*.lm`, `tests/ui/perform-without-row.lm`,
  `tests/ui/answer-type-mismatch.lm`,
  `tests/fuzz-regressions/unterminated-block.lm`, and the source
  strings in `week4.rs`, `week4_verifier.rs`, `bench_smoke.rs`, and
  `fuzz.rs`. The `as_call` arguments moved to descriptors
  (`as_call(Io.Print)`).
- `week4.rs`: `q.as_call(g)` on a function value moved from `E1004`
  to `E1051` (it is not a descriptor); a new case checks that the
  old callable form gets the `E1004` rewrite diagnostic;
  `sys.io.Blast` became `sys.io.blast`.
- `lm-bytecode` unit tests: trailing bytes now fail the section
  table (`BadSectionTable`); the corruption offsets moved to the
  semantic region start; new tests cover section offsets, boundary
  truncations, export count mismatches, and the absence of names in
  the semantic region.
- `corruption.rs`: five rejection needles now match the identity
  preflight messages (`selector`, `method function`, `class`,
  `type index`, `type application`), because identity validation
  runs before the verifier tables and rejects the same defects.
  `unknown_opcode` locates the final `Return` through the section
  table.
- `fixes.rs`: one needle moved from `invalid type index` to
  `type index` for the same reason.
- The core pin moved with format version 6 (expected churn), and
  `core/pinned-core-defs.txt` is new; the determinism and
  prelude-independence gates pass unchanged.
- The fuzz corpus regenerated for format version 6. The local-count
  bomb patches the count through the section table with a structural
  offset check. The replay test now asserts the rejection layer per
  seed: the bomb at the decoder, every forgery at the verifier.
- The differential corpus gained two `use`-alias programs, so the
  oracle covers the new resolution layer on the pure subset.

## Review fixes

An independent review confirmed one defect and two documentation
gaps. The defect: a byte stream with a dead duplicate pool entry
kept the semantic hash equal and rode a cached admission past the
verifier. The fix runs the module-level structural pass on every
load; the cache skips only the per-function dataflow. The trust
boundary above records the corrected claim, and a regression test
replays the attack. The family-distinctness wording and the
canonical-operand comment are corrected.

One review observation is deferred with rationale:

- Definition hashes are structural, so nominal distinctness rests on
  module-local class indices and the export table. That is sound
  while class values are not first-class (`E1018`). When class
  values land (reflection, week 13), class-value equality must not
  use the bare definition hash, or names must enter the identity.
  This decision is recorded here for that week.

## The second review pass

A second pass over the week-5 work confirmed two more defects. Both
are fixed. Neither fix moves a hash: the artifact hashes of
`examples/01-basics/factorial.lm` and the core pin are unchanged, and
the determinism gates pass.

- A duplicate selector name rode a cache hit. The canonical encoding
  writes the selector name, so a copy of a used selector keeps the
  module hash equal. The copy is a different dispatch key, and only
  the per-function pass resolved the method. A cache hit skips that
  pass, so the loader admitted the module and `DispatchRow::method`
  indexed past its table. The fix adds the selector uniqueness rule
  to the structural pass, which runs on every load.
- Two classes with one definition hash rode a cache hit the same way.
  Class hashes are structural, so `New A` retargeted to a
  structurally identical `B` keeps the module hash equal, and only
  the dataflow pass reads the class index. A nominal class hash does
  not close this: two classes with one NAME collide the same way, and
  no rule makes class names unique. The cache key is therefore the
  fix, not the hash domain. The class identity question moves to
  week 6, where import slots decide what a definition hash must
  distinguish.
- The cache key was the root cause behind all three cases, so it
  moved from the semantic hash to the verification hash. The section
  above records the corrected boundary. This also removed a defect
  introduced during the pass: an intermediate key over the container
  bytes dropped the operation manifest, which the semantic hash had
  covered transitively.
- The loader panicked on a hand-built closure cycle. A function that
  makes a closure of itself is a one-member `MakeClosure` cycle.
  `closure_body_digests` removed the function from the active path
  before it serialized that function's own body, so the self
  reference missed the cycle marker and read an unfinished digest.
  `load_bytes` panicked on crafted bytes, which is a loader denial of
  service: the identity runs before the verifier. The fix keeps the
  function on the path until its digest is complete.

The superlinear observation from the first pass is now closed as
measured, not deferred. The intra-component lookups in
`ordinal_of`, `type_digest`, `app_digest`, and `body_digest` are
still linear scans. Cost is the number of intra-component references
times the component member count. Measured on generated components:
a 868 KiB artifact with a 200-member dense component hashes in
3.9 ms, and a 3200-member call cycle hashes in 4.4 ms. The growth is
near-linear in artifact size at these sizes, so no small input makes
a large delay. The week-6 identity work still replaces the lookup
vectors with maps. The load path stays the place to watch, because
identity runs on untrusted bytes before the verifier.

## Deferred work

- Package and module imports, the manifest, per-module compilation
  against interfaces, and the on-disk build cache (week 6). The
  explicit empty import set in the module hash is the extension
  point.
- Definition-level verified-code caching; module-level is the
  week-5 bar.
- True sharing of core bodies across artifacts; modules still embed
  the core copy, hash-resolved.
- Debug content (source maps) and a container hash that moves
  independently of the semantic hashes.
- A binary row encoding inside artifacts.
- CI workflow files, Miri, `cargo-fuzz` targets, and committed
  benchmark distributions stay deferred as before.
