# Artifacts and Portable Snapshots

Status: accepted design. The atomic runtime swap is complete.

Performance closeout is active.

This document refines the artifact, linker, VM, and snapshot rules.

It replaces no source-language syntax.

## 1. Purpose

Loom code is a value. A Loom world must also be a portable value.

The current system links one complete program into one aggregate `Module`.

The current snapshot stores state against that ambient module.

This design removes that ambient dependency.

It keeps execution tables dense and fast.

It also keeps the standard core outside thin encodings.

The package rules remain in [packages.md](packages.md).

This document adds no second package graph, module path system, resolver, or cache.

## 2. Unified build-output model

The public model has two main values.

- An `Artifact` contains code and exact artifact dependencies.
- A `Snapshot` contains artifacts and runtime state.

Verification remains the publication boundary for decoded code.

The source package model keeps its current parts.

- `lm.package` defines one source package and its dependency aliases.
- The source tree defines canonical module paths.
- `CompileEnv` supplies visible interfaces to the compiler.
- `LinkUnit` carries one compiled module and its exact dependencies.
- `LinkEnv` resolves canonical module paths.

An artifact is the built form of one selected root.

It can be a program entry, one library module, or a `codeof` closure.

One source package can therefore produce several artifacts.

`Module` remains the decoded bytecode payload of one `LinkUnit`.

`Module` is not the deployment abstraction.

`LinkUnit` is an internal build and link value.

`Artifact` and `Snapshot` remain the public values.

An encoded artifact can include dependency units.

The same artifact can use a thin or fat encoding.

A thin encoding omits exact dependencies that the loader can supply.

A fat encoding includes the complete dependency graph.

Both encodings name the same root `ArtifactId`.

The build cache does not participate in artifact resolution.

### 2.1 One path from source to execution

The system uses this single path.

```text
source package graph
  -> CompiledModule
  -> LinkUnit
  -> Artifact
  -> LinkEnv
  -> CodeNamespace
```

`CompiledModule` is the unbound result of one compiler call.

It contains one canonical module and build metadata.

The compiler derives an `Interface` view from that module.

The source build cache can store that result through its existing keys.

The package builder binds every import to an exact provider.

That step produces one `LinkUnit`.

`LinkUnit` is the only linkable module type.

It can cache the derived interface view in memory.

It does not encode that view beside the module.

An `Artifact` stores one root and its reachable `LinkUnit` closure.

Thin and fat artifacts differ only in which exact dependency units they embed.

`LinkEnv` resolves the artifact graph for the compiler or runtime.

The runtime never reads the source build cache.

The `.lma` file contains the same `Artifact` value that runtime compilation produces.

A snapshot contains one or more ordinary artifacts and runtime state.

No stage rebuilds a second package graph.

## 3. Link units

The existing compiler `LinkUnit` moves into `lm-bytecode`.

`lm-compiler` re-exports that exact type.

Stage 3 removes all legacy construction with missing dependency bindings.

```text
LinkUnit
  module_path: String
  module: Module
  dependencies: List[ArtifactDependency]
```

The module contains executable code and public contract facts once.

The `.lmi` file is an optional derived compiler view.

The build cache can store that view for separate compilation.

The runtime artifact never stores the `.lmi` payload.

One dependency contains a canonical module path and an exact identity.

```text
ArtifactDependency
  module_path: String
  artifact: ArtifactId
```

The module path never contains a filesystem path.

The package builder derives it through the rules in `packages.md`.

Manifest dependency aliases do not enter the artifact.

The standard core uses the canonical module path `core`.

One unit cannot bind one module path twice.

Dependency order has no semantic meaning.

Canonical encodings sort dependencies by module path and identity.

Each `Import.module` must name a bound dependency module.

The linker rejects an import that has no exact dependency binding.

The wire codec can use a private record structure.

That record is not another public code abstraction.

## 4. Artifact containers

An encoded `Artifact` is a flat container.

It contains one root unit and zero or more embedded units.

The artifact header names the root by `ArtifactId`.

Each embedded unit carries its direct dependency list.

The units form a directed acyclic graph in the initial release.

The decoder rejects duplicate units and unreachable units.

The decoder uses an explicit work stack for graph checks.

Source module import cycles remain unsupported in the initial release.

Definition cycles inside one unit remain supported.

The definition collector uses the shared SCC implementation.

### 4.1 Canonical artifact encoding

The artifact magic is `LMAR`.

Artifact format version 3 uses this header.

```text
magic:               4 bytes
format version:      u16
ABI bundle digest:   32 bytes
root ArtifactId:     32 bytes
unit count:          u32
```

Units follow in ascending `ArtifactId` order.

Each unit uses this encoding.

```text
stored ArtifactId:   32 bytes
module path length:  u32
module path:         UTF-8 bytes
dependency count:    u32
dependencies:        repeated dependency records
module length:       u32
module payload:      LMBC bytes
```

The LMBC export section contains the facts that derive the compiler interface.

The LMAR container does not store a second interface type tree.

Each dependency uses this encoding.

```text
module path length:  u32
module path:         UTF-8 bytes
dependency identity: 32 bytes
```

The decoder recomputes each stored `ArtifactId`.

The artifact contains no trusted identity field.

The decoder rejects units that do not use ascending identity order.

The decoder rejects dependencies that do not use canonical order.

A trusted builder can sort values before encoding.

## 5. Identity

### 5.1 ArtifactId

`ArtifactId` is the semantic identity of one `LinkUnit` closure.

It uses BLAKE3-256 with a distinct domain tag.

It covers these values.

- The canonical module path.
- The module semantic hash.
- The derived interface identity.
- The exact bytecode semantic section.
- The exact linker-visible module surface.
- Every dependency module path and `ArtifactId`.

The derived interface identity uses facts from the same module payload.

The module semantic hash owns format and ABI version coverage.

`ArtifactId` does not encode those values a second time.

It does not cover source text or optional debug data.

A debug-only edit keeps the same `ArtifactId`.

A resolved dependency edit moves the `ArtifactId`.

### 5.2 Container hash

The existing container hash is BLAKE3-256 over the exact encoded artifact bytes.

A thin and fat encoding can share one `ArtifactId`.

They have different container hashes.

An encoding or debug-data edit moves the container hash.

The artifact model adds no second exact-byte hash type.

### 5.3 Stored identities

The container stores each unit identity for graph references.

The decoder recomputes every stored identity.

No stored digest acts as proof.

Semantic identity must return an error for every invalid decoded module.

Semantic identity must never panic on decoded input.

## 6. Initial resolution

The initial release has no local or network runtime artifact store.

The shared linker has two artifact sources.

1. It uses units embedded in the artifact.
2. It can use the exact runtime standard core.

Every non-core dependency must be embedded.

The linker identifies core through its canonical module path.

A thin core dependency must exactly match the runtime core `ArtifactId`.

The linker rejects a different thin core identity.

A fat artifact can embed another compatible core.

That core must pass the runtime ABI and verifier checks.

The runtime never substitutes a compatible artifact automatically.

A future store can resolve more exact `ArtifactId` values.

That extension does not change the artifact format.

The standard build stores one pinned compiled core beside its source pins.

The pin generator verifies the compiled core before it writes the file.

Each process decodes this compiled core once.

A custom ABI bundle compiles and verifies its own core.

`LinkEnv` remains the only module-path resolver.

Decoded artifact units populate one `LinkEnv` for each resolved graph.

The compiler, runtime loader, and snapshot restore use the same linker.

The existing build cache remains private to source builds.

Runtime loading never queries the build cache.

### 6.1 Shared linker ownership

The compiler and VM both need the same linker.

Neither crate can depend on the other.

The existing linker moves into one shared lower crate.

This document calls that crate `lm-link`.

`lm-link` depends on `lm-bytecode` and `lm-verify`.

It has no source compiler, filesystem, host, network, or clock dependency.

`lm-bytecode` owns the `LinkUnit` data and artifact codec.

`lm-link` owns `LinkEnv`, graph resolution, dependency collection, and relocation plans.

`lm-compiler` can re-export its existing `LinkEnv` surface.

This move replaces the compiler linker and runtime append linker with one implementation.

### 6.2 Package build flow

The existing package builder remains the only manifest consumer.

It resolves dependency aliases to canonical package and module paths.

It compiles modules in dependency order through `CompileEnv`.

It then binds actual imports to exact provider `ArtifactId` values.

The selected root and its reachable units form the Artifact.

Unreachable source modules do not enter that Artifact.

The standard catalog supplies ordinary `LinkUnit` values.

The core prelude also becomes one ordinary `LinkUnit` dependency.

`CompileEnv` keeps the prelude names available without a `use` declaration.

The lowerer emits imports for referenced core definitions.

A compiled source unit contains no copied core method body.

The old flattened linked-program `Module` stops being the deployment value.

The `.lma` file stores the Artifact container instead.

## 7. Runtime ABI

The operation table is universal for one runtime ABI.

The table is append-only.

An operation keeps its number and contract forever.

Removal leaves a tombstone.

A contract change creates a new operation.

The runtime ABI also fixes these items.

- Native intrinsic numbers and contracts.
- Primitive value representations.
- Calling conventions.
- Required core roles.
- Bytecode and snapshot format versions.

A fat core can differ from the runtime standard core.

It must satisfy this exact runtime ABI.

Cross-version admission remains deferred.

Bytecode effect rows store ABI operation and group slots.

Closed effect rows expand groups into exact operation slots.

A closed row depends on the group membership of its pinned ABI bundle.

Effect rows never store module string slots.

Linking and snapshot restore never relocate effect rows.

Snapshot admission validates each closed row against the exact ABI bundle.

## 8. Linking and verification

The compiler verifies each emitted `LinkUnit` once.

Trusted compiler linking does not repeat verification or interface validation.

An artifact decoder treats each embedded unit as untrusted code.

The artifact linker verifies each decoded embedded unit once before relocation.

The exact runtime core is already verified.

The artifact linker does not verify that core again.

An imported declaration has no executable body.

The linker resolves imports through exact `LinkUnit` dependencies.

An import identifies these values.

- A canonical dependency module path.
- An exported key.
- An export kind.
- An interface contract hash.

The provider interface hash and export kind select one exact provider.

The importer declaration has no authority after resolution.

Relocation replaces each imported declaration with the provider definition.

The linker compares the imported contract with the exported contract before publication.

It does not verify an aggregate arena after relocation.

Unit verification rejects conformances that attach to imported classes.

The linker rejects unresolved imports before executable publication.

The linker publishes a complete transaction or publishes nothing.

## 9. Code storage

One `World` owns one `CodeArena`.

The arena holds dense executable tables.

Each table uses immutable append-only chunks.

A revision is a prefix view of those chunks.

Publication never copies an existing semantic table entry.

Publication costs `O(unit + artifact graph)` work.

A `LinkUnit` enters the arena through one checked relocation.

Existing indices never move.

The arena deduplicates complete units by `ArtifactId`.

The arena builds each class dispatch row when that class enters.

It does not rebuild prior dispatch rows.

The initial release does not deduplicate definitions across different units.

One `CodeNamespace` is one relocated and immutable `LinkEnv`.

It records module bindings, slots, exports, relocation maps, and core roles.

Each machine stores one `NamespaceId`.

The coordinator resolves that identifier before execution.

The interpreter receives direct table and dispatch views.

Each machine, including a host root, owns one VM image record.

A root snapshot therefore restores through the same path as a run snapshot.

An execution lease pins its namespace view.

The world has no global core-role layout.

Each namespace carries the core-role layout of its exact core artifact.

Host replies use the destination machine's namespace.

Normal instruction dispatch uses dense arena indices.

It performs no hash or namespace lookup.

The first publication uses direct contiguous table storage.

Later publications add immutable chunks without changing the first storage.

Two namespaces can bind the same module path to different exact artifacts.

Cross-namespace work uses VM boundary operations.

Direct calls and slot targets stay inside one namespace.

The arena can hold two compatible core artifacts.

Each core keeps its own `ArtifactId`, definitions, and core-role layout.

One resolved artifact graph binds each module path once.

A world can hold several graphs through separate namespaces.

A snapshot can therefore contain `core` units with different identities.

Its namespace manifests state which exact core belongs to each machine.

Primitive value representations come from the universal runtime ABI.

Primitive virtual dispatch uses the current machine's core-role layout.

Heap instances carry relocated arena class indices.

## 10. Boundary values

Public VM boundaries accept only exact value types.

The checker rejects an open class type at the declaration site.

A final class is exact.

A sealed family is exact when its artifact closure contains every case.

Artifact collection keeps a sealed parent with all case children.

Tuples and immutable collections are exact when their element types are exact.

`DynValue` carries one value and its closed runtime type.

A dynamic result stays in the machine that produced it.

The holder receives a reference to that machine, like a run handle.

A snapshot of the holder closes over that machine.

The holder cannot call the value. It renders the value through the owning machine's code.

## 11. `codeof` and runtime compilation

`codeof` produces the same `LinkUnit` form as source compilation.

It includes the selected definition closure.

The closure includes required types, methods, conformances, literals, and called definitions.

It also includes exact dependency references.

`codeof` never requires a whole-module install.

Runtime source compilation produces the same artifact form.

Both paths use the same verifier and linker.

A closure activation inherits the spawner's pinned namespace revision.

It never inherits an unrelated current revision.

## 12. Snapshot model

A snapshot contains these logical sections.

```text
Snapshot
  artifacts: List[Artifact]
  namespaces: List[NamespaceManifest]
  state: RuntimeState
```

The artifact section uses the ordinary artifact container format.

The namespace section binds machines to resolved artifact graphs.

The state section stores heaps, frames, mailboxes, policies, slots, and resources.

A thin snapshot can omit the exact runtime standard core.

A fat snapshot embeds every required unit, including each core.

Both forms use the same admission path.

A snapshot never relies on the loader's ambient program.

An in-memory snapshot shares immutable arena units and namespace views.

Its trusted state can retain live arena indices.

Snapshot capture does not encode shared code until the caller requests bytes.

Trusted capture retains shared `Artifact` values.

The `bytes()` operation performs the first artifact encoding.

It also converts live arena indices into the snapshot-local code layout.

A serialized thin snapshot stores program and installed-code units.

It references the exact runtime core by `ArtifactId`.

A serialized fat snapshot embeds the complete unit graph.

## 13. Snapshot relocation

A serialized snapshot never stores a process-global arena index.

The artifact graph defines one deterministic snapshot-local code layout.

The loader starts with an empty arena.

It replays each namespace manifest in stored order.

Each manifest replays its artifact chain in stored order.

Each artifact replays dependency units before its root unit.

This replay assigns every snapshot-local code index.

The encoder maps live arena indices into the local layout.

The external loader rebuilds the same layout from verified artifacts.

Admission never uses a world execution namespace as the source layout.

World publication history cannot change an admission result.

A world can reuse a canonical layout for the same complete code manifest.

The reuse key includes the bundle, runtime core, artifacts, and namespace manifests.

The loader compares complete artifact bytes before it trusts a reused layout.

A clean extension chain already has this local layout.

The writer proves this property from the immutable arena prefix.

It then writes the local indices without replaying the artifact graph.

An interleaved publication uses the full relocation path.

Restore maps local indices into the destination arena.

`NamespaceId` values never enter the container.

Admission validates every stored index before restore.

Restore builds checked maps for every indexed table.

The maps cover these items.

- Strings and byte literals.
- Types and class positions.
- Type environments and applications.
- Functions, callbacks, and closures.
- Classes, instances, and conformances.
- Slots, bindings, and native code handles.
- Slot-table positions and replacement versions.
- String and byte-literal cache positions.
- Core roles and namespace records.
- Debug data and fault traces.
- Every type-bearing heap payload.

`EmptyCase.ty` is a type-bearing heap payload.

No restore path indexes a map before admission proves the source index.

Mutation tests must reach each map with a non-identity relocation.

Trusted in-memory restore uses its live layout without this conversion.

## 14. Debugger capability

A Loom debugger can load arbitrary snapshot bytes.

It can provide embedded artifacts to snapshot admission.

It can inspect stack frames, source sites, and faults.

It can drive, step, answer, and resume the foreign VM.

The debugger keeps its own namespace.

The debuggee keeps the namespace stored in its snapshot.

A debugger with core A can load a fat snapshot with compatible core B.

The same debugger rejects a thin snapshot that requires core B.

`Vm.restore_dynamic` restores the distinguished run of a loaded `VmSnapshot` as `Run[DynValue]`.

`Run.stack` lists the frames of a stopped run as `List[CodeLocation]`.

The terminal value of a dynamic run crosses the boundary as one `DynValue`.

The machine keeps a dynamic result flag. A snapshot stores that flag.

The public result type of a dynamic run is a fixed marker. No typed cast selects it.

## 15. Collection

Collection works inside artifact boundaries.

It never flattens all dependencies before collection.

The first pass selects needed `LinkUnit` values.

The second pass selects definitions inside each selected unit.

The definition graph includes every bytecode reference kind.

The graph also includes type, class, interface, conformance, slot, and role references.

The collector uses `lm-scc` for definition cycles.

It uses an explicit work stack.

`use m.f` roots only `f` and its dependency closure.

A module import can root the complete exported module surface.

Keeping a runtime role class does not keep every unused method.

Runtime construction roots only the methods that the runtime can call.

Collection produces ordinary `LinkUnit` values.

It recomputes each module identity, interface identity, dependency list, and `ArtifactId`.

Collection preserves the exact core dependency.

It never copies core definitions into a local unit.

## 16. Resource limits

The decoder checks total bytes before nested decode work.

It checks every count against the remaining input.

It applies limits before verification, identity work, or arena cloning.

The initial limits cover these values.

- Total artifact bytes.
- Artifact unit count.
- Direct dependency count.
- Module payload bytes.
- Total decoded code bytes.

The initial release uses these default limits.

| Limit | Value |
| --- | ---: |
| Total artifact bytes | 256 MiB |
| Artifact units | 4,096 |
| Direct dependencies per unit | 4,096 |
| One module payload | 64 MiB |
| Total module bytes | 256 MiB |
| One dependency module path | 4 KiB |

One operation decodes one artifact blob once.

Later phases share the decoded value.

## 17. Performance rules

Normal source compilation must not pay snapshot packaging costs.

Trusted source execution performs collection, identity calculation, and relocation.

Artifact build also performs encoding.

Trusted compilation does not repeat unit verification or interface validation.

Normal execution must not pay namespace lookup costs per instruction.

The process reuses its verified standard core unit.

The runtime keeps no on-disk verifier verdict cache.

Compression is also an additional optimization.

Artifact size gates use raw canonical bytes.

Performance records use same-session parent measurements.

The records name cold and warm paths separately.

## 18. Implementation stages

### Stage 0: clean foundation and baseline

- Start from `main`.
- Preserve the abandoned collector branch.
- Record same-session core and workspace measurements.
- Add this sidecar before code changes.

Gate: the baseline names its revision, profile, processor, and scheduler mode.

The initial baseline uses revision `8f7ba66`.

The host has an AMD Ryzen 9 9950X processor.

The release core benchmark uses deterministic mode on logical processor zero.

The workspace result uses a warm debug build and default test concurrency.

| Initial measurement | Result |
| --- | ---: |
| Core artifact bytes | 274,657 |
| Core compilation | 3.780 ms |
| Core decoding | 0.410 ms |
| Core verification | 1.401 ms |
| Core semantic identity | 2.523 ms |
| Core loading | 2.004 ms |
| Warm workspace suite | 49.92 s |

### Stage 1: artifact identity and dependencies

- Add `ArtifactId`.
- Add canonical dependency bindings.
- Compute identities from decoded content.
- Add bounded artifact decoding.
- Test identity stability and dependency sensitivity.

Gate: stored identities never bypass recomputation.

### Stage 2: thin and fat artifact containers

- Add the flat artifact encoding.
- Add exact runtime-core resolution.
- Reject missing non-core dependencies.
- Reject cycles, duplicates, and unreachable units.
- Keep the legacy `Module` codec as the payload codec.

Gate: thin and fat forms resolve to the same artifact graph.

Gate: a thin core mismatch rejects before linking.

Gate: a compatible embedded core resolves without ambient core identity.

Stages 1 and 2 do not replace the current execution loader.

### Stage 2R: package and linker reconciliation

- Make the wire record contain one complete `LinkUnit`.
- Store the canonical module path and module payload.
- Replace artifact namespaces with canonical module paths.
- Reuse the existing container hash.
- Remove the independent artifact resolver.
- Reject non-canonical unit and dependency order.
- Add semantic payload mutation tests.
- State that decoded artifacts remain untrusted.
- Rewrite this document around `packages.md`.

Gate: the artifact model defines no second package graph, resolver, cache, or byte hash.

Gate: the decoder rejects every non-canonical ordering.

Gate: semantic identity returns an error or a value for every decoded payload.

### Stage 2S: one canonical module surface

- Store public contract facts in the LMBC export section.
- Derive compiler interfaces from canonical module tables.
- Remove the second LMI payload from LMAR.
- Keep `.lmi` as an optional build-cache projection.
- Change the LMAR format version to 3.
- Change the LMBC format version to 58.

Gate: LMAR stores each module surface once.

Gate: an encoded interface view adds zero bytes to LMAR.

Gate: a decoded module derives the same interface view as trusted compilation.

### Stage 3: shared linking and core dependency

`lm-link` now owns exact artifact resolution and relocation.

Package builds and artifact loads now use one `LinkEnv`.

The compiler builds core once and emits source-module core imports.

Local functions keep separate arena entries after relocation.

The `.lma` file now contains the canonical Artifact container.

- Move the existing linker into the shared lower layer.
- Keep one `LinkEnv` implementation.
- Make package builds produce exact `LinkUnit` graphs.
- Compile core once as a normal dependency unit.
- Emit source-module core references as imports.
- Replace the flattened `.lma` payload with the Artifact container.
- Resolve imports across artifact units.
- Replace local extern declarations with exact provider definitions.
- Reject executable extern declarations.
- Reject foreign conformance attachment.
- Publish links as one transaction.

Gate: decoded artifact units verify before relocation.

Gate: trusted compiler units do not verify twice.

Gate: incompatible relocated calls reject during link contract comparison.

Gate: a source module contains no copied core function body.

Gate: the cold path does not use compression or a cache result.

### Stage 4: artifact-aware dependency collection

- Start from the selected artifact root.
- Collect the root module from its entry function.
- Read retained imports as exact provider export requests.
- Process provider modules in reverse dependency order.
- Collect each provider from the union of its requests.
- Use `lm-scc` for definition cycles.
- Retain sealed families as complete units.
- Rebuild each collected module interface from retained exports.
- Rebuild provider `LinkUnit` values before importer values.
- Recompute every dependency `ArtifactId` after collection.
- Keep the exact core dependency unchanged.
- Permit that core dependency without a retained core import.
- Reject every other unused exact dependency.
- Restore deep and cyclic collector tests.

Gate: program `1` keeps one local entry and no local core class.

Gate: `use m.f` removes unrelated exports.

Gate result: program `1` keeps one function and no class in its root unit.

Gate result: one selected import removes unrelated exports and an unused cycle.

### Stage 5: shared arena and namespaces

- Add one append-only `CodeArena` to `World`.
- Store arena tables as immutable append-only chunks.
- Define each revision as one prefix view.
- Add `CodeNamespace` and `NamespaceId`.
- Relocate each exact unit once.
- Build dispatch rows only for new classes.
- Deduplicate complete units by `ArtifactId`.
- Keep different units separate despite equal definition hashes.
- Move core roles from `World` into each namespace.
- Route host replies through the destination namespace.
- Keep execution on dense indices.

Gate: two namespaces can bind equal names to different definitions.

Gate: two compatible cores operate in separate namespaces.

Gate: direct-call performance stays within normal noise.

Gate: publication 100 does not cost more than twice publication 1.

### Stage 6: `codeof` and runtime compilation

- Make `codeof` emit a portable definition closure.
- Make runtime compilation emit the same artifact form.
- Install both forms through the artifact linker.

Gate: a function and a class install into an empty VM.

### Stage 7: artifact-backed snapshots

- Replace ambient-program snapshot binding with artifact containers.
- Encode thin and fat snapshots.
- Store namespace manifests.
- Share code views in in-memory snapshots.
- Preserve runtime-compiled code and slots.
- Encode code references through a deterministic snapshot-local layout.

Gate: an unrelated Loom program restores and drives the snapshot.

Gate: thin capture and restore do not encode or decode the runtime core.

Gate: no container field stores a process-global arena index.

### Stage 8: admission and relocation closure

- Validate every stored index before restore.
- Relocate every indexed value kind.
- Relocate positional slot and literal tables.
- Validate every closed effect operation slot against the ABI bundle.
- Extend mutation tests for every map.
- Run each mutation against a non-identity destination.

Gate: admitted bytes never panic during restore or execution.

Gate result: a debugger capture admits in a fresh core-only world.

### Stage 9: debugger proof

- Add one Loom debugger example.
- Load another program's snapshot from bytes.
- Inspect its stack and source.
- Drive the foreign program under debugger policy.

Gate: the debugger loads a fat snapshot with another compatible core.

Gate result: `examples/15-compiler-and-hot-code-reloading/12-debug-another-program.lm` loads a thin snapshot of an unrelated program.

The debugger reads the top frame source, answers `Clock.Now`, and renders the result.

Gate result: the debugger retains a foreign result across capture and external restore.

The fat snapshot gate remains open. No writer embeds a core.

### Stage 10: limits and performance closeout

- Apply all limits before expensive work.
- Remove repeated decode and whole-arena verification.
- Measure raw sizes and cold paths.
- Measure compile, link, load, install, restore, and execution.
- Run the full workspace suite.

Gate: each existing execution benchmark stays within five percent of its same-session parent result.

Gate: collected artifacts improve raw size and cold load time.

## 19. Deferred work

These items remain outside the initial release.

- A local artifact store.
- A network artifact store.
- Compatible dependency substitution.
- Source module import cycles.
- Cross-runtime snapshot migration.
- Fat snapshot encoding.
- Snapshot state diffs.
- Arena reclamation.
- Artifact compression.

None of these items changes the initial identity model.

## 20. Stage 2 performance record

The result uses revision `e1c73d4` and the Stage 0 measurement settings.

Existing compiler and execution paths do not consume the new artifact yet.

| Existing path | Parent | Stage 2 | Change |
| --- | ---: | ---: | ---: |
| Core artifact bytes | 274,657 | 274,657 | 0.0% |
| Core compilation | 3.780 ms | 3.802 ms | +0.6% |
| Core decoding | 0.410 ms | 0.399 ms | -2.7% |
| Core verification | 1.401 ms | 1.390 ms | -0.8% |
| Core semantic identity | 2.523 ms | 2.514 ms | -0.4% |
| Core loading | 2.004 ms | 2.009 ms | +0.2% |
| Warm workspace suite | 49.92 s | 49.56 s | -0.7% |

These differences stay inside normal process noise.

The prototype artifact path has these direct measurements.

| Artifact measurement | Result |
| --- | ---: |
| One-unit core artifact | 274,771 bytes |
| Artifact wrapper | 114 bytes |
| Artifact encoding | 0.128 ms |
| Artifact decoding and identity | 2.786 ms |

Artifact decoding recomputes semantic identity from decoded content.

The decoder does not trust the stored unit identity.

Stage 4 makes ordinary root units much smaller than the core unit.

## 21. Stage 3 performance record

The result uses the Stage 0 measurement settings.

This record predates the one-surface correction in Stage 2S.

Its LMAR stored a second interface view.

It does not describe the current LMAR format.

The current root unit still retains unused definitions before Stage 4.

| Stage 3 measurement | Result |
| --- | ---: |
| Core LMBC bytes | 285,598 |
| Core LMAR bytes | 454,904 |
| Core compilation | 4.539 ms |
| Core artifact decoding | 3.074 ms |
| Core verification | 1.409 ms |
| Core semantic identity | 2.516 ms |
| Core loading | 2.043 ms |
| Core cached loading | 0.162 ms |

The tiny program uses source `1`.

| Tiny program measurement | Result |
| --- | ---: |
| Raw artifact bytes | 221,295 |
| Embedded units | 1 |
| Root classes | 299 |
| Root functions | 896 |
| Artifact decoding | 0.615 ms |
| Artifact linking | 11.691 ms |
| Cold decode, link, and load | 13.688 ms |

These results are the Stage 4 baseline.

Stage 4 must reduce raw bytes and cold load time without compression or cached verification.

## 22. Stage 4 performance record

The corrected Stage 4 tree follows checkpoint `149d859`.

The `main` result uses revision `8f7ba66` from the same session.

The tiny program contains source `1`.

| Tiny program measurement | `main` | Stage 4 | Change |
| --- | ---: | ---: | ---: |
| Raw artifact bytes | 274,942 | 1,699 | -99.4% |
| Source compilation | 8.342 ms | 6.885 ms | -17.5% |
| Cold artifact load | 2.033 ms | 2.471 ms | +21.5% |
| Compilation and cold load | 10.375 ms | 9.356 ms | -9.8% |

The Stage 4 root contains one function and no class.

Artifact decoding takes 0.025 milliseconds.

Dependency collection takes 0.654 milliseconds.

Trusted linking takes 1.427 milliseconds.

Artifact linking takes 0.780 milliseconds.

These measurements use raw bytes without compression.

These measurements do not use cached verification.

Cold loading adds 0.438 milliseconds against `main`.

Compilation offsets that cost and improves the combined path by 9.8 percent.

The current core LMBC is 305,056 bytes.

The current core LMAR is 305,178 bytes.

LMAR adds 122 bytes and stores no second interface tree.

The former duplicate form used 456,464 bytes.

The current loader relocates the complete core into one flat executable module.

Stage 5 removes that flattening step through the shared `CodeArena`.

The warm workspace suite took 49.51 seconds on `main`.

The Stage 4 workspace suite took 56.43 seconds.

Stage 4 contains 1,684 tests.

The measured `main` tree contains 1,638 tests.

The absolute suite time increases by 14.0 percent.

Each test executable builds one process-local core `LinkUnit` when required.

The focused runtime gate used three pinned processes for each revision.

| Operation | `main` | Stage 4 | Change |
| --- | ---: | ---: | ---: |
| Direct call | 31.8 ns | 30.5 ns | -4.1% |
| Virtual call | 64.0 ns | 65.8 ns | +2.8% |
| List index | 44.4 ns | 43.6 ns | -1.8% |
| String interpolation | 208.5 ns | 214.1 ns | +2.7% |
| Interface default | 234.2 ns | 230.5 ns | -1.6% |
| List hash | 844.0 ns | 827.5 ns | -2.0% |
| List sort | 19,528.0 ns | 19,165.0 ns | -1.9% |
| Map hashable lookup | 216.9 ns | 213.6 ns | -1.5% |
| String builder | 39.9 ns | 42.0 ns | +5.3% |
| Text iteration | 76.8 ns | 75.9 ns | -1.2% |
| Large bytes decode | 911.6 ns | 940.2 ns | +3.1% |
| Byte buffer | 42.5 ns | 44.8 ns | +5.4% |
| Direct clock | 111.9 ns | 113.2 ns | +1.2% |

The mean ratio across these operations increases by 0.5 percent.

The largest measured increase is 5.4 percent.

Compiler-only surface fields follow the execution fields in decoded records.

This layout keeps the execution record prefix stable.

Stage 4 does not change the VM instruction path.

Stage 5 repeats the direct-call and interface-call gates after arena relocation.
