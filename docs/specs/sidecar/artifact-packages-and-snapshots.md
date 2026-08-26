# Artifacts and Portable Snapshots

Status: accepted design. Stages 0 through 2 have prototype implementations.

Stage 2R reconciles those implementations with the existing package and linker model.

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
- `LinkUnit` carries one compiled module and its interface.
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

It contains one module, one interface, and build metadata.

The source build cache can store that result through its existing keys.

The package builder binds every import to an exact provider.

That step produces one `LinkUnit`.

`LinkUnit` is the only linkable module type.

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
  interface: Interface
  dependencies: List[ArtifactDependency]
```

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

The units form a directed acyclic graph in version 1.

The decoder rejects duplicate units and unreachable units.

The decoder uses an explicit work stack for graph checks.

Source module import cycles remain unsupported in version 1.

Definition cycles inside one unit remain supported.

The definition collector uses the shared SCC implementation.

### 4.1 Canonical artifact encoding

The artifact magic is `LMAR`.

Artifact format version 1 uses this header.

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
interface length:    u32
interface payload:   LMI bytes
```

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
- The interface identity.
- Every dependency module path and `ArtifactId`.

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

## 6. Version 1 resolution

Version 1 has no local or network runtime artifact store.

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

`lm-link` owns `LinkEnv`, graph resolution, contract checks, and relocation plans.

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

## 8. Linking and verification

The verifier checks each `LinkUnit` before publication.

An imported declaration has no executable body.

The linker resolves imports through exact `LinkUnit` dependencies.

An import identifies these values.

- A canonical dependency module path.
- An exported key.
- An export kind.
- An interface contract hash.

The linker compares the importer declaration with the provider declaration.

The comparison covers the complete relocated contract.

For functions, it covers parameters, result, rows, generic bounds, and mutability.

For classes, it covers layout, parent, methods, conformances, and constructor contract.

An interface hash pin never replaces this structural comparison.

The linker rejects unresolved imports before executable publication.

The linker publishes a complete transaction or publishes nothing.

## 9. Code storage

One `World` owns one `CodeArena`.

The arena holds dense executable tables.

A `LinkUnit` enters the arena through one checked relocation.

Existing indices never move.

The arena deduplicates complete units by `ArtifactId`.

Version 1 does not deduplicate definitions across different units.

One `CodeNamespace` is one relocated and immutable `LinkEnv`.

It records module bindings, slots, exports, relocation maps, and core roles.

Each machine stores one `NamespaceId`.

An execution lease pins its namespace view.

The world has no global core-role layout.

Each namespace carries the core-role layout of its exact core artifact.

Host replies use the destination machine's namespace.

Normal instruction dispatch uses dense arena indices.

It performs no hash or namespace lookup.

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

`DynValue` stays inside the owning VM.

A debugger inspects dynamic values through VM operations.

It does not move those values into its own namespace.

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

Snapshot capture does not encode shared code until the caller requests bytes.

A serialized thin snapshot stores program and installed-code units.

It references the exact runtime core by `ArtifactId`.

A serialized fat snapshot embeds the complete unit graph.

## 13. Snapshot relocation

Admission validates every stored index before restore.

Restore builds checked maps for every indexed table.

The maps cover these items.

- Strings and byte literals.
- Types and effect rows.
- Type environments and applications.
- Functions, callbacks, and closures.
- Classes, instances, and conformances.
- Slots, bindings, and native code handles.
- Core roles and namespace records.
- Debug data and fault traces.
- Every type-bearing heap payload.

`EmptyCase.ty` is a type-bearing heap payload.

No restore path indexes a map before admission proves the source index.

Mutation tests must reach each map with a non-identity relocation.

## 14. Debugger capability

A Loom debugger can load arbitrary snapshot bytes.

It can provide embedded artifacts to snapshot admission.

It can inspect stack frames, source sites, and faults.

It can drive, step, answer, and resume the foreign VM.

The debugger keeps its own namespace.

The debuggee keeps the namespace stored in its snapshot.

A debugger with core A can load a fat snapshot with compatible core B.

The same debugger rejects a thin snapshot that requires core B.

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
- Interface payload bytes.
- Total decoded code bytes.

Version 1 uses these default limits.

| Limit | Value |
| --- | ---: |
| Total artifact bytes | 256 MiB |
| Artifact units | 4,096 |
| Direct dependencies per unit | 4,096 |
| One module payload | 64 MiB |
| One interface payload | 64 MiB |
| Total module and interface bytes | 256 MiB |
| One dependency module path | 4 KiB |

One operation decodes one artifact blob once.

Later phases share the decoded value.

## 17. Performance rules

Normal source compilation must not pay snapshot packaging costs.

Normal execution must not pay namespace lookup costs per instruction.

Core verification can be reused by exact `ArtifactId`.

This reuse is an additional optimization.

It cannot hide a slower cold path.

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
- Store the canonical module path and interface.
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

### Stage 3: shared linking and core dependency

- Move the existing linker into the shared lower layer.
- Keep one `LinkEnv` implementation.
- Make package builds produce exact `LinkUnit` graphs.
- Compile core once as a normal dependency unit.
- Emit source-module core references as imports.
- Replace the flattened `.lma` payload with the Artifact container.
- Resolve imports across artifact units.
- Compare complete importer and provider contracts.
- Reject executable extern declarations.
- Reject foreign conformance attachment.
- Publish links as one transaction.

Gate: a correct hash with a wrong local declaration rejects.

Gate: a source module contains no copied core function body.

Gate: the cold path does not use compression or a cache result.

### Stage 4: artifact-aware dependency collection

- Collect required `LinkUnit` values first.
- Collect required definitions second.
- Use `lm-scc` for definition cycles.
- Retain sealed families as complete units.
- Keep the exact core dependency unchanged.
- Restore deep and cyclic collector tests.

Gate: program `1` keeps one local entry and no local core class.

Gate: `use m.f` removes unrelated exports.

### Stage 5: shared arena and namespaces

- Add one append-only `CodeArena` to `World`.
- Add `CodeNamespace` and `NamespaceId`.
- Relocate each exact unit once.
- Deduplicate complete units by `ArtifactId`.
- Keep different units separate despite equal definition hashes.
- Move core roles from `World` into each namespace.
- Route host replies through the destination namespace.
- Keep execution on dense indices.

Gate: two namespaces can bind equal names to different definitions.

Gate: two compatible cores operate in separate namespaces.

Gate: direct-call performance stays within normal noise.

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

Gate: an unrelated Loom program restores and drives the snapshot.

Gate: thin capture and restore do not encode or decode the runtime core.

### Stage 8: admission and relocation closure

- Validate every stored index before restore.
- Relocate every indexed value kind.
- Extend mutation tests for every map.
- Run each mutation against a non-identity destination.

Gate: admitted bytes never panic during restore or execution.

### Stage 9: debugger proof

- Add one Loom debugger example.
- Load another program's snapshot from bytes.
- Inspect its stack and source.
- Drive the foreign program under debugger policy.

Gate: the debugger loads a fat snapshot with another compatible core.

### Stage 10: limits and performance closeout

- Apply all limits before expensive work.
- Remove repeated decode and whole-arena verification.
- Measure raw sizes and cold paths.
- Measure compile, link, load, install, restore, and execution.
- Run the full workspace suite.

Gate: existing execution operations stay within normal benchmark noise.

Gate: collected artifacts improve raw size and cold load time.

## 19. Deferred work

These items remain outside version 1.

- A local artifact store.
- A network artifact store.
- Compatible dependency substitution.
- Source module import cycles.
- Cross-runtime snapshot migration.
- Snapshot state diffs.
- Arena reclamation.
- Artifact compression.

None of these items changes the version 1 identity model.

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
