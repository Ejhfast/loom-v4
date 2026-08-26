# Artifact Packages and Portable Snapshots

Status: accepted design. Stages 0 through 2 form the first implementation unit.

This document refines the artifact, linker, VM, and snapshot rules.

It replaces no source-language syntax.

## 1. Purpose

Loom code is a value. A Loom world must also be a portable value.

The current system links one complete program into one `Module`.

The current snapshot stores state against that ambient module.

This design removes that ambient dependency.

It keeps execution tables dense and fast.

It also keeps the standard core outside thin packages.

## 2. Public model

The public model has two main values.

- An `Artifact` contains verified code and exact artifact dependencies.
- A `Snapshot` contains artifacts and runtime state.

`Module` remains the decoded bytecode payload of one artifact record.

`Module` is not the deployment abstraction.

An encoded artifact can include dependency records.

The same artifact can use a thin or fat encoding.

A thin encoding omits dependencies that the loader already has.

A fat encoding includes the complete dependency graph.

Both encodings name the same root `ArtifactId`.

## 3. Artifact records

One artifact record contains these fields.

```text
ArtifactRecord
  module: Module
  dependencies: List[ArtifactDependency]
```

One dependency contains a logical namespace and an exact identity.

```text
ArtifactDependency
  namespace: String
  artifact: ArtifactId
```

The namespace never contains a filesystem path.

The compiler derives it from the logical module or package namespace.

The standard core uses the namespace `core`.

One record cannot bind one namespace twice.

Dependency order has no semantic meaning.

Canonical encodings sort dependencies by namespace and identity.

## 4. Artifact packages

An encoded `Artifact` is a flat package.

It contains one root record and zero or more embedded records.

The package header names the root by `ArtifactId`.

Each embedded record carries its direct dependency list.

The records form a directed acyclic graph in version 1.

The decoder rejects duplicate records and unreachable records.

The decoder uses an explicit work stack for graph checks.

Source module import cycles remain unsupported in version 1.

Definition cycles inside one record remain supported.

The definition collector uses the shared SCC implementation.

### 4.1 Canonical package encoding

The package magic is `LMAR`.

Artifact package format version 1 uses this header.

```text
magic:               4 bytes
format version:      u16
ABI bundle digest:   32 bytes
root ArtifactId:     32 bytes
record count:        u32
```

Records follow in ascending `ArtifactId` order.

Each record uses this encoding.

```text
stored ArtifactId:   32 bytes
dependency count:    u32
dependencies:        repeated dependency records
module length:       u32
module payload:      LMBC bytes
```

Each dependency uses this encoding.

```text
namespace length:    u32
namespace:           UTF-8 bytes
dependency identity: 32 bytes
```

The decoder recomputes each stored `ArtifactId`.

The package contains no trusted identity field.

## 5. Identity

### 5.1 ArtifactId

`ArtifactId` is the semantic identity of one artifact record.

It uses BLAKE3-256 with a distinct domain tag.

It covers these values.

- The bytecode format version.
- The compiler ABI version.
- The ABI bundle digest.
- The module semantic hash.
- Every dependency namespace and `ArtifactId`.

It does not cover source text or optional debug data.

A debug-only edit keeps the same `ArtifactId`.

A dependency edit moves the `ArtifactId`.

### 5.2 BlobHash

`BlobHash` is BLAKE3-256 over the exact encoded artifact bytes.

A thin and fat encoding can share one `ArtifactId`.

They always have different `BlobHash` values.

An encoding or debug-data edit moves `BlobHash`.

### 5.3 Stored identities

The container stores each record identity for graph references.

The decoder recomputes every stored identity.

No stored digest acts as proof.

## 6. Version 1 resolution

Version 1 has no local or network artifact store.

The resolver has two sources.

1. It uses records embedded in the artifact.
2. It can use the runtime standard core.

Every non-core dependency must be embedded.

The resolver identifies the core through the `core` namespace.

A thin core dependency must exactly match the runtime core `ArtifactId`.

The resolver rejects a different thin core identity.

A fat artifact can embed another compatible core.

That core must pass the runtime ABI and verifier checks.

The runtime never substitutes a compatible artifact automatically.

A future store can resolve more exact `ArtifactId` values.

That extension does not change the artifact format.

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

The verifier checks each artifact record before publication.

An imported declaration has no executable body.

The linker resolves imports through artifact dependencies.

An import identifies these values.

- A dependency namespace.
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

An artifact enters the arena through one checked relocation.

Existing indices never move.

The arena deduplicates definitions by their existing identities.

One `CodeNamespace` names one resolved artifact graph.

It records bindings, slots, exports, and core roles.

Each machine stores one `NamespaceId`.

An execution lease pins its namespace view.

Normal instruction dispatch uses dense arena indices.

It performs no hash or namespace lookup.

Two namespaces can bind the same source names differently.

Cross-namespace work uses VM boundary operations.

Direct calls and slot targets stay inside one namespace.

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

`codeof` produces the same artifact record form as source compilation.

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

The artifact section uses the ordinary artifact package format.

The namespace section binds machines to resolved artifact graphs.

The state section stores heaps, frames, mailboxes, policies, slots, and resources.

A thin snapshot can omit the exact runtime standard core.

A fat snapshot embeds every artifact, including its core.

Both forms use the same admission path.

A snapshot never relies on the loader's ambient program.

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

The first pass selects needed artifact records.

The second pass selects definitions inside each selected record.

The definition graph includes every bytecode reference kind.

The graph also includes type, class, interface, conformance, slot, and role references.

The collector uses `lm-scc` for definition cycles.

It uses an explicit work stack.

`use m.f` roots only `f` and its dependency closure.

A module import can root the complete exported module surface.

Keeping a runtime role class does not keep every unused method.

Runtime construction roots only the methods that the runtime can call.

## 16. Resource limits

The decoder checks total bytes before nested decode work.

It checks every count against the remaining input.

It applies limits before verification, identity work, or arena cloning.

The initial limits cover these values.

- Total artifact bytes.
- Artifact record count.
- Direct dependency count.
- Module payload bytes.
- Total decoded code bytes.

Version 1 uses these default limits.

| Limit | Value |
| --- | ---: |
| Total artifact bytes | 256 MiB |
| Artifact records | 4,096 |
| Direct dependencies per record | 4,096 |
| One module payload | 64 MiB |
| Total module payload bytes | 256 MiB |
| One dependency namespace | 4 KiB |

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

- Add `ArtifactId` and `BlobHash`.
- Add canonical dependency bindings.
- Compute identities from decoded content.
- Add bounded artifact decoding.
- Test identity stability and dependency sensitivity.

Gate: stored identities never bypass recomputation.

### Stage 2: thin and fat artifact packages

- Add the flat artifact package encoding.
- Add exact runtime-core resolution.
- Reject missing non-core dependencies.
- Reject cycles, duplicates, and unreachable records.
- Keep the legacy `Module` codec as the payload codec.

Gate: thin and fat forms resolve to the same artifact graph.

Gate: a thin core mismatch rejects before linking.

Gate: a compatible embedded core resolves without ambient core identity.

Stages 1 and 2 do not replace the current execution loader.

Stage 3 makes the compiler and runtime consume the new artifact form.

### Stage 3: contract-safe artifact linking

- Resolve imports across artifact records.
- Compare complete importer and provider contracts.
- Reject executable extern declarations.
- Reject foreign conformance attachment.
- Publish links as one transaction.

Gate: a correct hash with a wrong local declaration rejects.

### Stage 4: shared arena and namespaces

- Add one append-only `CodeArena` to `World`.
- Add `CodeNamespace` and `NamespaceId`.
- Relocate each artifact once.
- Keep execution on dense indices.

Gate: two namespaces can bind equal names to different definitions.

Gate: direct-call performance stays within normal noise.

### Stage 5: artifact-aware dependency collection

- Collect required artifact records first.
- Collect required definitions second.
- Use `lm-scc` for definition cycles.
- Retain sealed families as complete units.
- Restore deep and cyclic collector tests.

Gate: program `1` keeps one local entry and no local core class.

Gate: `use m.f` removes unrelated exports.

### Stage 6: `codeof` and runtime compilation

- Make `codeof` emit a portable definition closure.
- Make runtime compilation emit the same artifact form.
- Install both forms through the artifact linker.

Gate: a function and a class install into an empty VM.

### Stage 7: artifact-backed snapshots

- Replace ambient-program snapshot binding with artifact packages.
- Encode thin and fat snapshots.
- Store namespace manifests.
- Preserve runtime-compiled code and slots.

Gate: an unrelated Loom program restores and drives the snapshot.

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
- Stored verifier verdicts for the new package path.

None of these items changes the version 1 identity model.
