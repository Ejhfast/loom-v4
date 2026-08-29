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

A class StructuralHash covers no construction function. A constructor
is a function value with its own StructuralHash, and section 5.2 ties
it to the class through the binding `<class key>.<new>`. A field
default is inlined into that constructor, and an `init` body is
reached through it, so both reach the constructor identity and no
class identity.

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
- the declared type and literal value of a constant.

An import slot pins the InterfaceHash of the export it names.

A constant pin has no runtime definition.

The build store uses InterfaceHash to decide which dependents rebuild.

InterfaceHash contains names by design. A rename moves it.

### 2.4 VerificationHash

The exact resolved input of the verifier. It answers one question: did
the verifier approve this exact representation?

The value covers the semantic region, the operation manifest digest,
and every resolved input the verifier reads. Section 9 defines the
membership rule and the constraint that governs when a name may leave
this hash.

### 2.5 Hash algorithm

All 256-bit content identities use BLAKE3-256.

This rule covers structural, interface, verification, artifact, and build-cache hashes.

An algorithm change moves all affected identities.

It must increment `ABI_VERSION`, `INTRINSIC_ABI_VERSION`, and `COMPILER_ABI_VERSION`.

## 3. The naming rule

Use this rule, and no shorter form of it:

> A declaration name never enters a structural definition hash. A name
> may enter an interface hash, a namespace hash, a qualified key, or a
> function binding.

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

## 5. The linker merge rules

### 5.0 The three rules

A name is a temporary reference to an identity. A name sits on the
left of the arrow. It is a binding that points at an identity, and it
is never a component of that identity:

```text
Source binding     "A"   -> NominalTypeId
Method selector    "foo" -> SelectorId
Implementation     SelectorId -> MethodHash
```

Three rules follow, and the rest of this section applies them:

> **QualifiedKey is the nominal identity of a class.**
>
> **A function value is identified by StructuralHash.**
>
> **A named function binding maps a qualified name to a function
> value.**

A function therefore has two layers:

```text
FunctionBinding { qualified_name, code_index }
FunctionCode    { structural_hash, signature, body }
```

Several bindings may point at one code object. A binding key never
enters a StructuralHash.

### 5.1 Class identity and class-slot merging

The linker compares two classes with this table. The table is
exhaustive, and the linker must implement all four rows.

| QualifiedKey | StructuralHash | Result |
| --- | --- | --- |
| same | same | merge into one class slot |
| same | different | reject: conflicting implementations |
| different | same | keep both class slots |
| different | different | keep both class slots |

Row 3 must keep the definitions distinct, because two class keys need
two runtime class slots.

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

### 5.2 Function binding resolution and code sharing

A named function binding maps a qualified name to a function value.
The linker compares the binding key and the StructuralHash of the
function each key names:

| Binding key | StructuralHash | Result |
| --- | --- | --- |
| same | same | share the binding and the code |
| same | different | reject: conflicting providers |
| different | same | keep both bindings, share the code |
| different | different | keep both bindings and both code objects |

The two tables differ in row 3 only, so one table cannot serve both
kinds. Two class keys need two class slots; two function names may
share one code object.

The binding keys:

- a free function takes `<module path>.<name>`;
- a method and an `init` take `<class key>.<name>`;
- a generated construction function takes `<class key>.<new>`;
- a closure body and the entry take no binding.

A class member takes the class key as its root, never the module path,
so the embedded core copy of every module binds one set of names.

Consequences the tests must hold:

- `a.first` and `b.second` stay two distinct bindings of one shared
  code object, and every report names both.
- Identical bodies share one code object. The generated constructor
  stubs of the abstract enum parents are all `unit; return`, and they
  share one code object under many binding keys.
- Function equality stays content-based, and imports still resolve
  through qualified names.
- A class StructuralHash covers no construction function. Row 2 of
  this table is therefore the only rule that separates two providers
  of one class key whose field defaults or `init` bodies differ.

The linker also proves that a module derives its constructor bindings
from its class keys: every class a module defines declares the binding
`<class key>.<new>`. A module that names a class one way and its
constructor another way is not self-consistent and rejects.

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

Bound the work twice. One budget bounds one component. A second budget
bounds the whole module, because a module holds many components and
their cost adds up: a budget on one component alone lets a crafted
module reach any cost through many components that each stay inside
it. A component or a module past its budget rejects with a clear
diagnostic that names which budget it passed. Report both measured
bounds in the implementation note.

## 7. Members that refinement never separates

Structural refinement cannot always give each member a unique label.
Two mutually recursive definitions with equal bodies keep one label
through every round.

Name the property exactly. Section 6 is 1-dimensional colour
refinement. Its stable partition is **bisimulation**: two members keep
one label exactly when they are bisimilar. Bisimulation is coarser
than isomorphism in general, so the rule may give one label to two
members an isomorphism test separates. An earlier version of this
document claimed graph automorphism and claimed that no
order-invariant rule separates such members. Both claims were too
strong, and this section replaces them.

One label stays sound. A member is a deterministic system with
ordered successors, so two bisimilar members have identical
unfoldings. Two definitions with identical unfoldings compute the same
thing, and merging them changes no program behavior.

Members with one label therefore share one StructuralHash. Their
QualifiedKey values keep them distinct wherever distinctness is
observable. The class table of section 5.1 reads both values, so two
such classes with different qualified names never merge.

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

VerificationHash covers every resolved verifier input. This set includes
the semantic region and the operation manifest. An artifact declares its
core role table. The verifier proves every filled slot.

Definition names remain in the export section. Published slot keys live
in the semantic region. The compiler derives these keys from binding
names and contracts. A source binding rename therefore moves
VerificationHash. A selector rename also moves VerificationHash.

The ordering constraint below is met. It stays recorded, because it
governs any future change that moves a name back into the verifier.

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
- **Section 3.7** must state the three rules of section 5.0, and it
  must define a named function binding. **Section 3.6** must carry
  both merge tables of section 5. The earlier text stated one table
  over "definitions" and one sentence that a function carries no key.
  The two disagreed, and section 5.2 settles the disagreement.
- **Section 8.6** must state that a class StructuralHash covers no
  construction function.
- Record every hash domain change and bump `COMPILER_ABI_VERSION`.
  Regenerate `core/pinned-core-defs.txt` with the ignored test
  `regenerate_core_pins`.

## 14. Facts measured on the current tree

Use these as the baseline. Re-measure and report each one.

- An export-label edit never changes the semantic region. A source
  binding rename changes its published slot key. This key lives in the
  semantic region.
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

## 15. The single-file module path

A module path comes from a package: the manifest supplies the root and
the directory tree supplies the rest. One source file outside a
package has neither.

> **A single source file has no module path.**

Every single-file command applies this one rule, so one file gives one
set of qualified keys, one semantic hash, and one admission answer. A
file name never becomes a module path: a file name may hold characters
a module name cannot, and it may be `core`, which the core image
reserves.
