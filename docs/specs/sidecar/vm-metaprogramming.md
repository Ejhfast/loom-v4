# Reified VMs, Runtime Compilation, and Syntax Trees

Status: accepted design. This document defines the staged implementation.

The language specification defines public operation identities and signatures. This sidecar defines detailed behavior and implementation stages.

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
- `VmSnapshot` is the untyped view of one admitted snapshot image.
- An `Artifact` contains portable, untrusted compiler output.
- `Compiler.Verify` performs independent verification and produces a `VerifiedModule`.
- `Vm.install` installs verified modules or portable definitions.
- Module installation returns an `Instance`.
- Definition installation returns an installed binding.
- Each binding retains an immutable definition target.
- `DefinitionSpec` binds new source to one verified definition contract.
- A `Slot` provides optional late binding under one immutable contract.
- `SlotChange` prepares one checked slot update without publishing it.
- `Vm.replace_all` publishes one checked set of slot changes atomically.
- Active frames pin exact function versions.
- Future slot operations read the current slot target.
- Static operations keep direct calls and current inlining.
- One general slot model covers functions, classes, values, and processes.
- The compiler emits code but never installs or executes it.
- Compile environments and link environments remain separate.
- Each run and proc owns one policy table.
- A terminal parent keeps routing authority for its live children.
- Public syntax trees are immutable and lossless.
- The public syntax tree does not expose the compiler's Rust AST.

## 3. Why the VM has no result parameter

The old `Vm[T]` type combines an execution image with one invocation.

An execution image can install many modules. It can start runs with different result types.

A single result parameter therefore describes one run, not one VM.

The new split uses these roles:

| Type | Role |
|---|---|
| `Vm` | Installed code, classes, types, slots, runs, and processes |
| `Run[T]` | One active root invocation with terminal type `T` |
| `RunSnapshot[T]` | Reachable VM state with one distinguished `Run[T]` |
| `VmSnapshot` | Untyped view of one admitted snapshot image |
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
Compiler.Compile with module name, source name, source, CompileEnv, and CompileOptions
        |
        v
Artifact
        |
        v
Compiler.Verify with one Artifact
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

Run and proc policy tables control effects during execution.

Possession of compiler authority grants no file, network, clock, process, or VM-control authority.

Possession of an artifact grants no installation or execution authority.

Possession of a slot grants only the replacement operations allowed by that slot.

### 5.1 Policy routing

Each `Run[T]` and proc owns one holder-local `PolicyTable`.

The table contains exact and group actions for mocks, passes, and blocks.

A mock handles one permitted operation without reaching an ancestor.

A pass continues at the parent table or the embedding host.

The parent relation follows machine creation. Installation does not create or change that relation.

Terminal completion stops machine execution. It does not remove the machine's policy table.

A live descendant can continue through a terminal intermediate parent.

The runtime retains that parent record while a live descendant refers to it.

Table edits affect future requests from the machine and its descendants.

A terminal world root cannot create new host requests. A pass that reaches it denies the request.

A missing or stale parent also denies the request.

An active pending operation keeps the routing decision that accepted it.

Snapshots do not contain policy tables or host authority.

Restore creates fresh default-deny tables for restored machines.

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

A verified module is the storage and verification unit.

It also defines imports, interfaces, nominal class families, and batch installation.

Several module revisions can coexist in one VM.

Installation never mutates an earlier module revision.

### 6.3 Portable definition code

`FunctionCode[A,R]` is one portable function view into a `VerifiedModule`.

`ClassCode` is one portable class view into a `VerifiedModule`.

Both values retain shared verified bytes and validated interface metadata.

Each value also stores one artifact-local definition index.

They do not copy bytecode and do not define a second artifact format.

The definition index is internal to the retained module revision.

It is not a portable name and does not enter a public slot API.

A function view carries its complete function contract and effect row.

A class view carries its nominal identity, class contract, and constructor contract.

These values can cross a value boundary because they contain no VM identifier.

`VerifiedModule.function_code[A,R](name)` returns one typed function view.

`VerifiedModule.class_code(name)` returns one class view.

`VerifiedModule.entry_code[A,R]()` returns the module entry as a function view.

Each lookup checks the requested static contract.

The compiler also provides `codeof` for definitions known at compile time.

`codeof(function)` returns `FunctionCode[A,R]` for a named monomorphic function.

`codeof(Class)` returns `ClassCode` for a class definition.

Reification publishes the selected definition and its local dependency closure.

The scan starts with the selected function body or all selected class bodies.

Class bodies include methods, `init`, and field default expressions.

The scan follows direct calls to named local functions.

It also follows construction and spawning of named local classes.

The scan examines nested closure and process bodies for further named dependencies.

Each named dependency uses its published slot for direct calls or construction.

Definitions outside this closure keep static linkage.

Reflection can recover code for a named capture-free function value.

Capturing closures remain valid activation targets.

They are not portable slot targets because they contain environment state.

Generic definitions require an explicit type application in version 0.2.

`FunctionCode.definition()` returns one frozen `DefinitionSpec`.

`ClassCode.definition()` returns one frozen `DefinitionSpec`.

The specification contains a logical module name, qualified key, definition identity, and verified slot specifications.

The logical module name creates qualified declaration keys.

The diagnostic source name does not enter these keys.

The VM derives this value from verified portable code.

The compiler host re-verifies every supplied slot artifact before it binds the specification.

Source attachments are not required for this operation.

### 6.4 `Instance`

`Instance` names one module installation inside one VM.

It provides typed lookup for exported definitions, entries, slot specifications, and live slots.

An instance cannot move to another VM. A second VM must install the verified module again.

`Instance.entry_binding[A,R]()` returns the installed entry binding.

`Instance.function_binding[A,R](name)` returns one installed function binding.

`Instance.class_binding(name)` returns one installed class binding.

Function lookup accepts an exact qualified binding key.

It also accepts an unambiguous qualified-key suffix.

This suffix form exposes published class methods such as `Box.amount`.

### 6.5 `FunctionDef`

`FunctionDef` names one immutable function definition.

It carries its complete function scheme, effect row, code identity, and owning instance.

Monomorphic functions can also cross typed APIs as ordinary function values.

Generic definitions use `FunctionDef`. Loom does not need rank-N function values for this API.

The Stage 6 bootstrap surface accepts monomorphic functions without captures or `mut` parameters.

Lookup returns `CodeError` for another function. Later stages can add typed applications without changing the handle representation.

### 6.6 `ClassDef`

`ClassDef` names one immutable class definition in one installed VM.

`Instance.class_def(name)` returns `Result[ClassDef, CodeError]`.

The handle carries no object instance. It identifies one verified class target for class-slot replacement.

### 6.7 Installed bindings

`FunctionBinding[A,R]` combines one mutable function slot with one immutable installed function target.

`ClassBinding` combines one mutable class slot with one immutable installed class target.

A binding belongs to one VM image and one module instance.

`binding.slot()` returns its live replacement address.

`binding.spec()` returns its portable slot specification.

`binding.instance()` returns the installation that supplied its immutable target.

`binding.target()` returns that immutable `FunctionDef` or `ClassDef`.

These projections return `CodeError` for stale handles.

`Vm.activate(function_binding, args)` reads the current target from the binding slot.

`Vm.replace(address_binding, target_binding)` writes the target binding's immutable installed target.

The operation does not read the target binding's current slot target.

This rule keeps two installed revisions distinct after either slot changes.

### 6.8 Direct installation

`Vm.install(VerifiedModule, LinkEnv)` returns an `Instance`.

`Vm.install(FunctionCode[A,R], LinkEnv)` returns a `FunctionBinding[A,R]`.

`Vm.install(ClassCode, LinkEnv)` returns a `ClassBinding`.

Direct installation installs the retained module revision as needed.

It returns the binding selected by the portable code value.

A self-contained definition install reuses an exact installed artifact in the same VM image.

The repeated install returns a binding from the existing module instance.

It does not append duplicate code or another module instance.

The installation retains all verified definitions and published slots from that module.

It never rewrites an existing slot.

The returned binding keeps its owning installed instance alive.

Its immutable target can resolve required slots through that instance.

`Vm.install(function)` is convenience syntax for a portable named function.

It returns the same binding type as `Vm.install(codeof(function))`.

The owning instance exposes each published dependency binding used by the installed closure.

```lm
worker = image.install(codeof(Worker))?
service = worker.instance()?
rate = service.function_binding[(Int,), Int]("rate")?
```

Calls from `Worker` to `rate` use the binding returned by this lookup.

```lm
original = image.install(rate)?
replacement = image.install(with_fee)?
image.replace(original, replacement)?
```

Calls through `original` now use `with_fee`.

`original.target()` still returns the installed `rate` definition.

`Vm.replace(slot, function)` uses the retained verified function as the new target.

The convenience forms reject capturing closures and unsupported generic values.

Static callers still use their immutable targets after direct installation.

Only slot instructions observe a later replacement.

New source and syntax can use an existing `DefinitionSpec` without retained source text.

The compiler binds one local declaration name to that verified specification through `CompileEnv`.

The new declaration must reproduce the same qualified key and slot contracts.

The diagnostic source name can differ across revisions.

### 6.9 Activation errors

`Vm.activate` returns `Result[Run[T], CodeError]` for closures, definitions, and function bindings.

The error covers stale code, cross-VM code, bad arguments, unsendable captures, and exhausted run capacity.

`Vm.activate_or_fault` accepts an ordinary closure and returns `Run[T]` directly.

It converts an activation error into a VM fault. Use it only when failure violates a caller invariant.

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

`NEW_SLOT` consumes the declared constructor arguments.

The class slot target contains a nominal class identity and one constructor version.

`NEW_SLOT` calls that constructor version. The constructor allocates and initializes the object.

The bootstrap compiler emits function, method, and class slots from Loom definitions.

Loom version 0.2 has no module-level value or process declaration.

Other verified artifact producers can emit value and process slots.

The Loom VM API can replace these targets after installation.

### 7.3 Compile environment selection

`CompileEnv` can bind a source name in static or late mode.

```text
CompileEnv.bind_static(name, interface)
CompileEnv.bind_late(name, SlotSpec)
```

A static binding records an immutable definition identity.

A late binding records one portable `SlotSpec`.

`CompileOptions` can select late linkage for new definitions and selected free names.

Every exported function and class has a published slot specification.

Publication alone does not change call or construction instructions.

The interface records a separate late-linkage bit for each published binding.

`codeof` and direct named installation set that bit for each named dependency in the local closure.

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

Dense slot indices are internal and can change between compilations.

`Instance.slot_spec(name)` returns the portable specification for one named slot.

`Instance.slot_for(spec)` resolves that specification inside the receiving instance.

Installed bindings provide the same stable lookup without a text name.

The lookup checks the stable key and immutable contract. It never guesses from an integer position.

A function body edit preserves its key. A contract edit creates a new key.

A verified module can find exported function and class slots by name.

Its specification resolves only inside an instance of the exact artifact.

Matching between distinct artifacts requires validated compiler interface metadata.

An internal binding requires compiler interface metadata or a retained `SlotSpec`.

### 8.2 Target categories

One slot table supports five contract categories.

| Slot category | Target | Instruction |
|---|---|---|
| function | `FunctionDef[A,R]` | `CALL_SLOT` |
| method | `FunctionDef[A,R]` with a receiver contract | `CALL_SLOT` |
| class | `ClassDef` and its constructor version | `NEW_SLOT` |
| value | one image-owned value | `LOAD_SLOT` |
| process | `Handle[M,R]` | `SEND_SLOT` |

`FunctionCode` covers function and method definitions.

`ClassCode` covers class definitions and proc subclasses.

A process slot stores a live process handle. It does not store proc class code.

A value or process target cannot be portable inside artifact bytes.

`LinkEnv` or a holder replacement supplies those live targets.

### 8.3 Replacement

Each replacement method checks the target against the slot's immutable contract.

```text
Vm.replace_function(Slot | FunctionBinding[A,T], FunctionDef[A,T] | FunctionBinding[A,T])
  -> Result[(), CodeError]
Vm.replace_class(Slot | ClassBinding, ClassDef | ClassBinding)
  -> Result[(), CodeError]
Vm.replace_value(Slot, T) -> Result[(), CodeError]
Vm.replace_process(Slot, Handle[M,R]) -> Result[(), CodeError]
```

`Vm.replace` selects function or class replacement from its typed arguments.

A successful replacement changes only the current target.

A failed replacement changes no VM state.

The operation never edits a frame, closure, object, or immutable definition.

Each live slot has one monotonic version number.

Every successful single replacement increments that version.

The checked preparation methods return an opaque `SlotChange`.

```text
Vm.change_function(Slot | FunctionBinding[A,T], FunctionDef[A,T] | FunctionBinding[A,T])
  -> Result[SlotChange, CodeError]
Vm.change_class(Slot | ClassBinding, ClassDef | ClassBinding)
  -> Result[SlotChange, CodeError]
Vm.change_value(Slot, T) -> Result[SlotChange, CodeError]
Vm.change_process(Slot, Handle[M,R]) -> Result[SlotChange, CodeError]
Vm.replace_all(List[SlotChange]) -> Result[(), CodeError]
```

`Vm.change` selects function or class preparation from its typed arguments.

A prepared change captures its VM image, slot, target kind, target, and current slot version.

Preparation validates the target contract. It does not publish the target.

`Vm.replace_all` requires changes from one live image.

It rejects duplicate slots, stale versions, invalid targets, and unsafe replacement points.

The VM validates the complete list before it publishes any target.

A failure publishes no target and increments no version.

A success publishes every target and increments every changed slot version.

An empty list succeeds and changes no state.

Another successful update makes an older prepared change for that slot stale.

### 8.4 Function contracts

A function contract contains the full type scheme, mutability markers, result type, and effect row.

Replacement accepts equal generic structure and compatible effect behavior.

The first implementation requires exact canonical contracts. Later variance can relax this rule safely.

### 8.5 Class contracts

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

The contract also contains the constructor type scheme and effect row.

A class slot target contains the compatible class identity and one constructor version.

Field default expressions and `init` bodies belong to the constructor version.

They do not belong to the class ABI.

A method-body change can replace method slots without changing the class contract.

A compatible class target must have the same class contract.

It must also have a constructor that matches the constructor contract.

Nominal identity remains part of the contract. A class from another module does not match.

A layout or signature change requires a new slot and a new class identity.

Existing objects never change layout.

A successful class replacement affects future `NEW_SLOT` operations only.

Future proc spawning also uses the current constructor target.

Existing objects keep their class identity and initialized state.

A class revision can change constructor code, field defaults, and method bodies.

The holder prepares the class slot and each changed method slot.

One `replace_all` call publishes that complete compatible revision.

No caller can observe a new constructor with old changed methods from that batch.

### 8.6 Value contracts

A value slot has one exact static type. `LOAD_SLOT` copies or references its current value by normal value rules.

Replacing a value affects future loads only. Existing copied values remain unchanged.

### 8.7 Process contracts

A process slot contains mailbox and terminal contracts.

A portable artifact cannot contain a live process target.

`LinkEnv` or a later replacement supplies the first process target.

Replacing a process target affects future slot operations only.

It does not migrate a mailbox, heap, frame, or resource.

Direct process handles keep their existing target.

A proc spawned by an image run belongs to the same image.

A nested proc inherits the same image.

Slot instructions in these procs read that image's slot table.

The proc image link keeps the image live until the proc becomes unreachable.

Snapshots preserve this link.

The proc owns its own policy table and parent route.

Its parent can become terminal while the proc remains live.

That terminal parent keeps its table until the final child route disappears.

## 9. Hot replacement semantics

A running frame pins one `FunctionVersionId` when a call creates that frame.

Replacing a function slot does not alter an active frame.

The next `CALL_SLOT` reads the new target. A `CALL_STATIC` keeps its old target.

The next `NEW_SLOT` reads the new constructor target.

A class replacement does not edit an existing object.

This rule gives a clear mixed-version boundary.

The VM retains old code while any frame, closure, slot, or snapshot references it.

The VM can reclaim an old version after the final reference disappears.

A batch does not alter active frames.

Future late calls and constructions read the targets from the published batch.

### 9.1 Hash rules

A static caller hash includes the exact target definition hash.

A late caller hash includes the `SlotKey` and canonical slot contract.

The hash does not include the current slot target.

A class structural hash includes static method identities or method slot contracts according to linkage mode.

### 9.2 Safe replacement points

Holder code can replace a slot while no guest instruction executes in that VM.

A stopped `Run` is a safe replacement point.

A paused process is a safe replacement point.

An executing image proc blocks installation and replacement for its image.

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

Two explicit installations of one `VerifiedModule` create distinct `Instance` values.

They can share immutable code storage inside one VM.

Repeated direct definition installs can reuse one exact self-contained module instance.

## 11. Interactive compilation

### 11.1 Definitions

An interactive definition or recursive definition group uses late namespace bindings.

The artifact exports immutable definitions. It can also declare new slots with initial targets.

The compiler returns a verified module with portable definition views.

The library installs each selected definition directly.

It maps each source name to its returned binding.

The VM does not own this source-name map.

The binding exposes its `SlotSpec` and owning `Instance` for batch operations.

### 11.2 Compatible redefinition

The compiler receives the existing `DefinitionSpec` through `CompileEnv`.

The compile environment maps a local declaration name to that specification.

The compiler emits a new immutable definition with the same qualified key and contracts.

The library installs the new `FunctionCode` or `ClassCode`.

It then prepares the affected slot changes.

One `Vm.replace_all` call publishes a coordinated revision.

Old compiled callers use the same slot and see the new target on their next late call.

The source and syntax paths use this same binding rule.

### 11.3 Incompatible redefinition

An incompatible definition cannot replace the old slot.

The library retains the new binding. It then maps the source spelling to that binding.

New compilations use the new slot. Old compiled code retains the old slot.

This rule avoids hidden type changes in existing code.

### 11.4 Expressions

An interactive expression compiles as an anonymous entry.

The compiler gives free namespace references late linkage.

The library verifies the module and selects its portable entry code.

It installs that entry directly and obtains a `FunctionBinding`.

A typed caller activates that binding with compile-time argument and result types.

A dynamic evaluator can activate an entry that returns `DynValue`.

`Instance.dynamic_entry()` returns `Result[FunctionDef[(),DynValue],CodeError]`.

The VM packages a value at the declared boundary. It does not erase internal operand types.

## 12. Compiler API

The compiler accepts source or one public syntax unit.

```text
Compiler.Compile(String, String, String, CompileEnv, CompileOptions)
  -> Result[Artifact, CompileErrors]

Compiler.CompileSyntax(String, String, SyntaxNode, CompileEnv, CompileOptions)
  -> Result[Artifact, CompileErrors]

Compiler.Verify(Artifact)
  -> Result[VerifiedModule, CodeError]
```

`CompileSyntax` treats the selected node as compiler input.

The first string is the logical module name.

The second string is the diagnostic source name.

The third `Compile` string is the source text.

`CompileSyntax` receives the syntax node as its third argument.

The logical module name creates every `QualifiedKey` in the new artifact.

It is not a filesystem path.

The source name identifies diagnostic spans and debug source records.

It never affects definition identity, slot identity, or compatibility.

`CompileEnv.definitions` maps local declaration names to verified `DefinitionSpec` values.

The compiler rejects a mapped declaration when its qualified key or slot contract changes.

The compiler validates the selected source before it creates an artifact.

Compilation is deterministic under its explicit inputs.

The operation depends on compiler, grammar, core, verifier, operation, and intrinsic versions.

The compiler reads no ambient filesystem or package catalog.

A caller must bind every external interface through `CompileEnv`.

`Compiler.Compile` remains an explicit effect. A policy can block runtime code creation.

`Compiler.Verify` is a separate effect and verifier boundary.

Its group records its compiler-pipeline role. The compiler still cannot approve its own output.

The generated program's effect row does not enter the compiler operation's outer row.

The caller inspects the generated row before activation. Run and proc policies remain the final authority.

### 12.1 Equivalent code paths

Source text and syntax nodes enter the same compiler pipeline.

Both inputs produce the same canonical `Artifact` for equal syntax and compile inputs.

The command-line path uses the same linkage selection as module and runtime compilation.

Verification creates the same `VerifiedModule` representation for both paths.

A compiled program can select a `FunctionCode` or `ClassCode` from that module.

An existing named Loom function can also recover its verified definition origin.

That origin identifies the same shared verified module bytes and local definition index.

Both paths therefore install the same portable definition form.

Both paths compute the same local dependency closure before linkage selection.

`codeof` creates this portable form without a VM.

It references the verified artifact that already contains the running source definition.

The portable form is the primary value for inspection, editing, installation, and replacement.

`Vm.install(function)` is convenience syntax for `Vm.install(codeof(function))`.

Both forms install the same artifact revision and return the same installed binding type.

Dense installed indices and holder-local handles can differ between installations.

Slot keys and contracts remain stable across these paths.

Both paths accept the same logical module name and diagnostic source name.

Both paths apply the same `CompileEnv.definitions` bindings.

### 12.2 Definition source records

An artifact can carry an optional source attachment in its debug section.

The attachment contains logical source names, source text, syntax records, and definition ranges.

It also maps each `codeof` instruction to one stable source origin key.

The key distinguishes named definitions that share one structural function body.

It does not affect the module semantic hash or verification hash.

It does affect the exact container hash.

`FunctionCode.source()` returns `Option[DefinitionSource]`.

`ClassCode.source()` returns `Option[DefinitionSource]`.

`DefinitionSource` contains the selected syntax node and its compile metadata.

The selected node is one definition or one required recursive definition group.

It also contains the diagnostic source name, slot specifications, and declared contract identity.

Tools can inspect or transform that node with the public syntax API.

They can pass the result to `Compiler.CompileSyntax`.

The compiler checks the edited definition against supplied compile bindings.

`DefinitionSource` is an editing convenience. It is not the compatibility authority.

`DefinitionSpec` supplies that authority without source text.

Code without source attachment still installs and executes normally.

### 12.3 Runtime fault locations

Each executable function version can reference one immutable source map.

The map belongs to that exact version and survives later slot replacement.

The interpreter records a function version and bytecode offset when a fault occurs.

It performs no source lookup during ordinary instruction execution.

`Fault.site()` returns the primary `Option[CodeLocation]`.

`Fault.trace()` returns a bounded list of `CodeLocation` values.

`CodeLocation` contains optional path and range fields, a function identity, and a bytecode offset.

Its `path` field has type `Option[String]`.

Its `range` field has type `Option[SourceRange]`.

A stripped artifact reports its function identity and bytecode offset with two `None` source fields.

The trace contains at most 64 locations in callee-to-caller order.

An asynchronous operation retains its perform location until completion.

A host fault therefore reports the guest operation site when one exists.

Fault locations are diagnostic metadata. They do not affect fault codes or effect rows.

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
| Full `VmSnapshot` | One complete stopped `Vm` | One stopped `Vm` |

`Run.snapshot()` returns `Result[RunSnapshot[T], SnapshotError]`.

`SnapshotImage` stores one optional distinguished-run selector and one optional full-VM selector.

A run snapshot stores the distinguished-run selector and its result-type digest.

A full VM snapshot stores the full-VM selector. It stores no distinguished-run selector or result type.

Machine ordinal zero has no selection meaning in a full VM snapshot. It is only the first serialized machine record.

A run snapshot captures the run, its reachable processes, and all required installation state.

It does not capture an unrelated run in the same VM.

The distinguished-run selector records its result-type digest.

A full VM image records no result type.

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

The exact operation is `Vm.SnapshotVm`.

`sys.vm.restore_vm(snapshot)` creates one stopped VM. The exact operation is `Vm.RestoreVm`.

Restoring a VM snapshot does not select one run.

Both snapshot forms record these items:

- installed module semantic hashes;
- instance identities and relocation tables;
- slot keys, contracts, and current targets;
- immutable targets retained by live installed bindings;
- active function version identities;
- class and type identities;
- selected runs, frames, heaps, and pending operations;
- selected process state and resource blockers.

Slot targets are code state. A snapshot records their exact versions.

Snapshots also retain reachable prepared `SlotChange` values.

A restored change keeps its captured version.

A restored stale change remains stale and fails during publication.

A class slot records both its nominal class identity and constructor version.

Portable snapshots grant no effect authority.

They record no `PolicyTable`, mock closure, or holder capability.

A restored run starts default-deny. Declared birth grants and explicit holder grants apply through existing rules.

A restored machine starts with a fresh default-deny table.

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

A host can cache the latest verified installed-code aggregate during repeated external admissions.

The cache key includes the base verification hash, artifact bytes, and provider relocation maps.

Each load still decodes and admits all mutable image state.

The cache retains only one aggregate.

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

Apply the VM and bytecode slot model to functions, methods, classes, values, and processes.

The bootstrap source compiler emits function, method, and class slots.

Verified artifact producers can emit value and process slots.

Gate: incompatible replacements reject atomically. Compatible future operations use the new target.

### Stage 5: make the bootstrap compiler slot-aware

Extend artifacts, interfaces, compile environments, link environments, and the bootstrap compiler.

Published binding metadata can change artifact bytes.

Gate: unrelated calls remain static and retain their prior execution cost.

### Stage 6: reify verification and installation

Expose modules, instances, slots, immutable targets, and installed bindings to Loom.

Expose `Compiler.Verify` as the independent verification boundary.

Gate: a Loom program verifies, installs, activates, and replaces code through typed APIs.

### Stage 7: add the in-language compiler operation

Expose `Compiler.Compile`, compile environments, link choices, options, errors, and dynamic result packaging.

Gate: command-line and in-language compilation produce the same canonical artifact.

### Stage 8: add public syntax trees

Expose lossless syntax values, generic parse status, ranges, traversal, construction, and detachment.

Gate: Loom code can build, classify, compile, install, and run expression or definition source.

### Stage 9: add portable definition views

Expose `FunctionCode`, `ClassCode`, definition lookup, and direct installation.

Retain one verified origin for named functions and classes.

Gate: a Loom function installs without a source string or module-level public edit.

Gate: direct installation returns a stable binding with both address and revision target.

Gate: all source and syntax compilation paths use identical publication and late-linkage rules.

### Stage 10: retain source attachments

Encode optional source text, syntax records, definition ranges, and source maps.

Exclude this attachment from semantic and verification hashes.

Gate: a function maps to editable syntax and compiles through `CompileSyntax`.

### Stage 11: report fault locations

Capture the exact function version and bytecode offset when a fault occurs.

Map the trace through optional source attachments outside the execution hot path.

Gate: static, replaced, and asynchronous faults report the correct source revision.

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
