# Identity, Linking, and the Build Store

Status: normative for the work it describes. This document refines
`language-spec.md` sections 3.7 and 8.6, and it replaces the identity
rules those sections state today. Section 13 lists the exact
specification edits.

## 1. Purpose

Loom conflates several identities in one hash. A class hash today
covers the class name, the field layout, the method signatures, and
the method bytecode. That single hash answers four different
questions, so a change for one purpose moves an answer for another.

This document separates the four identities, defines what each one
covers, and names the consumer of each. It then defines the linker
merge rule, the component labeling rule, and the three-stage build
store.

## 2. The four identities

### 2.1 QualifiedKey

The nominal identity of a definition. The value is the fully qualified
declaration path, for example `mathlib.geometry.Point`.

- The package name of the manifest supplies the root, never the
  dependency key.
- Two definitions are the same nominal definition when their
  QualifiedKey values are equal.
- The linker uses QualifiedKey. The type checker never compares
  QualifiedKey values, because it works on class indices inside one
  module.

Class equality at run time stays an index comparison inside one linked
program. QualifiedKey decides link-time identity, and the linker
materializes that decision as one class index. No run-time path
compares a hash.

### 2.2 StructuralHash

The name-free content identity of a definition. It covers the layout
and the implementation:

- for a class: the kind, the generic arity, the parent identity, the
  field types, the selector set, the method signatures, and the
  implementing function identities;
- for a function: the signature, the effect row, the local table, and
  the instruction stream.

A declaration name never enters its own StructuralHash. Section 4
defines how a reference to a nominal type enters it.

Implementation caching uses StructuralHash. Definition sharing across
modules uses StructuralHash together with QualifiedKey.

### 2.3 InterfaceHash

The named public API identity of one export. It covers what an
importing module must agree with:

- the export name;
- the kind;
- the full structural signature, with class references by qualified
  name;
- the field defaults, the arm names, and the initializer signature
  that the checker reads.

An import slot pins the InterfaceHash of the export it names. The
build store uses InterfaceHash to decide which dependents rebuild.

InterfaceHash contains names by design. A rename moves it.

### 2.4 VerificationHash

The exact resolved input of the verifier. It answers one question: did
the verifier approve this exact representation?

The value covers the semantic region, the operation manifest digest,
and every resolved input the verifier reads. Section 9 defines the
membership rule and the constraint that governs when a name may leave
this hash.

## 3. The naming rule

Use this rule, and no shorter form of it:

> A declaration name never enters a structural definition hash. A name
> may enter an interface hash, a namespace hash, or a qualified key.

The shorter claim "no name in any hash" is wrong. Interface hashes
must contain names, because an importer agrees with a named API.

## 4. Referenced nominal identity

A structural hash alone cannot separate two signatures that name two
structurally identical classes.

Consider a function `f` with one parameter. One version names `Vec2`,
and the other names `Point`. The two classes have equal
StructuralHash values. The parameter encodes as a type digest, and
that digest reads the referenced class identity. Both versions
therefore produce equal function bytes and one StructuralHash. The
two functions also share a QualifiedKey. A merge rule that reads only
those two values merges two different functions.

The identity of a definition must therefore carry the nominal identity
of every class it references. Two designs are available:

1. **A nominal reference map beside the structural hash.** The
   structural hash stays free of every name. The map lists the
   QualifiedKey of each referenced class. A safe merge compares
   QualifiedKey, StructuralHash, and the map.
2. **A qualified reference inside the type digest.** A type digest
   names a referenced class by QualifiedKey instead of by structural
   identity. A safe merge compares QualifiedKey and StructuralHash
   alone.

Design 2 removes one side table, and the artifact and the interface
carry one value fewer. It stays inside the rule of section 3, because
the prohibition covers the **own** name of a declaration, never a
referenced nominal identity.

**Decision: implement design 2.** Record the reason in the
implementation note. Reject design 1 only in the note, and state the
cost you measured. If design 2 fails a test that design 1 passes,
adopt design 1 and record the failing case.

## 5. The linker merge rule

The linker compares two definitions with this table. The table is
exhaustive, and the linker must implement all four rows.

| QualifiedKey | StructuralHash | Result |
| --- | --- | --- |
| same | same | merge into one definition |
| same | different | reject: conflicting implementations |
| different | same | keep distinct |
| different | different | keep distinct |

Consequences the tests must hold:

- `Vec2` and `Point` stay distinct, although their structures are
  equal.
- Every embedded copy of `core.Option` merges, because every copy
  carries the qualified key `core.Option` and one structure.
- A method body edit keeps the QualifiedKey and moves the
  StructuralHash.
- Two implementation versions of one qualified name never coexist in
  silence. The rejection names both providers and the rebuild.

The second row protects a defect this repository already met. A stale
cached module produced a program with a split core, and the fix added
the core source digest to the compile key. The second row rejects that
class of defect at the link step, on principle rather than by one key
field.

## 6. Order-invariant component labeling

Delete the canonical member order that sorts by name. Replace it with
structural refinement.

1. Give each member of a strongly connected component a first label.
   The label is the hash of the member bytes, with every reference
   inside the component replaced by one fixed placeholder.
2. Refine. The next label of a member is the hash of its current
   label plus the current labels of the members it references. Keep
   the references in their position order inside the member. Never
   sort references inside one member, because `f(g(x))` and `g(f(x))`
   differ.
3. Stop when the label partition stops refining. Cap the round count
   at the member count.
4. The final label is the StructuralHash of the member. The component
   hash is the hash of the sorted final labels.

Tarjan's algorithm keeps its job of finding components. The set of
components is a property of the graph. Component emission order stops
being visible in any hash, because a reference across components
already carries the final hash of its target.

This step runs before the verifier, on untrusted input. Measure the
cost on a wide hostile component. Exit as soon as the partition is
stable. Report the measured round count and the cost in the
implementation note.

## 7. Symmetric members

Structural refinement cannot always give each member a unique label.
Two mutually recursive definitions with equal bodies stay symmetric
through every round. This is a property of graph automorphism, not a
defect of the algorithm. No order-invariant algorithm separates them
without an external identity.

Symmetric members therefore share one StructuralHash. Their
QualifiedKey values keep them distinct wherever distinctness is
observable. The linker table of section 5 reads both values, so two
symmetric members with different qualified names never merge.

## 8. Slot resolution

The verifier reads names today through two paths. The core resolver
reads a class label. The class encoding holds selector names.

The linker must resolve every name into an explicit slot before the
verifier runs:

- class slots;
- selector slots;
- import slots;
- stable core role slots, which replace the label lookup of
  `corepin`.

The verifier then checks resolved slots and structures. The verifier
must not read a source name.

## 9. The verification hash and its ordering constraint

VerificationHash covers every resolved verifier input. Today that set
includes the definition names, because the core layout resolves by
label and the verifier reads the core layout.

**Constraint: keep the names inside VerificationHash until section 8
lands.** A name may leave VerificationHash only after the verifier
stops reading every name-derived value. An earlier removal restores
the cache defect an independent review found: a crafted rename moves
a core class hash, drops a core slot, and a cached load then admits
what an uncached load rejects.

The staging plan of section 12 enforces this order.

## 10. The three-stage build store

One content-addressed store holds three stages. Each stage keys on its
own input.

| Stage | Key | Value |
| --- | --- | --- |
| 1 | source bytes, root set, module path, dependency interface identities, versions | compiled module (`.lma`, `.lmi`) |
| 2 | the set of module content hashes of the program | linked artifact |
| 3 | VerificationHash of an artifact | the verification verdict and the resolved facts |

Stage 1 exists. Stage 2 is missing. Stage 3 exists and never hits,
because the loader builds a new cache for one load and drops it.

Stage 2 is the missing floor. Module compilation is incremental today,
and the link step plus the verification of the merged program run on
every build. Both costs grow with the whole program. A fully cached
rebuild of the three-module example measures 1.92 ms, and process
start dominates at that size. The shape is the problem, not the
current number.

Requirements:

- Stage 2 must store the linked artifact under the set of module
  content hashes. A rebuild with no source change must not link again.
- Stage 3 must persist beside stage 2, under the same directory. A
  verified artifact must not meet the verifier twice.
- Stage 3 must stay reachable for an artifact with no source, for
  example `lm run third-party.lma`. That is the only stage available
  for a foreign artifact.
- A rejected artifact must never enter stage 2 or stage 3.

## 11. The rule this design delivers

> Names control resolution and API compatibility. The verifier checks
> resolved structure, not source names.

State this rule in the implementation note. Do not state the stronger
claim that a rename never changes what the verifier checks. That claim
is false until section 8 lands.

## 12. Staging

Implement in this order. Any other order reopens the cache defect.

1. **Specification and naming only.** Split section 8.6 into the four
   terms. Rename the current class hash to StructuralHash. Define
   class identity as QualifiedKey. Correct section 3.7. No behavior
   changes in this step.
2. **The linker table.** Implement all four rows of section 5.
   Implement the referenced nominal identity of section 4.
3. **Order-invariant labeling.** Implement section 6 and section 7.
4. **Slot resolution.** Implement section 8. The verifier stops
   reading names.
5. **The verification hash.** Only now, remove the names from
   VerificationHash.

Section 10 (the build store) is independent of stages 1 to 5. Implement
it in parallel or after, as you prefer, but do not let it change an
identity rule.

## 13. Specification edits

- **Section 8.6** conflates four identities in one sentence: the class
  name, the field layout, the method signatures, the method bytecode,
  the parent requirement, and the generic arity. Split it into the
  four terms of section 2.
- **Section 3.7** states the canonical member order sorts by name, and
  it states a `may` bound for a rename inside a cyclic component.
  Section 6 deletes the name order. Replace both statements with the
  refinement rule, and delete the cyclic special case.
- Record every hash domain change and bump `COMPILER_ABI_VERSION`.
  Regenerate `core/pinned-core-defs.txt` with the ignored test
  `regenerate_core_pins`.

## 14. Facts measured on the current tree

Use these as the baseline. Re-measure and report each one.

- A rename never changes the semantic region. Four cases measured a
  byte-identical semantic section: a free function rename, a cyclic
  rename that keeps the order, a cyclic rename that changes the order,
  and a class rename. Names live in the export section, and the
  verifier never reads that section.
- A cyclic rename moves a definition hash only when the rename changes
  the name order. `even` to `evenx` and `even` to `aaa` moved no hash.
  `even` to `zzz` moved both member hashes.
- Definition sharing collapses three embedded core copies into one.
  The linked `examples/05-modules/app` program holds 27 classes. Three
  unmerged cores hold 78.
- A fully cached rebuild of that program measures 1.92 ms.
- The verified-code cache has no production hit. `lm check` and
  `lm build` call `lm_verify::verify_module` directly. `lm run` on an
  artifact builds one cache, loads once, and drops it.
- `module_identity` costs 138 us on a 6.9 KiB module, and the full
  verifier costs 159 us. Identity runs on every load.
