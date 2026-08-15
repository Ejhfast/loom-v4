# Week 6 Status

This note records the week 6 work. It covers:

- what landed;
- the identity decision, its justification, and the rejected
  alternatives;
- the two caches and their trust boundaries;
- the simplifications inside the slice;
- the changed tests and the deferred work.

Bytecode format version 7 carries the import table and the export
table. The compiler ABI version is 2. The core image pin moved to
`16eda1db90588c74321eb9352a84a5a1a4c982d3570fa83d2ef2aa76933f252f`
and `core/pinned-core-defs.txt` holds the twenty regenerated core
definition hashes.

## Landed

### The package layer

The new crate `lm-compiler` holds the developer loop, above
`lm-verify` and below `lm-cli`:

- `manifest` parses `lm.package`. The accepted grammar is a strict
  hand-written TOML subset: blank lines, `#` comments, the two table
  headers `[package]` and `[dependencies]`, `key = "string"` pairs,
  and the inline dependency table `{ path = "..." }`. There is no
  array, no nested table, no escape sequence, and no number or
  boolean literal. Every rejection names the line and the fix.
- `graph` turns `src/**/*.lm` into the module tree. The file
  `src/geometry/shapes.lm` is `geometry.shapes` inside its package
  and `<package>.geometry.shapes` across packages. The layer also
  loads the dependency closure and orders the packages and the
  modules of each package. A cycle rejects at either level.
- `env` holds the explicit typed `CompileEnv` and `LinkEnv` builders.
  Both freeze before use, so no build step mutates an environment
  another step reads.
- `module` compiles one module against dependency interfaces only.
- `link` merges the modules of one program into one closed artifact.
- `cache` is the content-addressed build directory.
- `build` runs the loop; `scaffold` writes a new package.

The module path across packages carries the package name of the
manifest, never the dependency key. Renaming a key changes the local
root name only, which is the documented fix for a name collision. Two
packages with one name in one graph reject.

### `use`, import slots, and the compile environment

A `use` path starts at a root name. The root set of one module is the
dependency keys of its manifest, this package's own top-level
modules, `std`, and `sys`. `use std.*` rejects with the message that
the standard library does not ship yet. An unknown root lists the
roots that exist.

A `use` of a module binds the module: every export resolves under the
bound name (`matrix.Matrix(2, 3)`, `matrix.describe(m)`). A `use` of
one export binds the short name. A bound short name that the module
already defines is an error with the module form as the stated fix.
A type may now carry a dotted name (`matrix.Matrix`), which the
parser accepts and the checker resolves through the `use` binding.

The checker materializes an import in two phases, because it fills
the class table in index order:

- phase A reserves the class indices over the transitive closure of
  the classes an interface names. It runs before any signature
  resolves, so a user signature may name an imported type;
- phase B fills the declarations, creates the imported functions, and
  records the slots. It runs after the core classes land, so an
  imported signature may name a core type.

An imported definition lowers to a signature with no body. Every one
takes an import slot: `Class`, `Ctor` (the construction function),
`Method`, or `Func`. The slot names the providing module, the export
name, the kind, and the pinned interface hash. The verifier proves
three rules:

- the map from slot to definition is injective;
- an imported function carries no body, no capture, and no extra
  local slot;
- a class and its methods share one import state.

The loader admits a module only with an empty import table, so an
unfulfilled slot never executes.

### The interface (`.lmi` version 2)

An interface entry holds the export name, the kind, the full
structural signature, the interface hash, and the definition hash.
A signature names a class by qualified name, for example
`mathlib.matrix.Matrix`. The empty module path names a core class.
The file is therefore position-independent, and a compiler needs
nothing else to check an importing module.

The entry carries what the checker needs and the bytecode module does
not: the declared parameter names, the fields that carry a default,
the `init` signature, the arm names, and the own-field start of the
layout.

The decoder caps the type nesting depth at 32. It also bounds every
length against the remaining input. A crafted file therefore rejects
instead of growing the host stack. `lm inspect file.lmi` dumps the
whole interface, methods included.

### The linker

`lm_compiler::link` merges the modules of one program into a single
artifact with an empty import table. It installs no global name,
performs no host operation, and reads no file. Three rules do the
work:

- **slot resolution**: a slot resolves by module path and export
  name, against the exports the provider registered. A provider whose
  interface hash differs from the pin rejects, and the message names
  the rebuild;
- **definition sharing**: two definitions with one definition hash
  are one definition in the merged program;
- **relocation**: every module-global index is renumbered, and
  strings, types, selectors, and applications intern by content.

Definition sharing is what makes the core one core. Each module still
embeds its own copy of the core image, and the copies carry identical
definition hashes, so the merge keeps one. The measured effect: the
linked `examples/05-modules/app` program holds 27 classes, which is
the 26-class core plus `Matrix`; three unmerged cores would hold 78.
Sharing is not an optimization here, it is the semantics: an
`Option[Int]` a dependency builds must match a `case` in the
importer. A merge failure is never silent. The merged artifact meets
the whole verifier before it runs, and two distinct `Option.Some`
classes fail the type rules of the call site.

The link order walks the import graph from the entry module, so a
module no slot names never reaches the program. A library may
therefore hold modules a given program does not pay for.

The shared-definition rule compares the relocated definitions and
rejects a hash that covers two different definitions. Function names
stay out of that comparison, because function identity excludes the
name. The generated constructor stubs of the abstract enum parents
are all `unit; return`, and they share one definition by design.

### The build directory and the rebuild gate

`build/cache/modules/<key>.lma` and `.lmi` hold one compiled module.
The key covers:

- the container format version, the compiler ABI version, the
  verifier version, and the operation manifest digest;
- the digest of the core sources, because every module embeds the
  core image and a core edit need not move the ABI version;
- the module path and the entry flag;
- the exact source bytes and the root set;
- the **interface identity** of every visible module.

An interface identity covers the export names, the kinds, and the
interface hashes. It covers no definition hash.

The visible set is every module the build already produced, not only
the modules one file may name. The key is therefore coarser than it
must be: an interface change in an unrelated package rebuilds this
module too. The gate the week needs still holds, and the coarseness
is sound in the safe direction.

That is the rebuild gate. An edit to an exported body moves the
definition hash and the module semantic hash. It moves no interface
hash, so the key of every dependent module holds and only the edited
module recompiles. The program still relinks, because the program
contains the new body. An edit to an exported signature moves the
interface hash and recompiles the dependents. A stale call then
becomes an ordinary type error.

`lm build`, `lm run`, and `lm new` work on packages:

```text
$ lm build examples/05-modules/app
built  mathlib.matrix  11822e8c013f
built  app.greeting  86850c4a059e
built  app.main  9fa072abca88
linked app  sem=0b91ae8fa454 container=879c429db5f4
  examples/05-modules/app/build/debug/app.lma
$ lm run examples/05-modules/app --allow Io.Print
Hello Ada!
2x3 has 6 cells
$ lm run examples/05-modules/app/build/debug/app.lma --allow Io.Print
Hello Ada!
2x3 has 6 cells
```

A second build reports `cached` for every module. The report line per
module replaces the per-package sketch in the build order. A module
is the unit the cache keys on, and the unit a user edits.

The build directory of a package sits at the package root, not at the
current directory. Two builds from two directories then share one
cache and write one program. `lm build` and `lm run` also default to
the current directory, so both work from anywhere inside a package.
The single-file `lm build file.lm` keeps the week-5 rule and writes
`build/debug` beside the current directory.

## The identity decision

Week 5 left the class-identity question open. Week 6 had to settle
it, because import slots are the first real consumer of the hashes.
The decision, in one line: **class identity is nominal, function
identity stays anonymous, and a second identity, the interface hash,
carries what import slots pin.**

### What moved

1. `class_bytes` writes the class name first. Two classes with
   different names are different definitions, whatever their shape.
2. A new interface hash (`lm-iface-v1`) covers the export name, the
   kind, and the full structural signature. It adds the compiler ABI
   version and the operation manifest digest. It covers no body.
3. The core pin resolves by the pair `(label, hash)`, and the lowest
   matching class index wins.
4. Every definition name enters the verification hash.

### Why nominal class identity

The gap week 5 recorded is reachable from idiomatic code. The probe
test in `crates/lm-testkit/tests/week6_identity.rs` compiles

```lm
enum MyErr
  Failed(message: String)
  def message(self): String ... end
end
```

and it failed before the change: `MyErr` shared a definition hash
with the core `IoError`, and `MyErr.Failed` with `IoError.Failed`.
The only hash-only lookup in the system, `corepin`, then chose
between them by emission order.

The decisive consumer is not the core pin, though. It is the linker.
The linker merges two definitions with one hash into one definition,
and a merged class keeps one name. Under structural identity,
`mathlib.Vec2 {x: Int, y: Int}` and `app.Point {x: Int, y: Int}`
would merge into one class. The name is observable: `--show-result`,
the value display, and every future reflective surface print it.
Nominal identity removes that class of defect at the root, instead of
guarding one lookup.

Function identity stays anonymous, with one week-5 exception. A
function hash never covers its own name, a caller references a callee
by hash, and the linker compares bodies without names. Inside a
cyclic component the canonical member order sorts by name. A rename
there moves the member ordinals, and therefore the member hashes of
that component. Week 5 recorded the exception and the specification did
not; specification 3.7 states both halves now.

### Why the interface hash exists, and why the four-way split does not

The external review proposed four identities: `NominalTypeId`,
`InterfaceHash`, `ClassCodeHash`, and `MethodHash`. Week 6 adopts two
of the four boundaries and folds the rest in:

- **InterfaceHash versus definition hash: adopted.** It has a real
  consumer. An import slot pins the interface hash, so an edit to an
  exported body rebuilds no dependent. Two tests hold the property:
  `a_body_edit_moves_the_definition_hash_and_no_interface_hash` at
  the hash level, and `a_body_edit_rebuilds_only_the_edited_package`
  at the build level.
- **NominalTypeId as a separate value: rejected.** The name is folded
  into both hashes instead. A third identity needs a third bump rule
  and a third place to keep in sync. No consumer needs a name-only
  identity. The linker needs nominal *plus* structural, and the type
  checker never compares hashes: it works on class indices inside one
  module.
- **ClassCodeHash as a separate value: rejected.** The class
  definition hash already is the implementation identity. It covers
  the fields, the selector names, and the implementing function
  identities. A second hash split out of it adds a bump rule with no
  consumer. The only user of the implementation identity is the
  linker, and the linker wants the whole class.
- **MethodHash: already present.** A method takes part in the class
  identity as one pair: the selector name and the implementing
  function identity. That function carries its own definition hash.
  A method body hash never serves as selector identity. The property
  is unchanged and now normative in specification 3.7.

The interface hash is deliberately cheap. It hashes the bytes the
`.lmi` publishes for one export, with class references by qualified
name. It does not repeat the strongly-connected-component machinery
in a second domain. That is sound, because every referenced
definition is separately pinned. A module that materializes `Matrix`
also materializes every class `Matrix` names. Each of them takes its
own slot with its own pin. A drift two levels deep therefore fails
the pin of the definition that drifted.

### Why the qualified name is inside the interface hash

An interface signature names a class by module path plus name. The
interface hash of an export therefore follows the module path
whenever the signature names a class of that module. `def add(a: Int,
b: Int): Int` names no class, so two modules publish one interface
hash for it. That is safe: the linker resolves a slot by module path
and export name first, and compares the hash after. Moving a module
rebuilds its dependents, and the importing `use` line changed too.
Definition hashes stay location-independent, so content identity is
unaffected.

### What the decision does not fix

A definition hash is still not injective over source programs. Two
classes with one name and one shape share a hash, and no rule makes
class names unique inside a module. The specification states this
now. It requires a deterministic tie rule at every lookup that must
choose. `corepin` states one: the lowest index wins. The choice is
unobservable, because such classes are the same definition.

### The verification-hash coupling

The core layout is a verifier input, and identity computes it, so
every input of identity must fix the verified-code cache key.
Identity reads two kinds of name:

- a class name, because class identity is nominal;
- a function name, because the canonical member order of a component
  sorts its members by name. The member ordinal enters every member
  hash of that component.

Both are in `verification_hash` now. The second one is a week-5 rule
that the week-5 note recorded and the specification did not: a rename
inside a cycle moves the component hashes. An independent review
turned it into an attack. A crafted rename of the core function
`Option.is_some` drops the `Option` slot from the layout. The
semantic region does not move, so the key held. The uncached load
then rejected the module, and a cached load admitted it.
`week6_names.rs` replays the attack.

That closure has a second effect. The key now fixes every input of
`module_identity` as well, so a cache hit may replay the identity
instead of recomputing it. The cache stores the definition hashes and
the core layout beside the admission, and `cache.identities` proves
the skip.

The loaded module still exposes no module semantic hash. The hash
covers the export table, which the key does not, so a replayed
semantic hash could be stale. `LoadedModule` exposes `class_hash`
and `func_hash` instead.

The cost of the name coverage is one cache miss per rename. That is
the right trade: a rename already rebuilds the module in the build
cache, and the admission invariant is worth more than one hit. The
principled alternative is a content-ordered member rule inside a
component, which would remove the function names from identity
entirely. It needs a canonical order over isomorphic members, which
is a graph-labeling problem, and it moves every cyclic definition
hash. It stays deferred with that reason.

### The identity load path

The intra-component lookups of `identity.rs` are maps now, not linear
scans. Week 5 measured the cost as the number of intra-component
references times the component member count. A release-build
measurement over one dense component with eight references per member
shows the term is gone:

```text
dense-200:  61 KiB identity 487us
dense-400: 117 KiB identity 874us
dense-800: 228 KiB identity 1.41ms
```

Four times the members cost 2.9 times the work, which follows the
artifact size and not the square of the member count. This matters
because identity runs on untrusted bytes before the verifier.

## The caches and the trust boundary

Two caches answer two questions, and neither key stands in for the
other.

**The verified-code cache** (`lm-vm`, in process) answers "did the
verifier admit these exact bytes before?". The key is the
verification hash plus the compiler ABI version plus the verifier
version. The verification hash covers the semantic region with every
index preserved, the operation manifest digest, and the class names.
The boundary, restated for week 6:

- the loader computes the key from the decoded content on every load;
  no hash stored in an artifact enters it, and the container stores
  no hash at all;
- the key fixes every verifier input, so a hit skips every verifier
  pass;
- the key fixes every identity input too, so a hit replays the
  definition hashes and the core layout;
- the import table is inside the semantic region. An added slot or a
  moved pin therefore misses the cache, and meets the verifier and
  the loader rule;
- a rejected module never enters the cache;
- the remaining assumption is SHA-256 collision resistance.

The sweep test `a_cached_load_and_an_uncached_load_always_agree` is
the durable invariant: for any byte stream, a cached load and an
uncached load must agree on admission. Week 6 extends it with two
import cases.

**The build cache** (`lm-compiler`, on disk) answers "must the
compiler run again?". A cached entry is not trusted content: it
decodes through the ordinary decoder, and a damaged file is a miss,
not a trust hole. Nothing in the build directory reaches the VM
without the linker and the verifier: the program artifact is verified
at link time and again at load time.

The linker is not a trust boundary either. A wrong pin, a wrong
resolution, or a crafted artifact can only produce a merged module
that the verifier then rejects. The pin check exists to give a
precise error before that, not to keep the program safe.

A per-module artifact is never verified alone on a cache hit, and it
never needs to be: it does not execute. The merged program is
verified at link time and again at load time. The second run is a
deliberate duplication: the link step wants a precise error, and the
loader never trusts an input, whatever produced it.

The verified-code cache still has no production consumer inside one
`lm` invocation, because the tool loads one program once. It is the
in-process cache a multi-load path needs, and the build cache is what
makes the developer loop fast.

## Simplifications inside the slice

- A `use` of a module imports the whole export set of that module.
  Unused slots stay in the artifact. Per-definition pruning is
  deferred; the cost is artifact size, not correctness, because the
  pins are interface-level.
- A fully qualified reference without a `use` line
  (`mathlib.matrix.Matrix`) is not accepted. The `use` line comes
  first. `docs/specs/packages.md` section 9 records it.
- An enum arm of a module-aliased enum has no qualified constructor
  form (`matrix.Shape.Dot(1)`). Bind the enum directly
  (`use mathlib.matrix.Shape`) and the unqualified arm names work.
- A class cannot inherit an imported class. An imported declaration
  carries no body, so a subclass cannot reach the `init` of its
  parent, and week 6 defines no slot kind for it. The diagnostic
  names the field alternative.
- `std` does not exist yet, so `use std.*` rejects with that message.
- The `LinkedEntry` handle is a Rust-side approximation of the typed
  `LinkedEntry[A,R]` of specification 3.6: it exposes the entry
  result type and row and checks them against an expected pair. The
  language-level `CompileEnv.bind` of a frozen value, `Artifact.link`
  inside the language, and `DynValue` stay with the reified compiler
  in week 13. The week-6 test
  `the_typed_environments_compile_link_and_run_by_hand` drives the
  Rust API end to end instead.
- The build directory of a package is `build/` at the package root.
  Only the single-file `lm build file.lm` keeps the week-5 rule and
  writes beside the current directory.
- Every module of a program links into the artifact, including the
  empty entry function of a library module. It is dead code of two
  instructions.
- `lm check` still takes one file. Checking a whole package is
  `lm build`.
- The interface carries the full export set of a module. A visibility
  modifier does not exist in version 0.2 (specification 3.1).

## Changed tests

- `week5_identity.rs`: the sweep case `class twin` now records that
  the mutation **moves** the module hash. Class identity is nominal,
  so `New A` retargeted to a structurally identical `B` is a
  different program. The admission invariant is the durable part of
  the test and is unchanged; the per-case hash expectation became
  explicit data. Two import cases join the sweep.
- `week5_identity.rs`: `a_rename_does_not_move_the_verification_hash`
  became `a_rename_moves_the_verification_hash`. The names are
  identity inputs, and identity feeds the core layout, which the
  verifier reads, so the key must cover them. The test also proves
  the semantic region does not move, which is the part the old name
  was really about.
- `week5_identity.rs`: the two interface tests moved to
  `week6_interface.rs` and were rewritten for the structural format.
  `building_twice_is_byte_identical` dropped its interface half,
  which the new file covers.
- `week5.rs`: `use_rejects_non_fixed_paths` expected the wording
  "module imports arrive with packages". A module import now needs a
  package, so the message names that fix. The test asserts the new
  wording.
- `core_image.rs` and `fuzz.rs`: the pinned core files and the fuzz
  corpus regenerated for compiler ABI version 2 and format version 7.
  Expected churn, no expectation weakened.
- `examples/01-basics/factorial.lm` moved to semantic
  `5e314b92102931ca85173c7ba84c90dc4449f4e2817eadc5da7e0ccd70f431a0`
  and container
  `e7d49b91c5c08e2f6c4a285e7d5adf7eeb19d61ba9b5d3e4d73d7bff3999af8e`.
  Both moves are deliberate: the identity change moved the semantic
  hash, and the export table in the export section moved the
  container bytes. No test pins those values; the week-5 note records
  the old pair.
- A module with import slots also carries the pinned interface
  hashes, so its semantic hash follows the interface encoding. The
  linked program has no slot, so its hashes do not.

## New tests

`week6.rs` (31 cases) covers the build loop end to end on real
package trees:

- the two-package workspace, the cache hit, and the two rebuild
  gates;
- the stale caller, the damaged cache entry, the stale-pin link
  rejection, and the crafted export table;
- the closed program artifact, the shared core, the unused module,
  and program determinism across two build directories;
- the imported enum, the imported generics, the imported mutable
  method, the imported effect parameter, and the transitive type;
- the inheritance rejection, the self-import cycle, the symbolic
  link, and authority;
- the scaffold, a build from a subdirectory, and the module tree from
  directories;
- the dependency-name collision, the unknown root, the library
  package, and the manifest subset;
- the hand-driven typed environments.

`week6_interface.rs` (7 cases) covers the structural signature, the
two hashes, the readable dump, determinism, every truncation and bad
tag, and the depth cap. `week6_imports.rs` (7 cases) covers the
import slot inside the artifact. `week6_names.rs` (3 cases) covers
the definition names as verifier inputs and the core family rule.
`week6_identity.rs` (3 cases) covers
nominal identity, the core slot, and the identity replay on a cache
hit. `fuzz.rs` gains the interface decoder and the manifest parser.
`tests/ui/` gains two `use` diagnostics. `bench_smoke.rs` times the
build, the link, and the cached load path.

## The self-review pass

A pass over the import surface with probe tests found three defects.
Each probe failed first and passes now.

- **The imported parent never reached the type store.** The
  materializer set the parent inside the checker class record only.
  `lm_types::TypeStore` answers every subtype question, and it held
  no parent. An imported enum arm was therefore not a subtype of its
  family, and `name(Dot())` rejected with a type mismatch. Phase A
  now calls `set_class_parent` as soon as it reserves the parent.
- **A field default that named an imported class panicked the
  checker.** Phase B ran after the default pass. The default
  therefore saw a class index the checker record did not hold yet.
  Phase B now runs before every body and every default. The default
  table takes the imported entries last, so the index alignment
  holds.
- **Inheritance from an imported class misled the user.** The old
  message asked for the parent declaration earlier in the file. No
  edit can do that. The rule is explicit now: a class cannot inherit
  an imported class. The message names the field alternative.

The `lm run <package>` report moved to standard error in the same
pass, so the program output stays clean on standard output.

## The independent review pass

An independent review of the week found six items. Four are fixed
here, one was already fixed, and one is a corrected sentence.

- **The verified-code cache admitted what an uncached load
  rejected.** The section above records the cause and the fix: every
  definition name enters `verification_hash`.
- **The compile key omitted the core image.** A core edit kept the
  dependents cached and produced a program with two `Pair` classes.
  The fix landed before the review ended: the key covers
  `lm_hir::core_source_digest()`.
- **A resolved core family could lose an arm.** The verifier proved
  the parent slot, and the runtime allocated through the arm slots.
  A crafted layout could therefore reach a slot the layout did not
  hold, and panic the host process. The verifier rejects a family
  that resolves without every arm now. No rename reaches the case, so
  the gap was latent, and the rule closes it by construction. The
  verifier version moves to 2.
- **The module walk recursed and followed symbolic links.** A link
  cycle produced forty copies of every module and a confusing
  diagnostic. The walk keeps an explicit stack now, and a symbolic
  link inside `src` rejects.
- **One signature encoding was ambiguous.** The marker and name
  vectors of an interface signature carried no count. Two signatures
  could therefore share one encoding, and one interface hash. The
  compiler never produced such a pair, because it builds all three
  vectors from one signature. Both vectors carry their count now, and
  the decoder forces the three counts equal.
- **One note sentence over-claimed.** An interface hash follows the
  module path only when the signature names a class of that module.
  The section above states the condition.

## Open questions

These need a decision. They are not defects with an obvious fix.

### Rename invariance inside a cyclic component

Specification 3.7 says a rename inside a cyclic component moves every
member hash of that component. The implementation moves a hash only
when the rename changes the canonical member order. Measured on the
`even`/`odd` component:

```text
even -> evenx  even_moved=false  odd_moved=false
even -> aaa    even_moved=false  odd_moved=false
even -> zzz    even_moved=true   odd_moved=true
```

Do not read this as "the specification is wrong". The specification is
normative, so the question is which behavior Loom must promise. Four
positions:

1. **Keep the specification as a conservative bound.** State that a
   rename inside a cycle MAY move every member hash, and let the
   implementation invalidate less. This promises nothing about the
   ordering mechanism, so a future change to the canonical order stays
   legal. The current implementation already satisfies this reading.
2. **Describe the mechanism.** State the order rule exactly, as the
   implementation behaves. This is precise, but it commits the
   specification to sorting by name, and it makes the user-visible
   rule strange: `even` to `evenx` is free, and `even` to `zzz`
   rebuilds the component and everything downstream.
3. **Change the implementation to match the text.** Always invalidate
   the component on a rename inside it. This is predictable and costs
   cache hits.
4. **Remove the name from the canonical member order.** Order cyclic
   members by a stable generated ID instead. Then a rename never moves
   a definition hash, inside a cycle or outside one, and the
   specification gets one uniform rule with no special case. There is
   precedent: a closure inside a component with named definitions
   already orders by its generated ID. The cost is one lost
   separation: the component hash no longer tells apart members that
   differ only by name. That may be acceptable, because a function
   hash already excludes its own name.

Position 4 gives users the simplest story and deletes a special case.
Position 2 is the weakest: it turns a guarantee into an implementation
description and makes rebuild cost depend on alphabetical order.

Week 6 takes position 1 for the text and leaves the mechanism open.
Section 3.7 says a rename inside a cyclic component **may** move every
member hash. The word states a bound, not a mechanism, so a later
position 3 or position 4 stays legal. The implementation note beside
it records position 4 and its cost. The cache key covers the names
either way, because the bound is what a host must assume.

### The `mut` flag vectors inside the semantic section

`encode_semantic` and the `BcType::Fn` arm of `encode_type` write the
`mut` flags with no count, and take the count from the parameter
table. `semantic_section` is the main input of `verification_hash`, so
the injectivity of the cache key rests on it. `decode_semantic` reads
`params.len()` flags, so a module that arrives as bytes is always
consistent. The exposure is `lm_vm::load_cached(module)` with a
hand-built `Module`: on a hit the loader replays the cached facts and
skips `preflight`, which is the only rule that rejects
`param_muts.len() != params.len()`. The independent review could not
build a concrete collision, because every field after the flags is
fixed-width or count-prefixed.

Three fixes, which are not equivalent:

1. Write the counts in the encoder. This closes the ambiguity for
   every reader of `semantic_section`, but it changes artifact bytes,
   so it needs a format version bump and a core re-pin.
2. Add the counts to `verification_hash` only. This changes no
   artifact, and closes the key injectivity alone.
3. Run `preflight` on every load. This rejects the malformed module
   instead of only separating it, and it restores the rule that a
   cached load and an uncached load agree.

Week 6 takes fix 2. The property at risk is the injectivity of the
cache key, and fix 2 closes exactly that. Fix 1 moves every artifact
byte for a defect no byte stream can reach, because the decoder
always writes one marker per parameter. Fix 3 costs a full preflight
per load, which is most of what the identity replay saves. The
question stays open only as a format question: an encoder that writes
its own counts is the cleaner shape when the format version moves
next.

## Deferred work

- Per-definition pruning of unused import slots.
- A fully qualified reference without a `use` line, and the qualified
  arm form through a module alias.
- The language-level compile and link API (`sys.compiler.compile`,
  `Artifact.link`, `DynValue`) with week 13.
- `std` and `lm test` with weeks 11 and 12.
- Definition-level verified-code caching; module-level is still the
  bar.
- Cross-artifact sharing of the core image. Every unlinked module
  still embeds a copy, and the linker merges the copies; a shared
  core image on disk would keep the module artifacts smaller.
- A binary row encoding inside artifacts.
- Debug content (source maps) and a container hash that moves
  independently of the semantic hashes.
- A parallel build loop. The loop is sequential and single-threaded.
- A content-ordered member rule for a cyclic component, which would
  remove the function names from identity.
- A compiler build identity inside the compile key. The key covers
  the format version, the compiler ABI version, the verifier version,
  the manifest digest, and the core digest. A checker or lowering
  change that moves the generated code must move the compiler ABI
  version, which is the bump rule; the key does not enforce it.
- CI workflow files, Miri, `cargo-fuzz` targets, and committed
  benchmark distributions stay deferred as before.
- Week 13 keeps the recorded constraint: class values must not
  compare by bare definition hash, because a definition hash is not
  injective over names.

## The identity, linking, and build-store rework

This section records the work that implements
`docs/specs/identity-and-linking.md`. It replaces the identity
decision above wherever the two disagree.

Bytecode format version 8 carries the class qualified keys, the core
role table, and a self-describing `mut` marker encoding. The compiler
ABI version is 3 and the verifier version is 3. The core image pin
moved to
`df377f94ffbf8cc9e4a6358f02146611448455e228bc029e0181a22f8dc68657`
and `core/pinned-core-defs.txt` holds the twenty regenerated core
structural hashes.

### The rule this design delivers

> Names control resolution and API compatibility. The verifier checks
> resolved structure, not source names.

Both halves hold now. The verifier reads resolved slots and
structures. It reads no class name, no function name, and no
qualified key.

### The four identities

- **QualifiedKey**: the fully qualified declaration path of a class,
  for example `mathlib.matrix.Matrix`. The core image owns the
  reserved path `core`, so a core class carries `core.Option`. A
  module path of exactly `core` rejects with diagnostic E0290.
- **StructuralHash**: the name-free content identity. A declaration
  name never enters its own structural hash.
- **InterfaceHash**: unchanged. It contains names by design.
- **VerificationHash**: the semantic region plus the operation
  manifest digest, and nothing else.

`BcClass` carries the key beside the name. Both live in the export
section, outside the semantic region.

### Design 2 of specification section 4, and why

A structural hash alone cannot separate two signatures that name two
structurally identical classes. Design 1 keeps a nominal reference map
beside the hash. Design 2 names a referenced class by QualifiedKey
inside the type digest. **Design 2 landed.**

The measured case is in `identity_linking.rs`:
`a_referenced_qualified_key_separates_two_equal_signatures` compiles
`class Vec2 {x, y}` and `class Point {x, y}` with `f(v: Vec2)` and
`g(p: Point)`. The two classes share one structural hash, and the two
functions do not.

Design 1 rejected, with its measured cost:

- it adds one value to the artifact, one value to the interface, and
  one comparison to the linker merge rule, for exactly the separation
  design 2 already gives;
- it keeps class references inside the reference graph, so a class and
  its methods stay in one strongly connected component. Under design 2
  no edge points at a class, so every class is a singleton component
  and every cycle is function-only. The refinement of section 6 then
  runs over functions alone;
- the enum family cycle disappears with it. Under design 1 the family
  parent and its arms form one component, which is what made the old
  name-ordered member rule load-bearing.

Design 2 has one cost, and it is real: a structural hash is no longer
location-independent. Moving a module moves the qualified key of every
class it declares, and every hash that names one of those classes. The
week-6 sentence "definition hashes stay location-independent" is false
now. The build cache already keys on the module path, so nothing goes
stale; the cost is one rebuild on a move, and the `use` line changed
too.

### One narrowing of specification section 5: functions carry no key

Specification section 5 states the merge table over "definitions". The
implementation gives a QualifiedKey to a **class** and to no function.
A function merges on its structural hash alone.

The reason is the principle the specification follows: a source
binding names a nominal type, a selector names a method, and an
implementation is content. A class is a nominal type (5.3, 8.6); a
function is not. The consequences of specification section 5 all read
as class consequences, and each one holds:

- `mathlib.Vec2` and `app.Point` stay distinct
  (`two_equal_shapes_with_two_keys_stay_distinct`);
- every embedded copy of `core.Option` merges
  (`every_embedded_core_copy_merges`);
- a method body edit keeps the QualifiedKey and moves the
  StructuralHash of the class that answers the selector
  (`a_method_body_edit_keeps_the_key_and_moves_the_hash`);
- two implementation versions of one qualified name reject with both
  providers named (`two_versions_of_one_qualified_key_reject`).

Row 2 still catches a drifted core method, because a class structural
hash covers the identity of every implementing function. The split
core the week-6 review found therefore rejects at the class level.

The cost of a function key would be real: the generated constructor
stubs of the abstract enum parents are all `unit; return`, and every
structurally equal function across two modules would stop merging.
Rows 2 and 3 of the table are therefore vacuous for a function. **This
is a deviation from the plain reading of the specification, and it
needs a decision.**

### Order-invariant labeling, and the measured cost

The canonical member order that sorted by name is gone. A component
labels its members by structural refinement:

1. the first colour of a member is its bytes with one placeholder for
   each in-component reference;
2. each round folds in the colours of the referenced members, in their
   position order inside the member;
3. the loop stops as soon as the distinct colour count stops growing.

An in-component closure body takes part in the refinement, so a member
that differs only inside a nested closure still receives its own
colour.

The colours are content ranks, not hashes: the rounds sort byte
signatures and assign ranks, and one SHA-256 pass at the fixed point
produces the component hash and the member hashes. **This is a
deviation from step 4 of specification section 6**, which states that
the final label is the iterated hash itself. Iterated hashing costs
one SHA-256 per member per round, and the round count is one per
member on a crafted component. The partition is identical either way;
only the hash domain differs.

Measured on a wide hostile component (`chain-N`: a cycle of N
functions where member zero carries one extra instruction, so
refinement separates one member per round), release build:

```text
chain-64:        3.5 KiB   62 rounds   162 us
chain-256:      13.9 KiB  254 rounds   1.14 ms
chain-1024:     55.4 KiB 1022 rounds  22.8 ms
chain-2048:    111.7 KiB 2046 rounds  94.6 ms
symmetric-256:  13.9 KiB    1 round    246 us
symmetric-2048:111.7 KiB    1 round   1.82 ms
symmetric-8192:449.6 KiB    1 round   7.58 ms
```

A wide symmetric component costs one round and stays linear in the
module size. A crafted chain is quadratic. The work is therefore
bounded: `REFINE_WORK_BUDGET` caps `rounds * (members + references)`
at 2^24, and a component past the budget rejects with a clear
diagnostic. The bound lets a chain of about 2900 members through and
costs about 200 ms at the limit. No source program reaches it;
`a_component_past_the_refinement_budget_rejects` proves the rejection.

### Symmetric members

`StepEvent.Ran` and `StepEvent.Waiting` are empty case classes of one
family. Nothing structural separates them, so they share one
structural hash. The pinned core table now holds one hash twice, and
the core pin resolves on the pair `(qualified key, structural hash)`.
`Ordering.Less`, `Ordering.Equal`, and `Ordering.Greater` are the same
case.

### Slot resolution and the core role table

An artifact carries a core role table: twenty class slots, one per
stable core role, inside the semantic region. The compiler fills it,
the linker relocates it and rejects a module that fills a role with
another class, and the verifier proves the kind, the generic arity,
the parent slot, and the exact field layout of every filled slot.

The consequences:

- the verifier reads no name and no definition hash;
- the loader computes no identity;
- an artifact with no source resolves its core from its own bytes;
- `core/pinned-core-defs.txt` is a determinism gate now, not a
  resolution mechanism.

The structural rules are weaker than the old hash pin in one way: a
core class with an edited method body now fills its role. That is
sound. The runtime allocates through the slots and reads fields by
position, and the verifier proves the layout it needs. The old rule
proved bit equality of the whole class, which is more than any rule
reads.

### The verification hash

The digest covers the semantic region and the operation manifest
digest. That is the exact input set of the verifier. A rename
therefore costs no cache hit, and
`a_rename_of_every_definition_keeps_the_verification_hash` renames
every class, every key, and every function and proves one verifier
run for both modules.

The order held: the names left the digest only after the verifier
stopped reading every name-derived value.

### The `mut` marker encoding

The week-6 note left this open as a format question. It is closed.
`encode_semantic` and the `BcType::Fn` arm of `encode_type` write an
explicit count before every marker vector, and the decoder rejects a
vector whose count differs from the parameter count. The semantic
region describes itself now, so the marker lengths left
`verification_hash`: the stopgap is redundant.

Three probes hold it:
`a_misaligned_function_marker_vector_rejects_at_the_decoder`,
`a_misaligned_type_marker_vector_rejects_at_the_decoder`, and
`two_marker_shapes_write_two_semantic_regions`.

### The rename question, closed

The week-6 note left four positions open on a rename inside a cyclic
component. **Position 4 landed**: the member order reads no name, so a
rename never moves a structural hash, inside a cycle or outside one.
The measured cases, which the week-6 note recorded as
`even -> evenx  false`, `even -> aaa  false`, `even -> zzz  true`, are
all `false` now (`a_rename_inside_a_cycle_moves_no_hash`). The stated
cost of position 4 is real and visible: the component hash no longer
tells apart members that differ only by name, and two symmetric
members share one hash.

### The three-stage build store

Stage 2 keys the linked artifact on the ordered module content hashes
of the program, so a rebuild with no source change neither links nor
verifies the merged program. Stage 3 keys the verifier verdict on the
verified-code key, so a second load of one artifact never meets the
verifier.

A stage-3 verdict carries no fact. The artifact declares its own core
role table, so a load reads the layout from the bytes. A damaged or
forged entry can therefore supply no resolved value; it can only
assert an admission. `lm_vm::load_with_record` recomputes the key from
the decoded module and rejects a verdict filed under any other key.

**One change the specification did not call for.** Section 10 says
stage 3 persists "beside stage 2, under the same directory". For an
artifact inside a package that is `<package>/build`, which the
developer owns. For an artifact with no package the first
implementation used `<artifact directory>/build`, and an independent
check found the hole: the supplier of a foreign `.lma` would then also
control the directory that holds its verdict, and a verdict replaces a
verifier run. The store of an artifact with no package moved to the
user cache directory (`LM_CACHE_DIR`, then `XDG_CACHE_HOME/lm`, then
`HOME/.cache/lm`). Stage 3 stays reachable for a foreign artifact,
which is the requirement.

### Measurements

All figures are release builds on one machine, median of 41 runs.

```text
                        base ad80ad4    now      work above the floor
process-start floor        1152 us    1139 us     -
lm build (no-op rebuild)   2559 us    1586 us     1407 us -> 447 us
lm run <artifact.lma>      1471 us    1251 us      319 us -> 112 us
```

The no-op rebuild falls 38% in wall time and 68% in the work above
process start. Process start is 72% of what is left.

The load path, on a 7.9 KiB module, 200 rounds:

```text
module_identity   195 us
verify_module      21 us
load_bytes         39 us   (decode, verify, dispatch tables)
```

`verify_module` no longer computes identity, and neither does the
loader. The week-6 note measured `module_identity` at 138 us and the
full verifier at 159 us on a 6.9 KiB module, where identity was almost
all of the verifier cost. Identity now runs in the compiler and the
linker only.

Definition sharing holds: the linked `examples/05-modules/app` program
still carries 27 classes.

### Moved hashes

- the core image pin:
  `16eda1db90588c74321eb9352a84a5a1a4c982d3570fa83d2ef2aa76933f252f`
  to
  `df377f94ffbf8cc9e4a6358f02146611448455e228bc029e0181a22f8dc68657`;
- every one of the twenty core structural hashes in
  `core/pinned-core-defs.txt`, regenerated with the ignored test
  `regenerate_core_pins`. `StepEvent.Ran` and `StepEvent.Waiting` now
  hold one value;
- every definition hash of every module, because the structural
  encoding changed;
- `examples/01-basics/factorial.lm` moved to semantic
  `9a0e71625cbfab30e8c19278bed49ef6c206d87ad36228be660394e7175c6a7c`
  and container
  `1f5338c1443db6914209258d9fdcdb1912fa6054d21488f4fedb211921debe07`;
- `examples/05-modules/app` moved to semantic
  `bfc4ad384da9aba74e58a625ed504609da3e0775389c60b02fcfffafcabb9bf3`;
- every fuzz corpus module, regenerated with the ignored test
  `regenerate_fuzz_corpus` for container format version 8.

### Changed tests

- `week5_identity.rs`:
  `identical_shapes_in_different_families_stay_distinct` became
  `a_referenced_qualified_key_separates_identical_shapes`. Two arms of
  one family with equal shapes now share one hash, which is section 7.
- `week5_identity.rs`: `a_duplicate_class_name_cannot_use_a_cache_hit`
  became `a_duplicate_class_key_cannot_use_a_cache_hit`. The probe
  needs one key, not one name.
- `week5_identity.rs`: `a_rename_moves_the_verification_hash` became
  `a_rename_keeps_the_verification_hash`. The verifier reads no name.
- `week5_identity.rs`:
  `a_corrupted_core_definition_no_longer_resolves` became
  `a_corrupted_core_definition_fails_the_role_shape`. The slot still
  resolves; the verifier rejects the shape.
- `week6_identity.rs`:
  `differently_named_classes_never_share_a_definition_hash` became
  `the_qualified_key_separates_a_user_enum_from_the_core_family`. Two
  classes with different names may share a structural hash; the
  referenced keys keep the two families apart.
- `week6_identity.rs`: `a_cache_hit_replays_the_identity` became
  `a_load_computes_no_identity`.
- `week6_names.rs`: the file inverted.
  `a_crafted_function_rename_cannot_split_the_cache` became
  `a_crafted_function_rename_moves_no_structural_hash`, and a crafted
  qualified key moves no verifier input either. The new probe
  `a_crafted_core_role_cannot_split_the_cache` holds the invariant on
  the value the verifier does read.
- `bench_smoke.rs`: the identity-replay assertion is gone.
- `core_image.rs` and `fuzz.rs`: the pinned core files and the fuzz
  corpus regenerated. Expected churn, no expectation weakened.

### New tests

`identity_linking.rs` (21 cases) covers the referenced nominal
identity, the four rows of the merge table, the order-invariant
labeling, the symmetric members, the refinement budget, the core role
slots, the self-describing marker encoding, and the reserved `core`
module path. Two ignored cases measure the refinement cost and the
load path. `week6_store.rs` (10 cases) covers stage 2 and stage 3.

### Deferred

- A QualifiedKey for a function, if the deviation above is settled the
  other way.
- The iterated-hash form of the labels in specification section 6, if
  the hash domain matters more than the quadratic worst case.
- A user-cache location for stage 3 that survives a `HOME` with no
  write permission. The store falls back to `.lm-cache` in the current
  directory, which is a working directory, not a user directory.

### One defect the self-review found

The first version of the refinement emitted only the definition
members into the component hash. A member names an in-component
closure body by colour, so the bytes of that closure reached no hash.
Two functions that differ only inside a nested closure then shared one
structural hash, and the linker would have rejected the program for
two definitions under one hash.

The component hash covers every refinement member now, closure bodies
included. The probe
`a_nested_closure_body_reaches_the_structural_hash` failed before the
fix and passes after it. The core pins did not move, because the core
image holds no closure inside a component.
