# Reified VMs, Runtime Compilation, and Syntax Trees

Status: accepted design. This document defines the staged implementation.

## 1. Purpose

This document defines Loom metaprogramming through verified code and reified virtual machines.

The design supports these uses:

- an interactive evaluator written in Loom;
- installation of new definitions;
- compatible replacement of late-bound code;
- controlled process upgrades;
- syntax inspection and source tools;
- a future self-hosted compiler.

The design keeps the primitive set small. Loom libraries can add revisions, symbol tables, rollback, and migration policy.

## 2. Main decisions

The implementation uses these decisions:

- `Vm` is a persistent execution image without a result type.
- `Run[T]` is one active invocation with terminal type `T`.
- `RunSnapshot[T]` captures one distinguished run and its reachable image.
- `VmSnapshot` captures one complete VM without a distinguished result.
- An `Artifact` contains portable, untrusted compiler output.
- Independent verification produces a `VerifiedModule`.
- `Vm.install` installs one verified module and returns an `Instance`.
- Installed definitions remain immutable.
- A `Slot` provides optional late binding under one immutable contract.
- Active frames pin exact function versions.
- Future slot operations read the current slot target.
- Static operations keep direct calls and current inlining.
- One general slot model covers functions, classes, values, and processes.
- The compiler emits code but never installs or executes it.
- Compile environments and link environments remain separate.
- Image and run policies control every effect from installed code.
- Public syntax trees are immutable and lossless.
- The public syntax tree does not expose the compiler's Rust AST.

## 3. Why the VM has no result parameter

The old `Vm[T]` type combines an execution image with one invocation.

An execution image can install many modules. It can start runs with different result types.

A single result parameter therefore describes one run, not one VM.

The new split uses these roles:

| Type | Role |
|---|---|
| `Vm` | Installed code, classes, types, slots, policies, and processes |
| `Run[T]` | One active root invocation with terminal type `T` |
| `RunSnapshot[T]` | Reachable VM state with one distinguished `Run[T]` |
| `VmSnapshot` | Complete stopped VM state without one result type |
| `Instance` | One installation of one verified module |

`Vm` remains holder-local. A guest cannot transfer its authority through an ordinary value boundary.

`Run[T]` owns execution state for one invocation. Its frames, heap roots, requests, and terminal value remain typed.

One VM can hold several stopped runs. The scheduler can also run several processes inside that VM.

The runtime stores VM images and runs in separate registries.

Each activation creates one run record. No activation receives a special storage path.

Image records consume the image limit. Run records consume machine and child limits.

A typed run snapshot selects one run inside an untyped captured image.

## 4. Code pipeline

The complete pipeline has these phases:

```text
Source or SyntaxTree
        |
        v
Compiler.Compile with CompileEnv and CompileOptions
        |
        v
Artifact
        |
        v
independent decode and verification
        |
        v
VerifiedModule
        |
        v
Vm.install with LinkEnv
        |
        v
Instance
        |
        v
typed entry lookup and Vm.activate
        |
        v
Run[T]
```

No phase silently performs a later phase.

The compiler cannot mark its own output as verified. The independent verifier creates `VerifiedModule`.

Installation resolves imports and slots. Installation does not execute a guest instruction.

Activation creates the initial frame. Activation does not execute a guest instruction.

Only `Run.step`, `Run.drive`, or `Run.run` executes guest code.

## 5. Three independent authorities

Runtime metaprogramming separates three forms of authority.

`CompileEnv` controls names and contracts visible during compilation.

`LinkEnv` supplies concrete definitions and holder-local slots during installation.

Image and run policies control effects during execution.

Possession of compiler authority grants no file, network, clock, process, or VM-control authority.

Possession of an artifact grants no installation or execution authority.

Possession of a slot grants only the replacement operations allowed by that slot.

### 5.1 Policy layers

`VmPolicy` is a holder-local ceiling for all runs in one VM.

`Run[T]` owns one holder-local `PolicyTable` for routing, mocks, passes, and blocks.

The VM checks its ceiling before it reads the run table. A ceiling denial always wins.

A run-table mock handles an allowed operation without reaching the outer holder.

A run-table pass sends an allowed operation to the VM holder or its next policy layer.

A new `Vm` permits pass-through to its holder. A new `Run` starts with the existing default-deny table.

An image policy cannot grant authority that its holder does not possess.

An image-policy change affects future operations in every run. A run-table change affects only that run.

An active pending operation keeps the decision that routed it.

## 6. Portable code values

### 6.1 `Artifact`

`Artifact` contains canonical bytecode, interfaces, imports, exports, source maps, and linkage declarations.

An artifact is untrusted until verification succeeds.

An artifact has stable semantic and container hashes. It contains no live VM identifier.

### 6.2 `VerifiedModule`

`VerifiedModule` proves that one artifact passed the current independent verifier.

It records the verifier version, semantic hash, ABI hashes, and validated tables.

The type is opaque. Guest code cannot construct or alter it.

A verified module remains portable. It does not name one VM installation.

### 6.3 `Instance`

`Instance` names one module installation inside one VM.

It provides typed lookup for exported definitions, entries, slot specifications, and live slots.

An instance cannot move to another VM. A second VM must install the verified module again.

### 6.4 `FunctionDef`

`FunctionDef` names one immutable function definition.

It carries its complete function scheme, effect row, code identity, and owning instance.

Monomorphic functions can also cross typed APIs as ordinary function values.

Generic definitions use `FunctionDef`. Loom does not need rank-N function values for this API.

The Stage 6 bootstrap surface accepts monomorphic functions without captures or `mut` parameters.

Lookup returns `CodeError` for another function. Later stages can add typed applications without changing the handle representation.

## 7. Static and late binding

Every code reference uses one of two linkage modes.

### 7.1 Static linkage

A static reference pins one immutable definition identity.

The compiler emits the current direct instruction, such as `CALL_STATIC`.

Static calls permit normal verification, devirtualization, and inlining.

An inlined body keeps the identity of the code used during compilation.

### 7.2 Late linkage

A late reference pins a stable `SlotKey` and an immutable `SlotContract`.

The compiler emits a specialized slot instruction.

The first instruction set contains these forms:

```text
CALL_SLOT slot, type_application
NEW_SLOT slot, type_application
LOAD_SLOT slot
SEND_SLOT slot
```

Each instruction reads one dense VM slot. It then performs its specialized operation.

The verifier checks the instruction against the slot contract. It never trusts the current target.

### 7.3 Compile environment selection

`CompileEnv` can bind a source name in static or late mode.

```text
CompileEnv.bind_static(name, interface)
CompileEnv.bind_late(name, SlotSpec)
```

A static binding records an immutable definition identity.

A late binding records one portable `SlotSpec`.

`CompileOptions` can select late linkage for new definitions and selected free names.

Normal package builds use static linkage by default.

Interactive tools use late linkage for their mutable namespace.

Source syntax does not need a `reloadable` modifier in the first implementation.

## 8. Slots

### 8.1 `SlotSpec` and `Slot`

`SlotSpec` is frozen portable metadata. It contains a `SlotKey`, target kind, contract, and optional initial target identity.

`Slot` is a holder-local capability. It names one dense slot in one VM.

A `SlotSpec` can cross artifact and compiler boundaries. A live `Slot` cannot cross VM boundaries.

`Vm.install` maps each required `SlotSpec` through `LinkEnv`.

An installation can also declare a new slot. The VM returns that live slot through the `Instance`.

### 8.2 Replacement

`Vm.replace(slot, target)` checks the target against the slot's immutable contract.

A successful replacement changes only the current target.

A failed replacement changes no VM state.

The operation never edits a frame, closure, object, or immutable definition.

### 8.3 Function contracts

A function contract contains the full type scheme, mutability markers, result type, and effect row.

Replacement accepts equal generic structure and compatible effect behavior.

The first implementation requires exact canonical contracts. Later variance can relax this rule safely.

### 8.4 Class contracts

A class contract contains the complete runtime ABI.

It includes these parts:

- nominal family identity;
- parent ABI;
- generic parameters;
- field order and types;
- interface conformances;
- method selectors and contracts;
- enum representation;
- primitive representation role.

A method-body change can replace method slots without changing the class contract.

A compatible class target must have the same class contract.

A layout or signature change requires a new slot and a new class identity.

Existing objects never change layout.

### 8.5 Value contracts

A value slot has one exact static type. `LOAD_SLOT` copies or references its current value by normal value rules.

Replacing a value affects future loads only. Existing copied values remain unchanged.

### 8.6 Process contracts

A process slot contains mailbox and terminal contracts.

A portable artifact cannot contain a live process target.

`LinkEnv` or a later replacement supplies the first process target.

Replacing a process target affects future slot operations only.

It does not migrate a mailbox, heap, frame, or resource.

Direct process handles keep their existing target.

## 9. Hot replacement semantics

A running frame pins one `FunctionVersionId` when a call creates that frame.

Replacing a function slot does not alter an active frame.

The next `CALL_SLOT` reads the new target. A `CALL_STATIC` keeps its old target.

This rule gives a clear mixed-version boundary.

The VM retains old code while any frame, closure, slot, or snapshot references it.

The VM can reclaim an old version after the final reference disappears.

### 9.1 Hash rules

A static caller hash includes the exact target definition hash.

A late caller hash includes the `SlotKey` and canonical slot contract.

The hash does not include the current slot target.

A class structural hash includes static method identities or method slot contracts according to linkage mode.

### 9.2 Safe replacement points

Holder code can replace a slot while no guest instruction executes in that VM.

A stopped `Run` is a safe replacement point.

A paused process is a safe replacement point.

Guest code cannot mutate the VM that currently executes it.

A supervisor can receive a request and perform replacement before it resumes the run.

## 10. Installation and linking

`LinkEnv` supplies exact static definitions and compatible live slots.

The installer checks these items:

- every static import identity;
- every slot key and contract;
- every class ABI;
- every operation and intrinsic ABI;
- every core image pin;
- every initial target;
- every export declaration.

The installer assigns dense VM-local indices after all checks succeed.

Installation is atomic. A failed installation adds no definition, class, slot, or value.

Two installations of one verified module create distinct `Instance` values.

They can share immutable code storage inside one VM.

## 11. Interactive compilation

### 11.1 Definitions

An interactive definition group compiles with late bindings from the current namespace.

The artifact exports immutable definitions. It can also declare new slots with initial targets.

After installation, a Loom library maps each source name to its returned `Slot` and `SlotSpec`.

The VM does not own this source-name map.

### 11.2 Compatible redefinition

The compiler receives the existing `SlotSpec` through `CompileEnv`.

It emits a new immutable definition with the same contract.

The library installs the definition. It then calls `Vm.replace` on the existing slot.

Old compiled callers use the same slot and see the new target on their next late call.

### 11.3 Incompatible redefinition

An incompatible definition cannot replace the old slot.

The library creates a new slot. It then maps the source spelling to that new slot.

New compilations use the new slot. Old compiled code retains the old slot.

This rule avoids hidden type changes in existing code.

### 11.4 Expressions

An interactive expression compiles as an anonymous entry.

The compiler gives free namespace references late linkage.

The library installs the module and obtains the entry metadata.

A typed caller uses an exact `Type[Fn[A,R,e]]` witness.

A dynamic evaluator can activate an entry that returns `DynValue`.

The VM packages a value at the declared boundary. It does not erase internal operand types.

## 12. Compiler API

The compiler accepts source or one public syntax unit.

```text
Compiler.Compile(SourceText, CompileEnv, CompileOptions)
  -> Result[Artifact, CompileErrors]

Compiler.CompileSyntax(SyntaxNode, CompileEnv, CompileOptions)
  -> Result[Artifact, CompileErrors]
```

`CompileSyntax` treats the selected node as compiler input.

The compiler validates the selected source before it creates an artifact.

Compilation is deterministic under its explicit inputs.

The operation depends on compiler, grammar, core, verifier, operation, and intrinsic versions.

The compiler reads no ambient filesystem or package catalog.

A caller must bind every external interface through `CompileEnv`.

`Compiler.Compile` remains an explicit effect. A policy can block runtime code creation.

The generated program's effect row does not enter the compiler operation's outer row.

The caller inspects the generated row before activation. Image and run policies remain the final authority.

## 13. Public syntax model

### 13.1 Concrete syntax tree

The public syntax API uses an immutable concrete syntax tree.

The tree is lossless and versioned.

It preserves tokens, whitespace, comments, delimiters, source ranges, and invalid fragments.

It shares one immutable source backing by default.

The compiler keeps its private Rust AST separate from this tree.

The public tree gives no identity contract for record indices.

Syntax handles are opaque views over one tree.

The syntax view hierarchy is sealed to core classes.

Programs construct syntax through `SyntaxBuilder`.

Programs cannot construct values from raw source and record fields.

The parser and builder create frozen values.

### 13.2 Structural values

The first API contains these values:

```text
GrammarVersion
SourceRange
SyntaxTree
SyntaxElement
SyntaxNode
SyntaxToken
SyntaxTrivia
SyntaxBuilder
SyntaxDiagnostic
SyntaxParse
ParseStatus
```

`SyntaxParse` contains one tree, one status, and a diagnostic list.

The first implementation uses `String` as its source-text value.

Stable `Int` codes form the low-level syntax-kind API.

Libraries can add typed kind views without replacing these code methods.

`ParseStatus` has `ParseComplete`, `ParseIncomplete`, and `ParseInvalid` cases.

`ParseIncomplete` means more source can finish one open grammar construct.

`ParseInvalid` means the parser found a definite syntax error.

`SyntaxElement.kind()` returns a stable syntax-kind code.

`SyntaxElement.children()` returns elements in source order.

`SyntaxElement.range()` returns its byte range in the source.

`SyntaxElement.text()` returns a text view into the shared source.

`SyntaxElement.detach()` copies one subtree into compact independent backing.

### 13.3 Grammar views and compiler inputs

Typed grammar views belong above the structural tree.

These views can cover definitions, expressions, statements, patterns, types, parameters, and arguments.

A typed view performs a checked projection from `SyntaxNode`.

The first implementation exposes structural views and top-level kind predicates.

It does not make each typed view a separate runtime class.

Definition grouping is a compiler input rule. It is not a syntax-node category.

`Compiler.CompileSyntax` accepts a `SyntaxNode` and validates its selected source.

The compiler normalizes that source into its private Rust AST.

### 13.4 Parsing and REPL policy

The primitive parser has this operation:

```text
Reflect.ParseSyntax(String) -> SyntaxParse
```

A complete tree can contain definitions and statements together.

The parser does not classify complete source as a REPL interaction.

A Loom REPL library inspects the root children and applies its interaction policy.

That library can define expression, definition, incomplete, invalid, and command cases.

Commands such as `Quit` are not Loom syntax nodes.

### 13.5 Transformations

`SyntaxBuilder` creates immutable tokens, trivia, and nodes.

Its leaf methods accept a stable kind code and exact text.

Its node method accepts a stable kind code and child elements.

The node method joins child text in source order.

It writes compact records with new source ranges.

It does not parse the result.

Named methods provide common node, token, and trivia kinds.

`SyntaxNode.with_children()` returns a new node with the same kind.

`SyntaxNode.to_tree()` makes that node the root of a frozen tree.

It reuses backing when the node is already the root.

It compacts a selected child subtree.

The builder checks record structure and kind categories.

The builder can represent invalid syntax on purpose.

Token text does not need to agree with its kind code.

This rule lets tools represent incomplete and invalid edits.

The compiler parses selected text before it creates an artifact.

This parse is an independent compiler validation boundary.

A future persistent backing can share unchanged branches without an API change.

A later `SyntaxEditor` can add higher-level editing operations over this builder.

No current identity rule constrains that editor or an incremental parser.

### 13.6 Retention

A small node can retain a large shared source buffer.

Tools that retain a subtree can call `detach()`.

The implementation can compact automatically only at a documented boundary.

It must not copy each node during ordinary traversal.

## 14. Snapshot rules

The snapshot API has two value types.

| Type | Captured root | Restore result |
|---|---|---|
| `RunSnapshot[T]` | One distinguished `Run[T]` and its reachable machine image | `Run[T]` in a target `Vm` |
| `VmSnapshot` | One complete stopped `Vm` | One stopped `Vm` |

`Run.snapshot()` returns `Result[RunSnapshot[T], SnapshotError]`.

`SnapshotImage` stores no selected run. It stores the complete admitted machine graph.

`RunSnapshot[T]` pairs that image with one typed run selector. The selector belongs to the view, not the image.

A run snapshot captures the run, its reachable processes, and all required installation state.

It does not capture an unrelated run in the same VM.

The distinguished run preserves the result type. The surrounding captured image has no result type.

`Vm.restore(snapshot)` imports a run snapshot and returns its distinguished `Run[T]`.

This form preserves the current branching pattern:

```text
vm = sys.vm.Vm()
case vm.restore(snapshot)
in Ok(run) then use(vm, run)
in Err(error) then report(error)
end
```

The pair `(vm, run)` is the restored image and its distinguished run.

`Vm.snapshot()` requires a safe stopped image. It returns `Result[VmSnapshot, SnapshotError]`.

Restoring a VM snapshot creates one stopped VM. It does not select one run.

Both snapshot forms record these items:

- installed module semantic hashes;
- instance identities and relocation tables;
- slot keys, contracts, and current targets;
- active function version identities;
- class and type identities;
- selected runs, frames, heaps, and pending operations;
- selected process state and resource blockers.

Slot targets are code state. A snapshot records their exact versions.

Portable snapshots grant no effect authority.

They record no live `VmPolicy`, `PolicyTable`, mock closure, or holder capability.

A restored run starts default-deny. Declared birth grants and explicit holder grants apply through existing rules.

A restored VM starts default-deny. Its holder must install its image policy before execution.

Restore admits every referenced verified module before it admits machine state.

Restore rejects a missing code version. It never redirects an active frame through a current slot.

The existing open-resource restrictions remain in force.

## 15. Optional full-image editing

General image editing is not part of the first public API.

A future `VmImage` can expose a stopped VM as data.

Any edit must pass complete admission before execution resumes.

Admission must validate frames, program counters, locals, objects, classes, slots, resources, ownership, and pending operations.

This path supports migrations and debuggers. It is slower than slot replacement.

The VM exposes no unchecked replacement operation.

Arbitrary class-layout edits can invalidate existing objects despite valid bytecode.

## 16. Performance model

Static code keeps the current call, allocation, field, and inlining paths.

A late operation adds one dense slot lookup and one contract-kind assertion in debug builds.

Installation performs all contract checks before execution.

Ordinary execution does not repeat contract comparison.

Immutable code can share storage across instances inside one VM.

Scalar syntax reads share source storage.

`children()` allocates its result list and element views.

The implementation tracks these benchmark groups:

- integer loops and direct calls;
- class allocation and method dispatch;
- static and slot function calls;
- slot replacement;
- module verification and installation;
- core compilation;
- syntax parse, construction, and traversal;
- nested VM and process control.

No stage can regress unrelated static execution outside normal benchmark noise.

## 17. Rejected designs

### 17.1 Keep `Vm[T]`

This type makes one terminal result appear to describe a whole execution image.

It also forces installation, code storage, and invocation into one state transition.

### 17.2 Replace definitions by source name

Source names are library namespace data. They are not stable runtime identities.

Renames, shadowing, module aliases, and incompatible redefinitions make name replacement ambiguous.

### 17.3 Replace immutable content hashes

A content hash identifies one immutable definition. Changing its target breaks its meaning.

Slots provide stable mutable indirection without corrupting content identity.

### 17.4 Make every reference late

Universal late binding adds overhead and blocks normal inlining.

Most package code needs reproducibility, not hot replacement.

### 17.5 Add one replacement API for each value kind

Separate function, class, value, and process tables create inconsistent lifetime and snapshot rules.

One contract-bearing slot model gives one replacement theorem.

### 17.6 Let the compiler install or execute

This design combines code creation with VM authority.

It also prevents independent verification of compiler output.

### 17.7 Expose the private Rust AST

The private AST discards trivia and follows compiler implementation details.

Tools need stable lossless syntax and explicit grammar versions.

### 17.8 Add unchecked replacement

Verified bytecode proves code safety against declared contracts.

It cannot make an arbitrary heap-layout edit safe.

## 18. Safety argument

Let `Gamma` contain every immutable slot contract used by a verified module.

The verifier proves each slot instruction under `Gamma`.

Installation accepts only targets that conform to `Gamma`.

Replacement preserves `Gamma` for the lifetime of each slot.

Future slot operations therefore retain bytecode type and effect safety.

This proof covers language safety. It does not prove application invariants or state migration correctness.

## 19. Implementation stages

### Stage 0: fix the bytecode contract

Specify artifacts, instances, slots, version identities, and snapshot records.

Gate: canonical encoding and identity tests cover every new field and instruction.

### Stage 1: split `Vm` and `Run[T]`

Replace `EmptyVm` with `Vm`. Replace loaded `Vm[T]` with `Run[T]`.

Replace `from_fn` with `activate`. Preserve execution behavior and policy rules.

Gate: all existing VM, process, effect, and snapshot behavior passes under the new types.

### Stage 2: add VM-owned stores

Move code, instance, class, and type ownership into `Vm`.

Give frames and closures stable function version identities.

Split snapshots into `RunSnapshot[T]` and `VmSnapshot`.

Gate: one VM runs two result types without rebuilding its stores. Typed run branching remains unchanged.

### Stage 3: add general slots

Add `SlotSpec`, `Slot`, contracts, slot bytecode, checked replacement, hashes, and snapshot state.

Gate: active frames stay old while later slot calls use a compatible replacement.

### Stage 4: cover every target kind

Apply the slot model to functions, methods, classes, values, and processes.

Gate: incompatible replacements reject atomically. Compatible future operations use the new target.

### Stage 5: make the bootstrap compiler slot-aware

Extend artifacts, interfaces, compile environments, link environments, and the bootstrap compiler.

Gate: static package builds remain byte-identical unless they use new linkage metadata.

### Stage 6: reify verification and installation

Expose `Artifact`, `VerifiedModule`, `Instance`, `SlotSpec`, `Slot`, and `FunctionDef` to Loom.

Gate: a Loom program verifies, installs, activates, and replaces code through typed APIs.

### Stage 7: add the in-language compiler operation

Expose `Compiler.Compile`, compile environments, link choices, options, errors, and dynamic result packaging.

Gate: command-line and in-language compilation produce the same canonical artifact.

### Stage 8: add public syntax trees

Expose lossless syntax values, generic parse status, ranges, traversal, construction, and detachment.

Gate: Loom code can build, classify, compile, install, and run expression or definition source.

## 20. Deferred Loom libraries

The primitive layer does not add these types:

- `CodeRevision`;
- `CodePatch`;
- `FnSymbol`;
- `MetaSession`;
- automatic object migration;
- automatic rollback policy.

Libraries can build these features from artifacts, instances, slots, syntax trees, and VM snapshots.

## 21. Self-hosting path

The bootstrap Rust compiler first produces the canonical artifact format.

A future Loom compiler consumes the same syntax model and emits the same artifact bytes.

Both compilers pass output through the same independent verifier.

Replacing the host compiler service does not change installation, slot, activation, or policy semantics.
