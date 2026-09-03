# Loom Language Specification

Status: version 0.2 design specification  
Source form: UTF-8 text, conventional extension `.lm`  
Artifact form: canonical bytecode module, conventional extension `.lma`  
Snapshot form: serialized machine image, conventional extension `.lms`

This specification defines an object language with a reified compiler, reified virtual machines, immutable code identity, explicit effect rows, runtime policy tables, snapshots, and isolated procs. “Must” and “must not” are normative. Text labeled *implementation note* describes the reference implementation without changing observable semantics.

This document defines the current language and reference implementation.

---

## 1. Governing model

Two rules govern the language.

**Types describe; the VM decides.** The type/effect checker proves static facts. An effect row is an upper bound on operations code can request. A type never grants authority. Every actual operation request is decided by the controlled machine's policy table or by the holder manually driving that machine.

**Machine state is data.** Guest frames, locals, operands, program counters, pending requests, and suspended continuations are explicit VM-owned records. Guest calls never rely on a live host-language call stack. Stepping, snapshots, nested VMs, migration, inspection, and procs all follow from this representation.

There is one guest-to-host boundary primitive: calling an operation object. Printing, files, clocks, randomness, networking, compilation, VM control, snapshots, reflection, and proc communication all use it.

### 1.1 Semantic, library, and implementation layers

A conforming distribution keeps five semantic layers distinct.

1. **Language primitives.** Syntax and structural types that cannot be written as ordinary declarations: unit, `Never`, `Any`, scalar machine types, tuples, function types, operation types, local mutation capability, and the bytecode/runtime machinery required to execute them.
2. **Core image.** A pinned artifact contains ordinary source and native class declarations. It defines `Option`, `Result`, `Ordering`, `Range`, VM events, and errors. Tuple carrier classes expose methods on primitive tuples. These definitions are not parser keywords. Stable core role slots identify them (5.2).
3. **Prelude.** A deliberately small set of names implicitly introduced during name resolution. The prelude re-exports selected primitive, native-core, and core-image names; it does not define their identity and does not automatically import general algorithms or host wrappers.
4. **Standard library.** Explicitly linked ordinary modules provide collections, text, files, networking, codecs, VM helpers, compilation, reflection, and testing. Standard code cannot bypass effect rows or policy.
5. **Host operations.** Fixed members of `sys.*`. They may suspend, are policy-gated, and their exact identities appear in rows.

This separation breaks the bootstrap cycle cleanly. The bootstrap compiler knows primitive types and the native-class manifest. It compiles the core image first. Host-operation signatures then resolve against core role slots. Thus, an operation can return `Result[Option[String], IoError]` without compiler built-ins for these classes.

A wrapper may make an operation convenient; it cannot make an effectful action pure or grant authority.

### 1.2 Reference implementation shape

The reference implementation is Rust, built as a Cargo workspace with strict dependency direction:

```text
lm-core-image / lm-std / self-hosted compiler
                    |
             verified artifacts
                    |
lm-cli + lm-host + lm-proc + bootstrap compiler
                    |
lm-vm + lm-jit + lm-link + lm-bytecode + lm-verify + lm-value + lm-graph
```

The VM and compiler use stable Rust. Unsafe code is permitted only in small audited modules that implement object storage or foreign-function shims; parser, checker, verifier, policy, snapshot validation, and host-operation routing remain safe Rust. No guest pointer is exposed as a Rust reference across an allocation, collection, suspension, or host call.

Pure native intrinsics are deterministic core operations such as arithmetic, strings, lists, maps, freezing, and digests. They do not suspend, do not consult policy, and have the empty row. The standard library is ordinary language code except for the declared native-core classes and intrinsic bodies.

### 1.3 Conformance boundary

Source syntax, static checks, artifact validity, snapshot validity, hashes, operation identities, boundaries, policies, and faults are normative.

Collector strategy, host thread-pool shape, and internal caching are not observable. Section 22.9 defines observable text and byte storage charges.

---

## 2. Lexical structure

### 2.1 Source encoding and identifiers

Source is UTF-8. An initial byte-order mark is ignored; another is invalid. Invalid UTF-8 is a compile error.

Version 0.2 restricts identifiers to ASCII:

```text
[A-Za-z_][A-Za-z0-9_]*
```

Spelling is case-sensitive. Standard style uses initial capitals for classes, enums, cases, and effect groups, but capitalization has no semantic force.

### 2.2 Whitespace, separators, and comments

Spaces, tabs, carriage returns, and newlines separate tokens. A newline or semicolon separates body expressions.

Delimiters, strings, and unfinished operators suppress this separator.

A line comment starts with `#` and extends to the newline. There are no block comments in version 0.2.

### 2.3 Keywords

```text
and as break case class continue def do effect else elsif end enum escaping
false for if in loop mut not or return self super then true use while with const
```

`sys` is a prebound ordinary value, not a keyword.

`final` and `frozen` are contextual modifiers only before `class`.

They are ordinary identifiers in all other positions.

### 2.4 Numeric literals

Integers may be decimal, hexadecimal, octal, or binary; underscores may separate digits:

```lm
0
42
1_000_000
0xff
0o755
0b1010_0011
```

`Int` is signed 64-bit. Unary `-` is not part of the literal token. Constant overflow is a compile error; runtime checked-integer overflow faults with `IntegerOverflow`.

A literal containing a decimal point or exponent is `Float`, stored as IEEE 754 binary64:

```lm
0.0
3.141_592
1e9
2.5e-3
```

A float literal is correctly rounded to nearest, ties to even. Runtime float arithmetic uses binary64 round-to-nearest/ties-to-even with no excess precision or implicit fused operation. Every NaN result is normalized to the canonical quiet-NaN encoding; signed zero is preserved by arithmetic. There is no implicit conversion between `Int` and `Float`.

### 2.5 Characters, strings, and bytes

A character literal contains one Unicode scalar value:

```lm
'a'
'\n'
'\u{1f642}'
```

Character literals use string escapes. A `\xNN` character escape must stay in the ASCII range.

A string is immutable UTF-8 text:

```lm
"hello"
"line one\nline two"
"braces {stay literal}"
"Hello #{name}!"
```

Plain braces do not start interpolation.

The marker `#{ expression }` starts one interpolation.

The body uses the normal expression scanner. It permits strings and balanced nested braces.

An interpolation body cannot contain a source comment. The comment would consume the closing brace.

The compiler rejects nested interpolation before scanner recursion exceeds its fixed limit.

The expression must produce a value that implements `Display`.

`Display.append_to` writes into one `StringBuilder` without an intermediate `String`.

`Int`, `Bool`, `Char`, and every `Text` value use native builder operations.

The pure `display(value)` helper builds one standalone `String`.

The escape `\#{` produces the literal text `#{`.

Double braces have no special meaning.

String escapes are `\\`, `\"`, `\'`, `\n`, `\r`, `\t`, `\0`, `\xNN`, and `\u{HEX}`.

A string `\xNN` escape must encode an ASCII byte from `00` through `7f`.

A byte string is immutable bytes:

```lm
b"LM\0\x01"
b"\x00\xff"
```

Byte strings accept direct ASCII and the common one-byte escapes.

Their `\xNN` escapes accept every byte from `00` through `ff`.

Byte strings do not interpolate expressions.

Raw regular-expression literals start with `re"` and end at the next unescaped double quote:

```lm
re"[a-z]+"
re"(?P<word>\p{Greek}+)"
re"a\"b"
```

The scanner copies every character between the delimiters without Loom escape processing.

A backslash and its next character remain in the regular-expression source.

Regular-expression literals do not interpolate expressions and cannot contain a line break.

Version 0.2 reserves triple-quoted strings.

### 2.6 Punctuation and operators

Punctuation:

```text
( ) [ ] { } , : . ; | ?
```

Operators:

```text
= == != < <= > >= + - * / % & | ^ << >> >>> ~
```

A left brace followed by a pipe starts a brace closure. Every other left brace starts a map literal. The empty form `{}` remains an empty map.

`and`, `or`, and `not` are short-circuit Boolean operators.

Section 6.4 defines the sealed operator hooks.

---

## 3. Modules, compilation environments, and linking

### 3.1 Source module

A source module is a sequence of top-level definitions followed by at most one trailing expression:

```lm
class Greeter
  def greet(self, name: String): String
    "Hello #{name}!"
  end
end

def twice(x: Int): Int
  x * 2
end

do |name: String|: String
  Greeter().greet(name)
end
```

Top-level definitions are `class`, `enum`, `def`, and `const`.

There are no mutable module variables, effectful initializers, or runtime namespace installation.

A constant has this form:

```lm
const RETRY_LIMIT: Int = 3
const STATUS: (Int, String) = (200, "ready")
```

Its value contains one literal or one tuple of constant values. A leading minus is valid only on a numeric literal.

The declared type must accept the value. Each use copies the typed literal into the consuming expression.

An imported constant use adds a pin-only import slot. The slot carries no runtime definition.

A constant has no function body, runtime slot, or module state. Its typed value forms part of the exported module surface.

All definitions are exported by source name. The optional trailing expression becomes the module entry value.

Inside a package, `src/main.lm` holds the program entry. Every other module must end without a trailing expression. The `src/` file tree defines module paths. A cross-package module path starts with the manifest package name.

### 3.2 Predeclaration and recursion

All top-level type and value names are predeclared before bodies are checked, permitting mutual recursion. A name may be defined once in its namespace. Enum arm labels live in the constructor namespace and do not consume ordinary top-level value names.

### 3.3 Fixed bindings, the core image, and free names

Every module is compiled against exactly one pinned core image. Primitive types and core definition identities require no user import slot. Only the names selected by the current prelude are introduced unqualified; other core definitions are available through their canonical core binding when explicitly named.

The following require no ordinary import slot:

- primitive and structural types;
- native core class declarations from the pinned core image;
- selected core definitions and constructors re-exported by the prelude;
- `sys`, the frozen host-operation object;
- syntax bindings such as `self` and `super` in valid contexts.

The standard library is not ambient. A package or explicit compile environment supplies every `std/*` module it uses. Every other free name must be defined by the module or supplied in the explicit compile environment.

The `use` declaration is the source-level surface of this rule. A `use` line binds one dotted path to a short name. A module use creates a named import slot. The build tool fulfills that slot. `use` never grants authority or changes an effect row.

A `use` path starts at a root name. The root set is fixed per module: the dependency keys of the manifest, this package's own top-level modules, `std`, and `sys`. A collision inside the root set is a compile error, and the fix is a manifest rename; resolution never picks silently. A path that names a module binds that module, and every export of it resolves under the bound name. A path that names one export of a module binds that export.

A module alias can qualify an enum case. For example, `keys.Key.Enter` names `Enter` from the `Key` family.

One import slot names the providing module, the exported name, the kind, and the pinned interface hash. A compiler checks the importing module against the interface alone, and never against the implementation of the provider. The linker resolves each slot and rejects a provider whose interface hash differs from the pin.

### 3.4 Primitive compile operation

`CompileEnv` contains provider modules, source-root mappings, and verified definition bindings:

```lm
env = CompileEnv(
  List[VerifiedModule](),
  List[(String, String)](),
  List[(String, DefinitionSpec)]()
)
options = CompileOptions(
  is_main: true,
  dynamic_result: false,
  late_definitions: false,
  late_functions: List[String](),
  late_classes: List[String]()
)
result = sys.compiler.compile("interaction", "interaction.lm", source, env, options)
```

Conceptually:

```text
Compiler.Compile(String, String, String, CompileEnv, CompileOptions)
  -> Result[Artifact, CompileErrors]
```

The first string is the logical module name.

The second string is the diagnostic source name.

The third string is the source text.

Each provider is a `VerifiedModule` with validated compiler interface metadata.

A root pair maps one source root to one provider module prefix.

The compiler records only referenced imports. It captures no provider instance or runtime value.

Dynamic compiler tools request a declared `DynValue` result. They do not widen normal APIs to `Any`.

### 3.5 Entry execution

The compiler lowers a trailing expression into the module entry function.

Verification and installation do not execute this function.

`Instance.entry` returns its typed `FunctionDef`. `Vm.activate` creates a run for that definition.

`Instance.entry_binding` returns a typed installed binding. Activation through that binding reads its current slot target.

The entry function carries the effect row of its source expression.

### 3.6 Installation and linking

Inside a `Result`-returning callable, installation can use propagation:

```lm
image = sys.vm.Vm()
provider_instance = image.install(provider_module)?
links = LinkEnv([provider_instance])
program_instance = image.install(program_module, links)?
```

Module installation returns `Result[Instance,CodeError]`.

Definition installation returns a typed installed binding.

`LinkEnv` contains installed provider instances from the same VM.

Installation validates imports, signatures, pinned hashes, slot contracts, and class identities.

Installation executes no guest instruction. A failed installation changes no VM state.

Missing, duplicate, or incompatible providers produce `CodeError` values.

Linking a program merges its modules into one closed artifact with an empty import table. The merge is pure: it installs no global name, performs no host operation, and reads no file. A module with an unresolved import slot never executes: the loader admits an artifact only with an empty import table. The merged artifact meets the whole verifier before it runs.

**The class table.** The linker compares two classes on QualifiedKey and StructuralHash (3.7, 8.6). The table is exhaustive:

| QualifiedKey | StructuralHash | Result |
| --- | --- | --- |
| same | same | merge into one class |
| same | different | reject: conflicting implementations |
| different | same | keep distinct |
| different | different | keep distinct |

The second row rejects two implementation versions of one qualified name. The rejection names both providers and the rebuild. The third row keeps `mathlib.Vec2` and `app.Point` distinct although their structures are equal. Row 3 keeps two class slots, because two class keys need two runtime classes.

**The function table.** A function value is identified by StructuralHash (3.7). A named function binding maps a qualified name to a function value, and several bindings may name one function value. The linker compares the binding key and the StructuralHash of the function each key names:

| Binding key | StructuralHash | Result |
| --- | --- | --- |
| same | same | share the binding and the code |
| same | different | reject: conflicting providers |
| different | same | keep both bindings, share the code |
| different | different | keep both bindings and both code objects |

The two tables differ in row 3 only, and the difference is the whole reason a function needs no QualifiedKey. Two equal bodies are one function value, so the core image every module carries becomes one core, the generated constructor stubs of the abstract enum parents share one code object, and a core value keeps its class across a module boundary. The two bindings stay, so a report names the function the source named.

A generated construction function takes the binding `<class key>.<new>`. A class StructuralHash covers no constructor, because a constructor is a function value of its own, and a field default and an `init` body live inside it. Row 2 of the function table is therefore the rule that rejects two providers of one class key whose constructors differ.

### 3.7 Definition and module identity

Four identities answer four questions about one definition: **QualifiedKey**, **StructuralHash**, **InterfaceHash**, and **VerificationHash**. Section 8.6 states them for a class. Each consumer names the one it needs, and no consumer reads a value another consumer owns.

Three rules govern how a name meets an identity. A name sits on the left of the arrow: it is a binding that points at an identity, and it is never a part of that identity.

> **QualifiedKey is the nominal identity of a class.**
>
> **A function value is identified by StructuralHash.**
>
> **A named function binding maps a qualified name to a function value.**

A **QualifiedKey** is the fully qualified declaration path of a class, for example `mathlib.geometry.Point`. The package name of the manifest supplies the root, never the dependency key. Two classes are the same nominal class when their QualifiedKey values are equal. A single source file has no module path, so its declarations carry one-segment keys.

A **named function binding** is a pair of a qualified name and a function value. A free function takes the binding `<module path>.<name>`, a method and an `init` take `<class key>.<name>`, and a generated construction function takes `<class key>.<new>`. A closure body and the entry take no binding. Several bindings may name one function value, so `a.first` and `b.second` stay two names of one shared code object. A binding key never enters a StructuralHash. It lives beside the code, and the linker reads both (3.6).

A **StructuralHash** covers canonical bytecode and constants, full signature and effect row, referenced definition identities, import requirements and pinned hashes, compiler ABI version, and intrinsic semantics version. It never covers the definition's own name.

**The naming rule.** A declaration name never enters a structural definition hash. A name may enter an interface hash, a namespace hash, or a qualified key. The shorter claim "no name in any hash" is wrong: an interface hash must contain names, because an importer agrees with a named API.

A reference to a class inside canonical bytecode names that class by QualifiedKey, never by the structural identity of the referenced class. Two signatures that name two structurally identical classes therefore receive different structural hashes. This rule stays inside the naming rule, because it covers a referenced nominal identity and never the declaration's own name.

Canonical bytecode is a dedicated identity encoding, not the loading encoding. It replaces every module-global index — function, class, type, string, application, and selector — with content identity, a qualified key, inline content, or structural encoding. Definition hashes therefore do not depend on definition order in the source or on pool interning order.

For mutually recursive definitions, the compiler finds strongly connected components. A component labels its members by structural refinement, and no name and no source order enters the rule:

1. The first label of a member is the hash of the member bytes, with every reference inside the component replaced by one fixed placeholder.
2. The next label of a member is the hash of its current label plus the current labels of the members it references. References keep their position order inside the member; a member never sorts its own references, because `f(g(x))` and `g(f(x))` differ.
3. Refinement stops as soon as the label partition stops refining. The round count is capped at the member count.
4. The final label is the StructuralHash of the member. The component hash is the hash of the sorted final labels.

The set of components is a property of the graph, so the emission order of the component walk is invisible in every hash. The rename rule is therefore narrower than "a rename moves no definition hash", and it reads:

- A **function binding** rename moves no code hash. A binding name lives outside every structural hash, so the function value keeps its identity and so does every caller.
- A **class key** rename and a **selector** rename can move a referenced hash. A type digest and a `New` site name a class by qualified key, and a method entry names a selector by name. A source class rename moves the key, so it moves the hash of every definition that names that class. Its own key never enters its own hash directly, so a class that never reaches its own key keeps its hash through a rename. A class does reach its own key through a method that names the class, a field whose type names it, or an enum arm the parent lists, and the class hash then moves with the rename.
- A **VerificationHash** includes each published slot key. A source rename changes this hash when it changes a published binding. An export-label edit alone preserves this hash. A selector rename also changes this hash.

Structural refinement cannot always give each member a unique label. The stable partition of this rule is bisimulation: two members keep one label exactly when they are bisimilar. Bisimulation is coarser than isomorphism, so the rule may give one label to two members an isomorphism test separates. One label stays sound, because a member is a deterministic system with ordered successors: two bisimilar members have identical unfoldings, so they compute the same thing. Members with one label share one StructuralHash, and their QualifiedKey values keep them distinct wherever distinctness is observable.

Refinement runs on untrusted input before the verifier, so its work is bounded twice: once per component and once per module. A component or a module past its bound rejects with a clear diagnostic. The bound is large enough that no source program reaches it.

A **method** takes part in its class identity as the pair of the selector name and the implementing function identity. Selector identity is therefore name-based and independent of any method body. An override with a different body keeps the selector name.

An **InterfaceHash** covers only the exported name, the kind, and the full signature, with class references by qualified name. It covers no method body and no function body. Import slots pin interface hashes. An edit to an exported body therefore moves the StructuralHash of that body and no interface hash, and no dependent module recompiles. The linker resolves an import slot to a definition, and it rejects a slot whose provider interface hash differs from the pin.

For a constant, InterfaceHash covers the declared type and literal value.

A **VerificationHash** is the exact resolved input of the verifier. It covers the semantic region, the operation manifest digest, and every resolved input the verifier reads. It answers one question: did the verifier approve this exact representation? A host that caches a verified admission keys on this value and on no other. The verifier reads resolved slots and structures, never a source name, so a rename moves no VerificationHash.

*Implementation note.* The reference implementation uses Tarjan's algorithm in an iterative form with an explicit work stack; the definition graph is untrusted input, so the walk must not grow the host stack. Traversal order is pinned: roots in ascending definition index, successors in ascending reference order. Tarjan emits components callees-first, and that emission order is the hash schedule: every referenced definition hash is complete before a component is labeled. The hashes themselves do not depend on the traversal order, because the partition and the member labeling are canonical.

A module semantic hash covers definitions, entry code/type, imports, and format version. It excludes source spelling, comments, paths, embedded source, source maps, and debug sections. A separate container hash covers exact bytes.

A StructuralHash is not injective over source programs. Two structurally identical classes share one value, and symmetric members of one component share one value. A lookup that must select one definition therefore states a deterministic tie rule, and no security property rests on the absence of ties. The linker separates such classes by QualifiedKey.

---

## 4. Names and scopes

A function, method, closure, branch, loop, and case arm introduces a lexical scope.

### 4.1 Namespaces

Version 0.2 has four statically resolved namespaces:

1. **types:** classes, enums, generic parameters, and fixed types;
2. **ordinary values:** locals, functions, class values, imports, and `sys`;
3. **effect descriptors:** groups such as `Vm` and identities such as `Clock.Now`;
4. **enum constructors:** canonically qualified names such as `Option.Some` and `Result.Err`.

An unqualified enum constructor is accepted only when the expected enum or case scrutinee selects one arm unambiguously.

### 4.2 Local declarations and assignment

The first assignment to an unresolved simple name in a scope declares a local:

```lm
count = 0
names: [String] = []
```

A local's type is fixed after declaration. Later assignment requires a compatible value. A branch-local declaration does not leak into the containing scope. A local used after conditional assignment must be definitely assigned on every path.

### 4.3 Resolution order

A simple ordinary value name resolves through innermost locals, parameters, current module values, compile-environment imports, then fixed bindings. There is no implicit field lookup and no ambient runtime lookup.

### 4.4 Closure capture

A closure captures referenced outer values at closure creation, not mutable local slots. Rebinding an outer local later does not alter an existing capture. A captured binding cannot be rebound inside the closure, though the captured object retains the mutable/read-only capability available at capture time.

A closure crossing a machine boundary must be sendable; its capture graph is checked by the boundary codec.

---

## 5. Types

The type system is intentionally small enough to implement directly and strong enough to make code identity, operation rows, bytecode verification, and typed manual VM driving useful. It is nominal for user data, structural only where the runtime already needs structure, locally inferred, and free of trait solving or global Hindley-Milner inference.

### 5.1 Type strata

The type universe has four strata.

**Primitive and structural types** are part of the language:

| Type | Meaning |
|---|---|
| `()` | Unit, with value `()` |
| `Never` | Bottom type; an expression that cannot return normally |
| `Any` | Explicit dynamic supertype of all ordinary values |
| `Bool` | `true` or `false` |
| `Int` | Signed 64-bit integer |
| `Float` | IEEE 754 binary64 |
| `Byte` | 0 through 255 |
| `(T,)`, `(T, U)`, ... | Fixed-arity structural tuples |
| `(A, B) -> R with e` | Function type with effect row |
| `Op[id, (A, B) -> R]` | Identity-indexed operation type |

**Native core classes** have ordinary nominal identities but runtime-supplied storage or methods:

| Type | Meaning |
|---|---|
| `Text` | Sealed abstract UTF-8 text base |
| `String` | Immutable UTF-8 text |
| `Substring` | Immutable shared UTF-8 text view |
| `Char` | Immediate Unicode scalar value |
| `Bytes` | Immutable byte sequence |
| `Digest` | 256-bit digest |
| `List[T]` | Mutable growable contiguous sequence; `[T]` sugar |
| `Map[K,V]` | Mutable insertion-ordered map; `{K: V}` sugar |
| `StringBuilder` | Mutable UTF-8 builder |
| `ByteBuffer` | Mutable byte builder |
| `Fault` / `FaultCode` | Frozen machine-fault diagnostic and stable code |
| `Class[T]` | Class value constructing `T` |
| `PolicyTarget` | Sealed non-callable parent used by non-granting table edits |
| `Operation` / `OperationGroup` | Exact-operation and group policy descriptors; both subtype `PolicyTarget` |
| `DynValue` | Explicit type/value package for dynamic APIs |
| `Artifact` | Opaque untrusted bytecode container |
| `VerifiedModule` | Portable verified module revision |
| `FunctionCode[A,T]` | Portable verified function code |
| `ClassCode` | Portable verified class code |
| `DefinitionIdentity` | Contract and implementation identities for one definition |
| `DefinitionSpec` | Verified identity and slot contracts for one definition |
| `DefinitionSource` | Optional syntax and verified identity data for portable code |
| `SourceRange` | Half-open byte range in one source text |
| `CodeLocation` | Function identity, bytecode offset, and optional source location |
| `FunctionDef[A,T]` | Installed function definition |
| `ClassDef` | Installed class definition |
| `SlotChange` | Opaque checked update for one live slot |

**Core-image nominal types** are ordinary source definitions with pinned hashes.

The minimum set includes `Option`, `Result`, `Choice`, `Ordering`, `Range`, tuple carriers, `StepEvent`, `DriveEvent`, and `Recv`.

It also includes `Set` and the core collection interfaces.

It also includes portable operation errors and typed VM request tokens.

**Host and holder types** include `Vm`, `Run[T]`, waits, snapshots, proc handles, policy tables, and resource handles.

Each native holder type has explicit boundary rules.

There is no `nil`. Absence is `Option[T]`; ordinary failure is `Result[T,E]`; machine failure is `Fault` observed by the holder.

### 5.2 Core image versus prelude

`Option` and `Result` are not parser-known special forms. Their declarations are ordinary sealed enums in the core image:

```lm
enum Option[T]
  Some(v: T)
  None
end

enum Result[T, E]
  Ok(v: T)
  Err(error: E)
end
```

`Option.Some` uses its bare payload as its runtime value.

`Option.None` uses the native empty-case value.

An artifact carries a **core role table**: one class slot per stable core role, for example `Option`, `Option.Some`, and `Option.None`. The compiler fills the table, the linker relocates it, and the verifier proves the kind, the generic arity, the parent slot, and the exact field layout of every filled slot. A rule that needs a core family, such as the pending-call type of a `Call` pattern, reads a slot. It reads no name and no hash, so a rename changes nothing the verifier reads, and an artifact with no source resolves its core from its own bytes. A family whose parent slot is filled must fill every arm slot.

Pattern matching and exhaustiveness use the same enum machinery as user enums. The host ABI reads the same slots, so `Io.ReadBytes` and user code cannot silently disagree about what `Result` means.

The prelude puts common result, range, and collection names into unqualified scope.

These collection names include `List`, `Map`, and `Set`.

Removing a future prelude name does not change its core identity.

### 5.3 Nominal classes and inheritance

A class introduces a nominal instance type and a value of type `Class[InstanceType]`:

```lm
class Animal
end

class Dog < Animal
end
```

`Dog <: Animal`. Inheritance is single. A class identity is its normalized sealed definition closed over dependency hashes. An instance's runtime class slot resolves to that identity.

`final class` rejects subclasses.

`frozen class` implies `final class` and rejects a parent.

A frozen class permits `mut self` only on `init`.

Every frozen field type must always be frozen.

Successful initialization marks the instance frozen.

An override must preserve parameter types and `mut` markers, may narrow the result type, and may narrow but never widen its row. These restrictions make a call checked against a supertype sound without dynamic row checks.

### 5.4 Generics and representation

```lm
class Box[T]
  value: T

  def init(mut self, value: T)
    self.value = value
  end
end
```

Class arguments are invariant. Type parameters can declare nominal interface bounds.

Interface applications are invariant in both type arguments and effect arguments.

Version 0.2 has no higher-kinded parameters, specialization syntax, or user-declared variance.

Top-level functions and methods may declare type and effect parameters.

An interface application places type arguments in brackets. Each effect argument follows one `with` keyword.

A bare interface application supplies an empty row for each effect parameter.

Use `+` between several interface bounds. Use commas between class conformances.

A conformance can declare type parameter premises after `when`.

```lm
final class Box[T] implements Display when T: Display
```

Each premise subject must name one class or enum type parameter.

A method can declare the same premises after its effect row.

The checker and verifier limit recursive conformance resolution to 128 applications.

An interface can extend several interfaces after a colon. Commas separate these parent interfaces.

Each inherited method and associated requirement remains part of the child contract.

Identical method contracts from several bounds merge into one callable requirement.

Different contracts with one method name remain ambiguous.

An interface method can provide a default body.

A declaration without a body remains a required method.

The default can declare type parameters, effect parameters, and a `when` clause.

A conforming class method with the same complete contract replaces the default.

Two unrelated defaults with one selector require an explicit class override.

The compiler uses no implicit interface linearization order.

Bare `Self` names the conforming type inside an interface contract.

Bare `Self` names the declared nominal application inside a class or enum.

A normal class that conforms to a `Self`-dependent interface must be final.

An enum family can conform because its family is closed.

An interface name is a bound. It is not a value type.

Version 0.2 has no existential interface value.

The core `Display` interface controls string interpolation.

```lm
interface Display
  def append_to(self, mut builder: StringBuilder)
end
```

The core `PartialEq` interface controls user-defined value equality.

```lm
interface PartialEq
  def __eq__(self, other: Self): Bool
end

interface Hashable: PartialEq
  def __hash__(self): Int
end

interface Comparable: PartialEq
  def compare(self, other: Self): Ordering
end

interface Copyable
  def copy(self): Self
end

interface Error: Display
end
```

Core also defines composable arithmetic interfaces.

```lm
interface Add
  def __add__(self, other: Self): Self
end

interface Subtract
  def __sub__(self, other: Self): Self
end

interface Multiply
  def __mul__(self, other: Self): Self
end

interface Divide
  def __div__(self, other: Self): Self
end

interface Negate
  def __neg__(self): Self
end

interface Number: Add, Subtract, Multiply, Divide, Comparable
  def min(self, other: Self): Self
  def max(self, other: Self): Self
end

interface SignedNumber: Number, Negate
  def abs(self): Self
end
```

`Number` defines shared operations. Each conforming type defines overflow, division, ordering, and exceptional-value behavior.

`SignedNumber` adds negation and absolute value. It does not define a common representation.

`Int` and `Float` implement `SignedNumber`.

`Display`, `Hashable`, and `Copyable` remain independent capabilities.

`hash_of` returns the stable semantic hash of a `Hashable` value.

`hash_combine` combines one field hash with an ordered seed.

One unparenthesized row item can follow `with`. Parentheses group an empty row or a row with several items.

An associated type can declare several interface bounds. The selected type must satisfy every bound.

Code can use methods from every associated bound. A shared method name is ambiguous without a more specific operation.

Generic definitions are checked once with type variables and share one bytecode body. Loaded applications receive dense type and class slots. Reflection, boundary validation, and field signatures use these slots. Generic elements retain the canonical `Value` representation.

Type arguments are inferred only when a unique solution follows from arguments and expected result:

```lm
empty = List[String]()
value = identity[Int](1)
```

Ambiguous calls require explicit arguments; the checker never searches arbitrary conversions or implementations.

### 5.5 Tuples

Tuples are fixed-arity immutable structural values:

```lm
point = (10, 20)
single = ("only",)
```

`()` is unit, not a zero-field heap tuple. Tuple elements are covariant and addressed only by compile-time position. Tuples are used for lightweight returns, map entries, and typed operation argument packs. Their maximum portable arity is 16; larger records must be classes.

Core declares native carriers named `Tuple2` through `Tuple16`.

Conformance and method lookup view each structural tuple through its carrier.

Tuple representation remains `Type::Tuple`, `BcType::Tuple`, and `Object::Tuple`.

Each carrier conditionally implements `Display`, `PartialEq`, `Hashable`, and `Comparable`.

### 5.6 `Any`, `DynValue`, and deliberate dynamic boundaries

Every ordinary value can widen to `Any`, but normal generic APIs must use a type parameter rather than `Any`. In particular, list algorithms, `freeze`, `digest`, `deep_equal`, VM results, proc messages, compile environments, and operation replies preserve their caller's type.

`Any` is a primitive name but prelude and standard APIs do not return it merely for convenience. It appears only in code that intentionally does a dynamic type test. Narrowing is explicit:

```lm
if value is String
  text = value as String
end
```

`is` is pure. `as` returns the same value or faults with `BadCast`.

Truly dynamic APIs use the opaque `DynValue` package.

The compiler creates this package for a declared dynamic result.

`DynValue.render()` gives a bounded text representation.

`DynValue` preserves its hidden exact type. It does not expose that type to guest code.

A local `DynValue` is digestible when its packaged graph is digestible.

A cross-machine dynamic reference is holder-local and not digestible.

Version 0.2 has no general type descriptors, dynamic unpacking, or value reflection.

### 5.7 Function, operation, and effect-variable types

A function type includes parameters, result, and row:

```lm
(String, Int) -> Bool
(Bytes) -> Result[Int, IoError] with Io.Write
(T) -> U with e
```

The checker normalizes source function syntax to the structural form `Fn[A,R,e]`, where `A` is the fixed argument tuple, `R` the result, and `e` the row. `Fn` is ABI/type-checker metanotation rather than an additional source type name; it lets native APIs such as `Vm.activate` use ordinary first-order generics instead of a variadic or dependent typing rule. Function parameters are contravariant, results covariant, and effects covariant by set inclusion.

An operation object has an identity-indexed callable type:

```lm
Op[Io.Write, (Bytes) -> Result[Int, IoError]]
Op[e, (Bytes) -> Result[Int, IoError]]
```

There is no callable operation type that erases identity. Widening to `Operation` makes it non-callable and suitable only for inspection, equality, diagnostics, or non-granting policy APIs.

An effect variable ranges over a finite operation set and may appear in function types, operation types, and rows:

```lm
def map[T, U, effect e](
  xs: [T],
  f: (T) -> U with e
): [U] with e
  xs.map(f)
end
```

There is no row subtraction or runtime effect value.

### 5.8 Subtyping

Subtyping is the least relation containing:

- reflexivity and transitivity;
- `Never <: T` and `T <: Any`;
- declared nominal inheritance;
- element-wise tuple covariance;
- function parameter contravariance and result covariance;
- effect-row inclusion;
- equality only for invariant generic applications.

Different instantiations of one generic class are unrelated unless all arguments are equal. `List[Dog]` is not a `List[Animal]`, because mutation would make that unsound.

The implementation interns all types and answers subtype queries by memoized structural recursion plus a precomputed nominal ancestor table. There is no user-programmable subtyping rule and no backtracking solver.

### 5.9 Bidirectional checking and local inference

The checker uses two judgments:

```text
Γ; M ⊢ e ⇒ T ! ρ      synthesize expression type and row
Γ; M ⊢ e ⇐ T ! ρ      check expression against an expected type
```

`Γ` maps names to immutable type bindings. `M` records local mutation capability, initialization state, and flow refinements. `ρ` is the finite row computed for the expression.

The compiler infers:

- local binding types from initializers;
- literal element/key/value types from members or expected type;
- closure results when omitted;
- generic call arguments by first-order unification;
- branch joins using the least available declared supertype other than `Any`; `Any` is chosen only when the expression is explicitly checked against `Any` or annotated as such.

It does not infer public APIs globally. Fields, method parameters, top-level function parameters, and non-unit public results are annotated. Recursive functions must declare a result and row before their bodies are checked. Omitting a function or method result means `()`; omitting a closure result requests local inference.

A direct function parameter type is nonescaping by default.

The `escaping` marker gives that parameter an ordinary function type.

Function types in fields, results, locals, and containers remain escaping.

A nonescaping parameter can be called or passed to another nonescaping parameter.

It cannot be returned, stored, captured, or used inside a type argument.

An unannotated closure can infer an expected polymorphic effect row from its body.

A standalone or ambiguous effectful closure requires an explicit `with` clause.

Generic inference and literal/branch joining never invent `Any` merely because two types are unrelated; source must request an explicit dynamic boundary. No implicit numeric conversion, truthiness conversion, string conversion, or user-defined coercion exists. A failed local constraint produces one diagnostic at the smallest expression whose expected and synthesized types disagree.

### 5.10 Flow refinement, patterns, and exhaustiveness

Within the true branch of `value is T`, the binding is refined to the intersection represented by the tested nominal subtype. The original type returns after the branch. A cast does not permanently retype the source binding.

A `case` arm refines the scrutinee and binds constructor fields at their declared types. Exhaustiveness is proven for `Bool` and sealed enums. Guardless duplicate or unreachable arms are diagnosed. Ordinary class hierarchies are open and therefore require `_` unless the static scrutinee type is a sealed enum family.

Definite return analysis proves that a non-unit callable returns on every normal path. `Never` branches do not contribute to a join.

### 5.11 Initialization and local mutation proof

For each constructor the checker tracks every required field as uninitialized or initialized across the control-flow graph. Before completion, `self` may assign fields, read initialized fields, and perform the required `super.init`; it may not escape. All required fields must be initialized on every normal exit.

Static `mut` is a local permission proof, not an alias theorem. It proves only that this reference may be used at a mutating call or write site. It does not prove uniqueness, freedom from races outside the one-thread VM model, or that another alias cannot freeze the object. The runtime frozen bit remains authoritative.

### 5.12 Effect proof

Each callable is checked under a declared row. The body row is the union of:

- exact operations directly performed;
- declared rows of statically selected callees;
- instantiated effect variables of higher-order callees;
- the operation or group granted by a typed `PolicyTable.pass` call.

The checker requires `computed ⊆ declared`. Recursive strongly connected components are checked against their already declared signatures; there is no whole-program fixed-point inference. An override may not widen its inherited row. A callable operation value always retains its identity parameter.

The resulting theorem is:

> For verified bytecode produced from a checked callable with row `ρ`, every `PERFORM` reachable in any execution names an operation in `ρ`.

The VM does not re-check this theorem at run time. Policy remains a separate dynamic decision.

### 5.13 What the checker proves—and what it does not

| Proven before execution | Deliberately dynamic or unproven |
|---|---|
| every local, field, call, return, and constructor argument has a compatible type | list/map bounds and integer overflow |
| no implicit nil and enum/Boolean cases are exhaustive | policy grants or operation success |
| required fields are initialized before `self` escapes | deep frozenness, sendability, digestibility, or resource liveness |
| writes and mutating calls use a locally mutable reference | uniqueness or alias freedom |
| a direct `for` source has no direct mutation in its body | mutation through an alias or opaque call |
| scoped designators do not enter an escaping position | general lifetime inference or borrow checking |
| overrides preserve call and row substitutability | termination, fuel use, heap use, or asymptotic cost |
| every possible perform is contained in the declared row | absence of faults, deadlock, host failure, or timing side channels |
| typed manual replies match the selected operation at the source level | a stale or cross-VM request token; the runtime still validates it |
| emitted bytecode has typed stack/local states | correctness of externally supplied bytes until the verifier accepts them |

A native ABI declaration may mark a type as scoped. The scoped marker is a property of the type. A scoped designator may flow through local aliases, function parameters, and closure parameters. A parameter with a scoped type is a scoped designator inside its callable, and the same escape rule applies there. A scoped designator cannot be returned, captured from an outer scope, stored in an object or collection, frozen, transferred, digested, sent, or snapshotted. A scoped type is not a valid generic type argument. This is one native capability rule, not a general lifetime system. Bytecode records the scoped marker, and the verifier checks every move into an escaping location.

### 5.14 Checker and verifier construction

The compiler represents primitive, nominal, tuple, function, operation, and type-variable forms as an interned immutable type DAG. Names resolve to dense definition IDs before type checking. Typed HIR stores resolved field, selector, class, operation, and intrinsic IDs; later phases never repeat textual lookup.

Each callable is lowered to a control-flow graph. Definite assignment, return analysis, and stack-shape planning are forward dataflow problems with finite states. Effect rows are sorted small sets of operation IDs with an inline representation for common rows and an interned representation for larger rows.

The source checker and bytecode verifier are independent implementations over different representations. The verifier reconstructs local/operand type states at block entries, checks joins, calls, fields, intrinsics, performs, scoped-designator escape, and claimed rows, and rejects malformed external code before it enters the verified-code cache. Source types are erased from the execution hot path to dense slots after verification; ordinary instruction dispatch performs no general subtype lookup.

## 6. Expressions

Every executable source construct is an expression. Definitions remain module members. Many expressions evaluate to `()`.

### 6.1 Blocks, assignment, and calls

A body is a sequence of expressions. Its value is the last expression, or `()` if empty. `return` exits the nearest callable.

A newline can open any control body. `then` can open a conditional or pattern body.

`do` can open a loop body. A `while` or `for` body needs a separator after `do`.

`end` closes each body form. The same expression grammar applies inside every body.

A callable with result `()` discards its final expression value and returns `()`.

An expression in discarded position has no result constraint. Each arm of a discarded control-flow expression follows the same rule.

Assignment declares or rebinds a local. It can also write a permitted field. Assignment evaluates to `()`.

An assignment is valid in every expression position. A new local belongs to the innermost containing body.

`return`, `break`, and `continue` have type `Never`.

A `return` operand must match the callable result type. A `break` operand must match the enclosing `loop` result type.

An expression cannot complete normally when a mandatory operand has type `Never`.

An expression after an expression that cannot complete is unreachable. The compiler does not perform termination analysis.

```lm
x = 1
x = 2
self.name = "Ada"
```

Calls use parentheses. Arguments evaluate left to right; the receiver evaluates first. Labeled arguments follow positional arguments and match declared parameter names:

```lm
f(1, 2)
obj.method(1)
vm.activate_or_fault(program, args: ("Ada",))
```

A label names one declared parameter. Labels can appear in any order after the positional arguments. A call rejects an unknown name, a repeated name, a name that a positional argument already fills, and a positional argument after a label. A label changes nothing in the call ABI.

A native method declares parameter names, so `list.push(value: x)` and `vm.answer(call: c, value: v)` follow the same rule. A closure value carries no names, and a direct operation call names no parameter. Both take positional arguments only.

A call may place one closure after its closing parenthesis. That closure becomes the final argument:

```lm
files.with_open(path, options) { |file|
  file.read_all(max_bytes: 1_000_000)
}

files.with_open(path, options) do |file|
  file.read_all(max_bytes: 1_000_000)
end
```

The two trailing forms have identical precedence and evaluation order. A call accepts at most one trailing closure. A trailing closure must start on the call line. A newline after the call ends the expression first (2.2). Only `?` can follow a trailing closure. There is no overload resolution.

### 6.2 Closures

```lm
increment = do |x: Int|: Int
  x + 1
end

printer = do |text: String| with Io.Write
  print(text).expect("the output writes")
end

thunk = do || 42 end
```

A brace closure is an equivalent spelling:

```lm
increment = { |x: Int|: Int x + 1 }
thunk = { || 42 }
```

Both forms lower to the same typed HIR node and bytecode form. They have identical capture, result, row, and evaluation rules.

A closure is a sealed function object containing code identity and captures.

Omitting `with` means an empty row unless contextual polymorphic row inference applies.

An explicit `with ()` always declares an empty row.

A monomorphic top-level function name produces a zero-capture function value.
A generic function name needs a direct call in this version.

### 6.3 Fields, `self`, and `super`

`receiver.field` is statically resolved. `self` exists only in methods. A mutating method declares `mut self`. `super.method(args)` calls the immediate superclass implementation with the same receiver and a compile-time selector.

### 6.4 Arithmetic, bitwise operations, comparison, and equality

Final core classes provide the built-in operator methods.

The checker maps each supported source operator to one sealed method.

```text
-a      -> a.__neg__()
~a      -> a.__invert__()
not a   -> a.__not__()
a + b   -> a.__add__(b)
a - b   -> a.__sub__(b)
a * b   -> a.__mul__(b)
a / b   -> a.__div__(b)
a % b   -> a.__rem__(b)
a & b   -> a.__and__(b)
a | b   -> a.__or__(b)
a ^ b   -> a.__xor__(b)
a << b  -> a.__shl__(b)
a >> b  -> a.__shr__(b)
a >>> b -> a.__ushr__(b)
a == b  -> a.__eq__(b), when the left type implements PartialEq
a != b  -> not a.__eq__(b), through the same conformance
a < b   -> a.__lt__(b)
a <= b  -> a.__le__(b)
a > b   -> a.__gt__(b)
a >= b  -> a.__ge__(b)
```

Each core method body names one pure intrinsic manifest entry. Static resolution and trivial-body inlining emit the canonical instruction.

`Text + Text` uses `Text.__add__` and produces String.

Any class may declare the arithmetic, bitwise, and ordering hooks.

The operator reads the hook from the class of the left operand.

For a type parameter, the operator reads the hook from its declared interface bounds.

The checker rejects the operator when no unique bound supplies the hook.

The call takes the ordinary method path:

- the declared parameter type checks the right operand;
- the declared result type is the type of the operator expression, and it needs no relation to the operand types;
- the declared effect row is charged to the caller, so `a + b` can perform an operation, and the caller must hold the row;
- a `final` class calls directly, and any other class dispatches virtually.

A class that declares no arithmetic or ordering hook keeps the rules below.

```lm
final class Money
  cents: Int

  def init(mut self, cents: Int)
    self.cents = cents
  end

  def __add__(self, other: Money): Money
    Money(self.cents + other.cents)
  end
end

Money(150) + Money(250)
```

An explicit `PartialEq` conformance activates a user-defined `__eq__` method.

A method with that name does not activate value equality by itself.

The contract is pure. It accepts the conforming type and returns `Bool`.

A normal user class with this conformance must be `final`.

The compiler resolves a final receiver statically.

A generic receiver uses one verified interface call.

Native values retain their existing equality instructions.

The `!=` operator never reads a `__ne__` method.

Built-in structural and identity equality remain language rules.

They do not create an implicit `PartialEq` conformance.

`Hashable` extends `PartialEq` for map and set keys.

`digest` and `std.value.deep_equal` never call a user method.

Text map keys use their visible UTF-8 content. A `String` key and a `Substring` key match when their visible content matches.

The specification uses `BorrowedKey[K,use]` as metanotation for one closed key relation.

It is not a source interface. Programs cannot add relation entries.

| Input | Declared key | Use |
| --- | --- | --- |
| a subtype of `K` | `K` | every key use |
| `Text` | any text type | lookup |
| `Text` | `String` | insertion |
| one validated UTF-8 `Bytes` range | `String` | pool interning |

`has`, `get`, `at`, `remove`, and map indexing use `BorrowedKey[K,lookup]`.

`put` uses `BorrowedKey[K,insertion]`.

A borrowed lookup never changes the stored key. A hit allocates no guest object.

A borrowed String insertion retains the stored key after a hit.

A miss creates one bounded String key.

Iteration, `keys_list`, and snapshots expose the declared key type `K`.

They never expose the borrowed input.

Other map insertions require the declared key type.

Other classes use reference identity for direct equality unless they implement `PartialEq`.

`and` and `or` remain control-flow operators. They evaluate the right operand only when required.

`Int` uses checked `+`, `-`, and `*` operations.

Its `/` truncates toward zero. Its `%` keeps the dividend's sign.

Division by zero faults. The one overflowing division case also faults.

`Float` arithmetic follows the binary64 rules in section 2.4.

Float division by zero produces infinity or NaN.

Float remainder uses truncating division and keeps the dividend's sign.

There is no implicit numeric conversion.

The `Int` operators `&`, `|`, `^`, and `~` use all 64 payload bits.

The operator `>>` shifts right with sign extension.

The operator `>>>` shifts right with zero extension.

All shift amounts must be from 0 through 63.

An invalid shift amount faults with `ShiftOutOfRange`.

`Int.wrapping_add`, `wrapping_sub`, and `wrapping_mul` use two's-complement wrapping arithmetic.

`Int.rotate_left` and `rotate_right` rotate all 64 payload bits.

An invalid rotation amount faults with `ShiftOutOfRange`.

`Int.count_ones` counts set bits in the 64-bit two's-complement value.

`Int.leading_zeros` and `trailing_zeros` return 64 for zero.

`Int.signum` returns -1, 0, or 1.

`Bytes` implements elementwise `&`, `|`, `^`, and `~`.

Both binary operands must have equal lengths.

A length mismatch faults with `LengthMismatch`.

`Bytes` does not implement shift operators.

`Bool` does not implement bitwise operators.

`Int.to_float` rounds to binary64 with nearest-even rounding.

Every `Int` with magnitude through 2^53 converts exactly.

`Float.to_int` truncates toward zero.

It returns `NonFinite` or `OutOfRange` when no `Int` result exists.

`Float.bits` returns the signed `Int` view of the canonical binary64 bits.

`Float.from_bits` accepts the same signed bit view and canonicalizes every NaN.

`Float.abs` clears the sign bit and canonicalizes NaN.

`Float.min` and `max` propagate NaN.

They select negative zero for a minimum and positive zero for a maximum.

`Float.sqrt`, `floor`, `ceil`, and `trunc` use their binary64 operations.

`Float.round` rounds to the nearest integer value. A tie rounds to the even value.

Advanced Float operations use the pinned `lm-math` algorithm revision. Version 0.2 uses `libm` 0.2.16 without architecture-specific routines.

`Float.mul_add` performs one fused operation. It does not round the multiplication separately.

`Float.atan2` treats its receiver as the vertical coordinate. Its argument is the horizontal coordinate.

Every advanced operation canonicalizes a NaN result. Finite rounding can differ between target architectures.

`Float.is_finite` rejects infinities and NaN.

`Float.is_infinite` accepts only positive and negative infinity.

Ordering requires equal numeric types. Float ordering follows ordered IEEE comparison and is false when either operand is NaN. Language equality for floats is total and hash-friendly: both signed zeros are equal and all canonical NaNs are equal. Strings compare lexicographically by Unicode scalar value; bytes lexicographically by unsigned byte.

`==` and `!=` otherwise compare scalars, strings, bytes, and digests by value; class values, operation identities/groups, and zero-capture top-level functions by canonical hash; captured closures, ordinary instances, lists, maps, VMs, handles, and resource descriptors by VM-local reference identity.

Tuples compare structurally. Two tuples are equal when their types are equal and every element pair is equal under these same rules. Tuple equality requires equal static tuple types, and its element comparisons never widen: an element position compares by the rule for its declared element type. This matches the canonical digest encoding, which treats a tuple as its ordered elements.

Deep graph equality is `std.value.deep_equal` and requires frozen digestible values.

### 6.5 Tuples, lists, maps, and indexing

```lm
point = (10, 20)
numbers = [1, 2, 3]
empty: [String] = []
counts = {"a": 1, "b": 2}
```

A one-element tuple requires a trailing comma: `(value,)`. Parentheses without a comma remain grouping.

List elements and map keys/values require a common non-`Any` type unless the literal is explicitly expected as an `Any`-containing collection. Unrelated members do not silently produce `List[Any]` or `Map[K,Any]`. Empty literals need an expected type. Maps preserve insertion order. `list[index]` and `map[key]` are faulting access; non-faulting `get` returns `Option`. Indexing a tuple is allowed only with a compile-time integer literal.

### 6.6 Precedence

The operator groups use this order, from strongest to weakest:

1. postfix call, field, index, `?`, and trailing closure;
2. unary `not`, `-`, and `~`;
3. multiplicative operators;
4. additive operators;
5. shifts;
6. `&`;
7. `^`;
8. `|`;
9. ordering operators;
10. equality, `is`, and `as`;
11. `and`;
12. `or`;
13. assignment.

Assignment is right-associative. Other binary operators are left-associative.

---

## 7. Control flow

### 7.1 Conditionals

```lm
value = if ready
  compute()
elsif fallback
  default_value()
else
  fail_value()
end
```

Conditions must be `Bool`. If used as a value, all normal branches must have a common type. Omitting `else` is allowed only where the resulting type is `()`.

`then` can open an arm on the same line:

```lm
width = if value > limit then limit else value end
```

A newline can open the same arm as a block. `then` and a newline do not create different expression categories.

A discarded conditional does not join its arm types. The checker still checks each arm independently.

### 7.2 Loops

```lm
while condition
  body()
end

for item in items
  use(item)
end

loop do
  body()
end
```

`while` and `for` have type `()`. Their `break` expressions cannot carry values.

The compiler always gives `while` a condition-false exit. It does not inspect the condition value during type checking.

An inline `while` or `for` body can use a semicolon separator:

```lm
for item in items(); use(item) end
```

`do` can precede the separator. It opens a loop body only before a newline or semicolon.

`continue` starts the next iteration of the nearest loop. It has type `Never`.

`loop` has the join type of the `break` values that it owns. A bare `break` contributes `()` to this join.

```lm
result = loop do
  value = next()
  if valid(value) then break value end
end
```

A `loop` with no `break` that it owns has type `Never`. `Never` is a subtype of every type.

The checker uses sibling break types to resolve constructor inference. It does not search for a result type.

A loop result is mutable only when every value-producing break result is mutable.

A discarded `loop` does not join its break value types. Each break value remains independently type-checked.

An expression after a loop with no normal exit is unreachable. Unreachable expressions are compile errors.

Only a normal `break` exits `loop`. A `return` exits its callable and contributes no loop value.

A callable that never completes and holds no `return` states no value of its declared result type. Its result type must be `Never` or `()`; any other declared type is a compile error, because nothing in the body produces one.

```lm
def serve(): Never
  loop do
    handle()
  end
end
```

### 7.3 Case

```lm
case value
in Some(v) then use(v)
in None
  fallback()
end
```

Arms are tested in source order. An arm can use `then` with an ordinary expression body. It can also use a newline body.

Cases over enums and `Bool` are checked for exhaustiveness. Cases over other types require a wildcard or binding arm.

Duplicate unreachable arms are compile errors.

### 7.4 Select

```lm
select
in child.drive_wait() -> event
  handle_drive(event)
in self.receive_wait() -> command
  handle_command(command)
end
```

A select has at least two arms. Each arm expression has type `Wait[T]`.

The arm name has type `T`. `_` discards that arm's result.

The compiler lowers select to `Wait.choose`, `Wait.wait`, and `Choice`. Section 23.9 defines their operations.

The runtime tests ready arms in source order whenever the proc resumes.

`sys.wait.any(waits)` selects from a nonempty `List[Wait[T]]`.

It returns `(index, value)`, where `index` identifies the winning list entry.

The call consumes every wait root. It tests roots in list order.

Losing drive waits keep all work completed before withdrawal.

An exact operation can define `wait` through its manifest entry.

For `Op[op, (A...) -> R]`, `wait(A...)` returns `Wait[R]` and charges `op`.

The checker rejects `wait` when the operation manifest does not permit selection.

---

## 8. Classes and objects

### 8.1 Declaration and sealing

```lm
class Hello
  name: String = ""

  def set_name(mut self, name: String)
    self.name = name
  end

  def say_name(self) with Io.Write
    print("Hello #{self.name}!")
  end
end
```

At `end`, the definition is sealed. Fields, layout, superclass, generic parameters, methods, selectors, signatures, and bodies can never change. A class cannot be reopened.

The optional `final` modifier prevents subclass declarations:

```lm
final class Token
end
```

Classes remain open for inheritance when they omit `final`. Enum cases remain final without this modifier.

### 8.2 Fields

A field has a mandatory type and optional default:

```lm
count: Int = 0
name: String
items: [String] = []
```

Defaults are pure, cannot use `self`, and are evaluated separately for each instance in source order. They may refer to fixed bindings, imports, and module definitions. Mutable defaults create fresh objects.

Fields are readable by code with the class's static definition. Writes require a mutable reference and an unfrozen target.

### 8.3 Initializers

A class may define one `init`:

```lm
class Point
  x: Int
  y: Int

  def init(mut self, x: Int, y: Int)
    self.x = x
    self.y = y
  end
end
```

`init` uses `mut self` and returns `()`. Calling `Point(2, 3)` allocates, evaluates defaults, invokes `init`, and returns the instance. Its row is the initializer row.

Every field without a default must be assigned on every normal path, exactly once before first read. Until all required fields are initialized, `self` may only receive field assignments, read initialized fields, and participate in the required `super.init`. It may not escape by return, capture, store, argument, or ordinary method call. An initializer fault leaves no observable partial instance.

A class without explicit `init` receives a zero-argument initializer only when all fields have defaults.

### 8.4 Methods

Every source method is an instance method and explicitly names `self` as its first source parameter. `self` has no written type annotation; the containing class supplies it.

```lm
def size(self): Int
  self.items.len()
end

def add(mut self, value: String)
  self.items.push(value)
end
```

There are no user-defined static methods, overloads, optional or variadic parameters, runtime method installation, or bound-method extraction. Fixed native methods on class values may exist where the specification names them—most notably proc-class `spawn` sugar—but source classes cannot add such methods. Use a closure for a callback:

```lm
callback = do |x: Int| obj.update(x) end
```

### 8.5 Inheritance and dispatch

Inheritance is single and nominal:

```lm
class Child < Parent
end
```

Ordinary non-final definitions may be subclassed. A subclass inherits fields and methods and may add both. A final definition rejects every subclass.

An override must keep parameter types and `mut` markers. It may narrow the result and row.

Constructor signatures are not inherited. A subclass initializer handles inherited required fields and may call `super.init(...)` exactly once before reading fields initialized by it.

A call selector is fixed at compile time; the runtime class selects the sealed implementation. Computed selectors are not representable.

### 8.6 Class identity

A class value is frozen. Four identities answer four questions about one class. Each consumer names the one it needs.

- **QualifiedKey** — the nominal identity. The value is the fully qualified declaration path, for example `mathlib.geometry.Point`. Two classes are the same nominal class when their QualifiedKey values are equal. The linker uses this value. The type checker never compares it, because it works on class indices inside one module.
- **StructuralHash** — This name-free identity covers kind, final flag, generic arity, parent, fields, selectors, methods, and intrinsics. It excludes the class name and construction function. A constructor has its own StructuralHash. The `<class key>.<new>` binding ties it to the class. Section 3.7 defines reference hashing.
- **InterfaceHash** — This named public identity covers the export name, kind, final flag, signature, defaults, arms, and initializer. An import slot pins it. A rename changes it.
- **VerificationHash** — the exact resolved input of the verifier. It answers whether the verifier approved this exact representation.

The linker merges two classes on QualifiedKey and StructuralHash together (3.6). Instance headers point to a VM-local class slot that the linker resolved. Class equality at run time stays an index comparison inside one linked program, and no run-time path compares a hash.

---

## 9. Enums and patterns

### 9.1 Enums

```lm
enum Option[T]
  Some(v: T)
  None
end
```

An enum is sugar for one abstract closed parent and one final case class per arm. The family is normalized and hashed together. Each arm has a canonical qualified identity such as `Option.Some`.

Methods may follow all arms:

```lm
enum Option[T]
  Some(v: T)
  None

  def is_some(self): Bool
    case self
    in Some(_) then true
    in None    then false
    end
  end
end
```

An enum can use `implements` after its generic parameters.

Associated type bindings follow all arms and precede methods.

### 9.2 Patterns

Version 0.2 patterns are wildcard `_`, binding, supported literal, and constructor patterns with nesting:

```lm
case msg
in Line(text)
  consume(text)
in Pair(Some(x), None)
  use(x)
in _
  ignore()
end
```

A constructor pattern resolves an enum arm from the scrutinee type or an explicit qualification, tests the final case, and binds fields in declaration order. An ordinary class constructor pattern tests the named class. Repeating one binding name in a pattern is a compile error. Matching invokes no user code and is pure.

### 9.3 Exhaustiveness

An enum case must cover every arm or use a wildcard/binding. `Bool` is exhaustive with `true` and `false`. Other primitive/class/`Any` cases require a wildcard or plain binding.

---

## 10. Mutability, freezing, and graph operations

### 10.1 Static reference capability

The checker tracks mutable/read-only capability separately from nominal type:

- newly constructed objects and values returned from calls are mutable references;
- a parameter is read-only unless marked `mut`;
- `self` is read-only unless declared `mut self`;
- a field read through a read-only reference is read-only;
- a `mut` argument position requires mutable capability.

A `for` loop treats its direct source place as read-only inside the body.

The rule follows local names, captures, and direct field paths.

A direct `mut self` call or `mut` argument use fails with `E1065`.

Runtime epoch checks detect structural mutation through aliases and opaque calls.

The analysis is local, does not prove uniqueness, and does not track aliases across calls.

### 10.2 Dynamic frozen bit

Every mutable heap object has a frozen bit. Strings, bytes, code, class values, operations, descriptors, types, digests, faults, and snapshots are born frozen.

`freeze()` deeply and transitively freezes a graph, preserving cycles and sharing. It may be called through a read-only reference and returns the same root:

```lm
config.freeze()
```

Freezing is monotonic and idempotent. There is no interior mutability. A later field/list/map/buffer write into the graph faults with `FrozenWrite`.

### 10.3 Boundary checks and digest

Frozenness is checked at digest and cache-key creation and at map-key insertion. Failure faults. A boundary crossing does not check frozenness: it copies the value and preserves the frozen bit of each object (16.1).

`digest()` computes BLAKE3-256 over a canonical frozen graph encoding. The encoder traverses deterministic field/index/insertion order, assigns object ordinals at first encounter, uses back-references for later encounters, encodes code/classes by hash, and includes sharing and cycles. Float encoding normalizes both signed zeros to positive zero and all NaNs to the canonical NaN, matching language equality. Live resources and nondigestible descriptors cause `BoundaryViolation`.

One graph walker must define reachability and ordering for freeze, verification, copy, transfer, digest, and snapshot traversal. Each native shape also declares one snapshot classification: machine state or host attachment (16.4). Machine state can enter snapshot bytes. A live host attachment blocks snapshot creation.

---

## 11. Effects, operations, and rows

### 11.1 `sys` and descriptors

`sys` is a frozen ordinary object supplied by the host ABI. Its group objects include:

```text
sys.io       sys.fs       sys.clock    sys.rand
sys.dns      sys.tcp      sys.tls      sys.proc
sys.vm       sys.compiler sys.reflect  sys.wait
sys.choose   sys.env      sys.entropy
```

`Choose.Pick(Int) -> Int` states a number of candidates and answers one
index. It is the choice point of a search. A driver reads the count and
answers, so a searched program holds no randomness authority and needs
no `Rand` grant. The operation carries no candidate list, because every
exact operation is monomorphic and a count names a branch.

A machine stopped at a choice point is ordinary machine state, so a
snapshot of it restores once for each candidate.
`examples/14-vm-as-multishot-search` shows the driver.

The ABI supplies one descriptor constant for each exact operation and group.

Examples include `Io`, `Tcp.Stream`, `Http.Client`, `Io.Write`, and `Tls.Handshake`.

A group constant is an `OperationGroup`.

An exact constant is an `Operation`.

`sys.io.write` has type `Op[Io.Write, (Bytes) -> Result[Int, IoError]]`.

Scope grants nothing.

Casing separates the two roles. A callable member of `sys` uses the
snake_case form of its descriptor name. `sys.io.write` performs
`Io.Write`. `sys.io.read_bytes` performs `Io.ReadBytes`. The
mapping is mechanical. Descriptors keep initial capitals, and they
appear wherever code names, grants, mocks, or matches an operation.
`Args.Get` also has the direct `sys.args()` surface.
Exactly one `sys` member is capitalized: the machine constructor
`sys.vm.Vm()`, whose name is the constructed type. Every other
member is a snake_case verb, including members that return objects,
such as `sys.reflect.parse_syntax(source)`. Lowercase performs an
operation. A capitalized name identifies its descriptor.

### 11.2 Perform

```lm
sys.io.write(b"Hello").expect("the output writes")
```

Calling an operation object executes one `PERFORM`. The VM records exact identity, arguments, expected reply type, destination, and continuation PC, then either dispatches automatically or exposes the request to a manual driver. No other guest mechanism reaches host semantics.

The core output helpers are normal Loom functions.

They use `Display` and perform the byte operations.

### 11.3 Rows and checking

A row is a comma-separated set of exact identities, groups, and effect variables:

```lm
def print_name(self) with Io.Write
  print(self.name).expect("the output writes")
end

def copy(src: String, dst: String) with Fs
  # body
end

def apply[T, U, effect e](x: T, f: (T) -> U with e): U with e
  f(x)
end
```

A namespace group denotes every exact operation declared in that namespace.

An effect set denotes explicit operations and other effect sets.

The checker expands every group to its transitive exact-operation closure.

Unknown members, duplicate members, and membership cycles reject during ABI validation.

Omitting `with` means the empty row.

For each body the checker unions direct performs, declared rows of statically selected calls, effect variables of called higher-order values, and initializer rows. The declared row must be a superset. Checking is local; no whole-program inference is required.

An override may not widen its row. Therefore a virtual call through a supertype is charged safely from the supertype signature.

### 11.4 Dynamic choice without dynamic selectors

```lm
routes: {String: () -> () with Io} = {
  "health": do || print("ok") end,
  "help": do || print("help") end
}

routes[route]()
```

The selected closure carries its row in its function type. There is no operation that invokes a method by computed name.

### 11.5 Row inclusion and grants

Rows are ordered by operation-set inclusion:

```text
empty row <: Io.Write <: Io <: Io, Fs
```

Admission checks use subsumption.

Row identity uses the normalized exact-operation closure.

The operation manifest digest covers each effect-set membership edge.

Passing authority to a child is charged to the granter's row. `PolicyTable.pass(target)` has a built-in dependent static rule: its argument must preserve a known exact identity, group, or effect variable; that operation set is added to the caller's row. A value widened to identity-erased `PolicyTarget` cannot be passed. `block`, `clear`, and pure `mock` add no row.

The interpreter never consults rows during verified execution. Rows prove a bound; tables and manual driving decide actual requests.

---

## 12. Faults and ordinary alternatives

### 12.1 Ordinary values

Expected alternatives use values:

```lm
enum Option[T]
  Some(v: T)
  None
end

enum Result[T, E]
  Ok(v: T)
  Err(error: E)
end
```

File-not-found, end-of-input, parse failure, connection refusal, mailbox closure, snapshot blockers, and restore binding failures belong in ordinary result types.

A library or user method must be total. It must give an answer for every input of its declared parameter types, and it must not fault. Two forms satisfy the rule: the method defines a meaning for the whole input range, or it reports the failure through `Option` or `Result`.

The test is the source of the argument. Any argument can come from a file, a socket, or a configuration value, so an argument range is untrusted input. A separator, a radix, a pattern, and a length all reach a method from data in ordinary programs.

Two exceptions stand:

- An index method may fault when the same class publishes a total sibling. `at` faults and `get` answers `Option`, so a caller chooses the form it needs. Section 24.4 states the pair.
- A machine-integrity failure faults, because no value can describe it. Section 12.2 lists those.

Prefer a defined meaning over a reported failure when one exists, because a reported failure costs the caller a `case` at every call. `split` with an empty separator matches at every scalar boundary, so it needs no result type.

### 12.2 Machine faults

A fault halts the current machine immediately. There is no catch, unwinding, `finally`, destructor, or user-visible stack unwinding. The holder receives a frozen `Fault` through VM/proc supervisory APIs.

A fault contains a stable code, a message, an optional operation, and a bounded execution trace.

```text
code: FaultCode
message: String
operation: Option[Operation]
trace: List[CodeLocation]
```

Hosts may redact message and trace details while preserving the stable code.

`Fault.site()` returns `Option[CodeLocation]` for the first retained frame.

`Fault.trace()` returns at most 64 locations in callee-to-caller order.

`CodeLocation.function` contains the exact verified function identity.

`CodeLocation.bytecode_offset` contains the instruction offset within that function.

`CodeLocation.path` has type `Option[String]`.

`CodeLocation.range` has type `Option[SourceRange]`.

Both optional fields contain values when the exact function version retains source data.

A stripped function uses `None` for both fields.

The VM records compact execution coordinates only when a fault occurs.

Source lookup and location allocation occur only when a holder calls `site` or `trace`.

A delayed host failure retains the source coordinate of its suspended `perform` instruction.

### 12.3 Stable codes

| Code | Cause |
|---|---|
| `PolicyDenied` | blocked or ungranted operation |
| `FrozenWrite` | write into a frozen object |
| `OutOfFuel` | instruction/intrinsic budget exhausted |
| `HeapLimit` | local machine heap limit exceeded |
| `StackLimit` | frame/operand limit exceeded |
| `BoundaryLimit` | transfer/snapshot bound exceeded |
| `InvalidVmState` | illegal control method/state |
| `InvalidRequestToken` | stale, consumed, or cross-VM request token |
| `BadOperationReply` | answer did not match declared reply type |
| `BadCast` | failed `as T` |
| `TypeMismatch` | a value crossing a VM boundary carried another type |
| `MalformedState` | machine state reached a rule the verifier proves for live code |
| `BoundaryViolation` | codec or descriptor rule violated |
| `UnsendableValue` | holder-local or nonsendable value crossed a boundary; unfrozen graph reached `digest()` or `deep_equal` |
| `MalformedArtifact` | invalid artifact/bytecode |
| `MalformedSnapshot` | invalid snapshot/machine-world image |
| `LinkMismatch` | link binding incompatible |
| `MissingCode` | required code hash unavailable |
| `DeadProc` | operation required a live proc |
| `IndexOutOfBounds` | invalid sequence index |
| `ShiftOutOfRange` | shift or rotation amount outside 0 through 63 |
| `LengthMismatch` | fixed-length operands have different lengths |
| `InvalidPrecision` | formatting precision is negative |
| `MissingKey` | faulting map lookup missed |
| `DivideByZero` | invalid division |
| `IntegerOverflow` | checked integer overflow |
| `AssertionFailed` | assertion false |
| `HostFault` | host failure outside ordinary operation result |

Implementations may attach diagnostic subcodes; portable code relies only on the stable set.

### 12.4 Recovery

A child VM is the recovery boundary:

```lm
case child.run()
in Ok(v)  then use(v)
in Err(f) then recover(f)
end
```

Faults are not implicit result types and do not appear in effect rows.

---

## 13. Policy tables

### 13.1 Ownership and initial state

Every VM owns one native table outside the guest heap. It is excluded from snapshots. A fresh table has no entries and a default action of `block`.

```lm
t = vm.table()
```

The full VM holder may edit the synchronized table. `vm.table()` is a `Vm` operation and returns a holder-local native capability. Once that capability is possessed, its edit methods are non-suspending kernel control intrinsics rather than performs by the controlled child: `block`, `clear`, and valid pure `mock` have the empty row, while `pass` has the dependent grant row described below. This preserves the rule that blocking or stubbing grants no host authority, while creation, driving, inspection, and acquisition of a child's table remain explicit `Vm` effects.

### 13.2 API and specificity

```lm
t.pass(Io)
t.block(Fs)
t.mock(Clock.Now, do || 1_700_000_000 end)
t.clear(Clock.Now)
```

- `pass`: forward to a live holder, parent table, or root host;
- `block`: fault the controlled guest with `PolicyDenied`;
- `mock`: use a pure handler with the exact operation signature;
- `clear`: remove the exact/group entry.

`block` and `clear` accept any `PolicyTarget`. `pass` follows the identity-preserving static rule in section 11.5. `mock` requires an exact operation descriptor known to the checker.

Lookup order is exact operation, group, then default block. Groups are flat. Insertion order has no effect.

### 13.3 Mock execution

A mock handler has verified code, an empty row, and a sendable capture graph. Installation boundary-copies it into table-owned storage. It has no table, cannot suspend, and receives a deterministic work limit. Its heap result must be sendable, and that result copies into the controlled guest. A mock fault, budget exhaustion, or invalid result faults the controlled guest. The guest sees the whole perform as one instruction.

### 13.4 Pass chains and revocation

Each table applies its own action before a pass continues. A block denies the request, and a mock answers it.

If a table passes while its machine has an active driver, the request goes to that driver. Otherwise, resolution continues at the parent table.

Resolution can eventually reach an embedding-host registry. Each ancestor therefore keeps its authorization decision.

Parent edits affect future child performs. A missing parent or root binding denies the request with `PolicyDenied`.

Terminal completion stops machine execution. It does not remove a table used by live descendants.

A live descendant can pass through a terminal intermediate parent.

A pass that reaches a terminal world root denies the request.

Editing a table while a proc runs affects future lookups; it does not retroactively cancel a host operation already accepted unless that operation's own semantics expose cancellation.

### 13.5 Manual policy

`drive()` stops its direct machine before lookup. A descendant request also stops when a pass reaches the active driver.

The holder can answer, reject, or dispatch the request. `dispatch()` applies the stopped table for a direct request.

For a routed request, `dispatch()` continues after the pass that reached the driver. Tables remain the only automatic policy mechanism.

---

## 14. Virtual machine object

### 14.1 VM images and typed runs

`Vm` is a native persistent execution image. It owns installed code, live slots, runs, and processes.

`Run[T]` names one active invocation. Its terminal result has type `T`.

Both types are holder-local. Their control methods use operations in group `Vm`.

Every nested run uses the same native interpreter. Loom never implements nesting through recursive interpretation.

The public families preserve the final result type:

```text
Vm
Run[T]
StepEvent[T]
DriveEvent[T]
RunSnapshot[T]
VmSnapshot
```

There is no execute-an-unknown-signature shortcut.

`Vm.activate` requires a function with a known signature. It returns `Result[Run[DynValue],CodeError]` only for a declared `DynValue` result.

### 14.2 Construction and loading

```lm
vm = sys.vm.Vm()
activation = vm.activate(program, args: ("Ada",))
```

`Vm.New` creates an empty execution image.

`activate[A,R,e](program: Fn[A,R,e], args: A) -> Result[Run[R],CodeError]` checks and transfers the arguments.

`activate_or_fault[A,R,e](program: Fn[A,R,e], args: A) -> Run[R]` faults when activation fails.

Use `activate` for code handles or other inputs that can fail at runtime.

Use `activate_or_fault` when failure indicates a program invariant violation.

Activation creates the initial frame but executes no guest instruction.

The VM remains available after activation. It can create later runs with different terminal types.

Installed entries use the same activation rule. A typed caller uses `Instance.entry[A,R]()` with compile-time argument and result types.

`Instance.dynamic_entry()` requests `FunctionDef[(),DynValue]` without a source-level type witness.

### 14.3 States

| State | Meaning |
|---|---|
| `image` | persistent installed code and live slots; public type is `Vm` |
| `ready` | paused and holder-controlled |
| `running` | executing on a host thread |
| `asked` | `drive` stopped before dispatch |
| `waiting` | dispatched host completion pending |
| `proc_owned` | scheduler owns execution |
| `done` | terminal value stored |
| `faulted` | terminal fault stored |

Each run has at most one pending perform record.

A ready machine can hold one nested control edge. The edge links a pending `run`, `step`, or `drive` operation to its direct child.

A routed-request record links a driven surface to an asked descendant. It also stores the next policy location.

These records are not public machine states.

### 14.4 Events

```lm
enum StepEvent[T]
  Ran
  Waiting(wait: WaitView)
  Done(value: T)
  Fault(fault: Fault)
end

enum DriveEvent[T]
  Asked(request: Request)
  Done(value: T)
  Fault(fault: Fault)
end
```

`WaitView` is inspection-only because automatic policy has already accepted and dispatched that operation.

`Request` appears only on the manual path before policy lookup. It can describe the driven machine or one routed descendant.

An event holds no reference into the controlled machine. Its native parts, `WaitView`, `Request`, and `Fault`, are frozen views. Before terminal success is published, the value crosses transfer mode. A mutable result copies, and the holder receives a mutable copy. A holder-local or nonsendable result converts the controlled machine to `Fault(UnsendableValue)`.

### 14.5 `step`

`step()` retires exactly one guest instruction with automatic policy enabled.

- A normal instruction returns `Ran` unless terminal.
- A synchronously resolved `PERFORM` counts as the one retired instruction and normally returns `Ran`.
- An accepted asynchronous/blocking perform returns `Waiting(wait)` and leaves the machine in `waiting`.
- A terminal instruction returns `Done` or `Fault`.

If called while still waiting and no completion is ready, `step` returns the same semantic `WaitView` without fuel use. When a completion is ready, the next `step` validates and installs it before retiring one new guest instruction.

### 14.6 `run`

`run()` is valid from `ready` and `waiting`. It uses automatic table dispatch, waits for accepted blocking host operations, and continues to `done` or `faulted`. It enters one dedicated interpreter loop; it is not implemented as repeated public `step()` calls and allocates no event per instruction.

`run()` returns `Result[T, Fault]`.

### 14.7 `drive`

`drive()` is valid from `ready`, `waiting`, and `asked`. It is also valid when the machine holds a routed request.

From `asked`, it returns the same request with a fresh holder token. It executes no instruction and consumes no fuel.

The routed case returns the descendant request with a fresh token. From `waiting`, an accepted wait completes before interception resumes.

Otherwise, it runs until one `PERFORM` records its operation, arguments, reply type, destination, and continuation PC. It then stops before table lookup:

```lm
case vm.drive()
in Asked(q)
  # state asked
in Done(v)
  # terminal success
in Fault(f)
  # terminal fault
end
```

The perform consumes normal fuel. No host-stack continuation and no copied guest stack are created.

Nested VM execution uses the same rule. A descendant pass can return `Asked` through the machine that the holder drives.

### 14.8 Typed request matching

`Request` is an opaque holder-local token for one pending perform. The token names the machine that performed the operation.

That machine can be a descendant of the `Vm` receiver. Its erased inspection surface contains no `Any`:

```lm
q.op_name(): String
```

`op_name` gives the qualified name of the operation, such as `Clock.Now`. A wildcard arm carries no operation identity, so a holder reads the name for a report or for the reason of a denial.

The token names a machine and an ordinal, and never the operation, so the name comes from the pending record of that machine. The request must still be live. `answer`, `reject`, `dispatch`, and `serve_file` each spend one request, and reading the name after any of them faults the caller with `InvalidRequestToken`.

Version 0.2 has no wider erased request surface.

It has no identity-erased operation, value, or type views.

To read arguments or answer, the holder matches the request against an exact typed operation object:

```lm
case q
in Call(Io.Write, call, (bytes,))  # the tuple is (Bytes,)
  captured.push(bytes)
  vm.answer(call, Ok(bytes.len())) # reply is Result[Int, IoError]
in Call(Clock.Now, call, ())
  vm.answer(call, 123)
in _
  vm.dispatch(q)
end
```

`Call(op, call, args)` names one exact operation of the manifest, binds the `PendingCall`, and matches `args` against the argument tuple. The operation set is open, so a `case` over a `Request` always needs a final wildcard arm, and two arms that name one operation report the second as unreachable.

Call a continuation method on the same `Vm` receiver that produced the event. The route proves that the descendant request reached this receiver.

The `Call` pattern has a narrow compiler-known type rule. Its first position is an exact `Operation` descriptor known to the checker, such as `Io.Write`. If the manifest signature of that descriptor is `(A...) -> R`, the arm binds a `PendingCall[(A...), R]` and matches its third position against `(A...)`. The callable `sys` member is not used here: matching is descriptor work, and the compiler supplies the typed signature from the manifest. `PendingCall[A,R]` exposes:

```text
args(self) -> A
reply_type(self) -> Type[R]
request(self) -> Request
```

This is existential elimination at an operation-identity test, not general dependent typing. The checker instantiates the token type from the static `Op` type; bytecode carries the expected dense operation/type slots; and the runtime returns `Some` only when the pending exact operation slot matches. ABI initialization has already verified that the slot owns that argument/reply signature, so the success path needs no general dynamic cast. The only other such native rule in version 0.2 is effect charging for `PolicyTable.pass`.

### 14.9 Continuation methods

While the controlled VM is `asked` or holds a routed request:

```lm
vm.answer(call, value)       # PendingCall[A,R], value: R
vm.reject(q, fault)          # Request, Fault
vm.dispatch(q)               # Request
```

The runtime builds one internal `ReplySink` after it validates the continuation. This check validates the receiver, route, target, ordinal, and any typed operation.

`answer` boundary-encodes the typed reply and validates its runtime `TypeId`. It installs the reply in the performing machine.

A bad reply faults the performing machine with `BadOperationReply`. A stale or foreign token faults the caller with `InvalidRequestToken`.

`reject` installs the supplied frozen fault in the performing machine. `dispatch` continues policy resolution from the saved location.

`Fault.denied(reason)` builds the fault a holder needs for `reject`:

```lm
vm.reject(request, Fault.denied("the operation is not permitted"))
```

This is the one fault a program can build. Its code is always `PolicyDenied`, so no program can claim a machine-internal code such as `OutOfFuel`. The value is pure and needs no authority: only `reject` installs it, and `reject` charges `Vm`.

`reject` records the operation that the target machine performed, and it ignores the operation field of the supplied value. A holder therefore states the reason alone.

Live denial matches advance denial. `block` denies before the request, and `reject` denies the request in hand. Both leave the performing machine faulted with `PolicyDenied`. A holder needs the live form, because a reply type such as the `Int` of `Clock.Now` carries no error arm, and a wildcard arm holds no reply type at all.

A nested VM control dispatch records an edge and returns. The next control call rebuilds the driver activation before the child runs.

Tokens need not be linear in the source type system. The VM validates single use.

These methods are invalid in other states. Calling `step` or `run` with a live request faults the caller with `InvalidVmState`.

Repeating `drive` performs the token recovery described above.

Terminal execution calls return the stored terminal event idempotently.

### 14.10 Reentrancy, inspection, and ownership

A control method on a currently running `Run` faults.

Guest code also cannot control its current run. Execution and inspection fault during process ownership.

An existing policy-table handle permits synchronized live revocation.

A routed request parks its descendant activation chain. Only the holder of the driven surface can consume that route.

`stack()` is valid only while the run is stopped and holder-owned.

It returns deep-frozen `CodeLocation` values with the top frame first.

At most one host thread owns execution. Guest execution remains one logical thread.

### 14.11 Fuel and limits

A VM has instruction/intrinsic fuel, heap-byte limit, frame/operand limit, boundary-byte limit, mailbox limit, and snapshot-byte limit. One bytecode instruction consumes one fuel unit; pure intrinsics have deterministic published charges based on logical input size rather than host hash-table probe count.

A parent granting child resources reserves them from its own budget. A root host may mint resources. Exceeding a limit faults only that VM.

The instruction budget of one machine and the budget one world shares
both default to the largest value. A program that serves forever is an
ordinary program, and section 7.2 declares `serve(): Never`, so a root
program takes no cap it did not ask for. A caller that runs code it
does not trust states a bound. The default is a very large number, not
the absence of a bound.

The child budget counts the children a machine holds, not the children
it ever created. A child that ended, that no live machine names, and
that holds no host resource can never run again and can never be read,
so the world frees its record and returns its budget unit. A search
driver therefore pays for the branches it still holds.

The reference runner permits 262,144 live machines, VM images, child
reservations, and waits by default. These ceilings reserve no memory
in advance.

Hard structural limits do not set the record reclamation interval.

The runtime reclaims unreachable machine and VM image records before
their tables approach those limits.

The CLI exposes `--max-machines`, `--max-images`, `--max-children`, and
`--max-waits`. An embedding host supplies the same limits through the
runner API.

### 14.12 One interpreter loop

The Rust reference VM exposes one internal entry point:

```text
execute(vm: &mut VmState, mode: StopMode) -> VmExit
```

`StopMode` selects one instruction, terminal-only automatic execution, or stop-before-policy manual execution. The loop uses decoded numeric instruction records, dense code/class/type/selector slots, contiguous frame and operand vectors, and one preallocated pending-perform record. Public `Request`, `WaitView`, and event values are materialized only when execution exits to the holder.

The driver stores nested VM control as explicit machine edges. It rebuilds activation records from those edges.

A routed request stores its target and saved policy cursor. The driver needs no host-stack continuation for either record.

The perform hot path is: write pending fields, load exact table action, fall back to group action, then block/pass/mock dispatch. `drive` takes the same path only until the pending fields are complete. No row lookup, string lookup, heap continuation, or public API transition occurs per guest instruction.

## 15. Nested VMs

Nesting is ordinary composition of functions that use `Vm`:

```lm
def f2(): Int with Vm
  case sys.vm.Vm().activate_or_fault(do || 21 end, args: ()).run()
  in Ok(v)  then v
  in Err(_) then 0
  end
end

def f1(e: () -> Int with Vm): Int with Vm
  expr = do || with Vm
    x = e()
    x + x
  end

  vm = sys.vm.Vm().activate_or_fault(expr, args: ())
  vm.table().pass(Vm)
  case vm.run()
  in Ok(v)  then v
  in Err(_) then 0
  end
end

f1(f2)
```

`f2` executes inside machine A when called there; its `Vm.New` request climbs A's table. Since A passes `Vm`, the holder creates machine B. B's pure payload needs no grants. Native VMs make this an authority tower, not interpreter recursion.

A fresh table denies everything. Each level must explicitly pass what code running below it may request. Because `pass` is charged to the granter's row and callable rows are transitive, a top-level row bounds operations the whole descendant tower can cause.

A full VM handle held by code running inside that same VM is re-entrant poison: its control methods fault with `InvalidVmState`.

---

## 16. Boundary codec and sendability

### 16.1 One codec, explicit contexts

One boundary codec serves VM load, terminal results, proc send/spawn, snapshots, linking, imports, and inspection. It runs in one of four contexts:

1. **transfer:** independently controlled heap boundary;
2. **control envelope:** holder-supplied temporary containers such as `args`, compile imports, link maps, and restore bindings;
3. **snapshot:** canonical machine-world serialization;
4. **inspection:** detached frozen views.

A boundary crossing copies the value. Sharing and cycles inside one crossing are preserved, nothing is shared across the boundary, and object identity does not cross. The copy preserves the frozen bit of each object, so a mutable graph crosses as a mutable graph and a frozen graph crosses as a frozen graph. An implementation may share frozen storage or elide a copy when no program can observe the difference.

A control envelope is holder-owned native metadata rather than a guest collection. Every member installed into guest, link, or compiler state is independently encoded and checked. Thus `args: ("Ada",)` is legal, and each member crosses under its own rule.

Each host-operation parameter and result position has an ABI mode. The default `value` mode supplies an immutable/frozen boundary value. `transfer` moves a sendable value into another independently controlled heap (for example proc messages). `designator` accepts only the exact native handle kind named by the signature. `inspect` permits a transient read-only graph walk of the performing VM without making that graph sendable; the host receives a bounded inspection cursor/view and may not retain a guest pointer. `control` is reserved for holder-facing VM/compiler/link/snapshot envelopes. These modes are fixed in the operation manifest and cannot be chosen dynamically by guest code.

### 16.2 Sendable values

Transfer mode accepts:

- unit, booleans, numbers, characters, strings, bytes, digests;
- graphs of sendable fields/elements, mutable or frozen;
- class/code/function values by hash plus their capture graphs;
- operation/group/type descriptors;
- sendable typed proc handles and snapshots;
- host value types explicitly marked sendable by the ABI.

It rejects scoped designators, full VM and policy-table handles, live host callbacks, and live OS resources. Rejection faults with `UnsendableValue` or `BoundaryViolation`. An accepted value copies, and the copy keeps the frozen bit of each object (16.1).

### 16.3 Code and class transfer

Code and classes cross by semantic hash. The receiving code store must already contain verified bytes for that hash or obtain them through an embedding-host code resolver. Missing code yields `MissingCode`. Code bytes are never accepted under a mismatched hash.

A closure transfers code identity and a copy of its capture graph. A capture that includes a holder-local handle or a scoped designator makes the closure unsendable.

### 16.4 Handles, machine references, and resources

A full `Vm`, `PolicyTable`, or raw host registry handle is holder-local for transfer. A proc `Handle[M,R]` is a live sendable typed designator. Transfer preserves its target and its exact `M` and `R` types.

Each live proc reference has a proc identity and a generation in the reference implementation. These fields are not ordinary guest fields. A stale generation produces the existing dead-proc result.

Snapshot context is different from transfer context. A snapshot copies a whole machine world (17.1). A proc handle and a held machine handle are machine references inside that world, so the snapshot copies them and their targets together.

Every other native value is one of two kinds. **Machine state** has bytes the codec can copy: data graphs, code, classes, descriptors, snapshots, and closed resource handles. A **host attachment** names live state outside every machine: an open file, a socket, or a pending host operation. A host attachment has no bytes to copy. A live attachment blocks snapshot creation with an ordinary typed error (17.4). A closed resource handle carries no host authority. Its operations return the ordinary closed-resource error.

### 16.5 Inspection

Everything read out of another heap—stack frames, mirrors, pending request arguments, artifact metadata—returns as an immutable native value or deep-frozen detached graph. Inspection never returns a writable guest reference.

---

## 17. Snapshots

### 17.1 VM snapshots and run snapshots

Machine state is data (section 1). A snapshot copies machine state at one moment.

`RunSnapshot[T]` contains one complete image and selects one `Run[T]`.

```lm
case run.snapshot()
in Ok(snap)
  case sys.vm.Vm().restore(snap)
  in Ok(restored) then restored.run()
  in Err(error) then report_restore(error)
  end
in Err(error)
  report_snapshot(error)
end
```

`VmSnapshot` contains one complete image without a typed run selection.

`Vm.snapshot()` captures every stopped run and process in that VM.

A held run snapshot selects one paused `Run[T]` as its root.

A receiverless self snapshot records an untyped distinguished run marker.

The snapshot world contains the root and every reachable machine. Handles, nested control edges, and routed requests establish reachability.

Heap, frame, closure, mailbox, pending, and terminal values can contain handles. Reachability is transitive.

Running procs, paused procs, terminal procs, and held nested machines all ride along.

The world is closed by construction. Reachability follows the handles, so every handle in the capture targets a captured machine. A reference that leaves the world is not representable. The design therefore needs no ownership records, no external references, and no restore-time resolution. What cannot exist needs no tracking.

The held and self forms use distinct operation identities.

A held call returns `Result[RunSnapshot[T],SnapshotError]`.

A self call returns `Result[VmSnapshot,SnapshotError]`.

The self call cannot name the enclosing run result type.

External bytes first pass through `sys.vm.load_snapshot(bytes)`. The loader decodes and admits the bytes once and returns `Result[VmSnapshot,SnapshotError]`.

A guest snapshot always has admitted host backing.

Each creation path runs admission or copies a stopped verified image.

Editable snapshot data has no guest spelling. It never backs a guest value.

`RunSnapshot[T]` is a typed capture of one run.

```text
VmSnapshot.to_bytes(self) -> Result[Bytes, SnapshotError]
RunSnapshot[T].to_bytes(self) -> Result[Bytes, SnapshotError]
```

External bytes produce `VmSnapshot` because they have no guest type witness.

`Vm.restore_dynamic` restores its distinguished run as `Run[DynValue]`.

A run image stores an explicit distinguished-run selector. The selector does not give machine ordinal zero special meaning.

A full VM image stores a separate VM-image selector. Machine ordinal zero has no selection meaning in that image.

### 17.2 World contents

A snapshot contains format and ABI versions, code manifests, type tables, heaps, frames, limits, fuel, and machine states.

Code manifests include current slot targets and immutable targets retained by installed bindings.

It also contains pending requests, nested control edges, routed requests, mailboxes, terminal results, machine references, and a container hash.

It excludes policy tables, root grants, live host callbacks, host thread identity, executor tasks, mutex/channel storage, wake objects, and live OS handles. It can include closed resource handles.

The encoder assigns one canonical machine ordinal to each captured machine. A run image assigns ordinal zero to its distinguished run.

A full VM image orders machines canonically. Its VM-image selector identifies the captured VM.

A handle in snapshot bytes stores its machine ordinal and static type. Restore relocates each handle to its restored machine.

This rule covers handles in heaps, frames, locals, operands, closure captures, mailboxes, pending arguments, and terminal results.

Relocation is implementation work. Guest code cannot observe it.

### 17.3 Consistent cut

A snapshot is a copy of the world at one moment. One scheduler barrier defines that moment.

1. It stops the root and each reachable running machine at an instruction or operation boundary.
2. It walks the stopped heaps and stops newly found machines until the set is closed.
3. It freezes mailbox acceptance for the set at one cut marker.
4. It records accepted queues and machine states.
5. It preflights host attachments.
6. It encodes only after every preflight succeeds.
7. It resumes the original world after success or failure.

A send accepted before the cut appears in the snapshot queue. A send accepted after the cut affects only the original world. The barrier does not stop unreachable machines.

The cut is safe because control is serialized. A guest holder is one machine with one logical thread, and it blocks inside its own snapshot call. Every control operation on a machine goes through the scheduler, so an outside call waits for the cut instead of racing it. The snapshot is a copy, not a move, so a call that lands after the cut changes only the original world. No configuration of machines, handles, or pauses makes the cut fail.

Barriers over disjoint worlds may run concurrently. Barriers over overlapping worlds serialize in the scheduler.

A failed snapshot leaves the original world unchanged. No machine remains stopped after the failure returns.

### 17.4 Snapshot errors

Two conditions block a copy. Both are ordinary typed errors, not machine faults.

```text
ResourceActive(machine_path, resource_kind)
SnapshotLimitExceeded
```

`ResourceActive` reports a live host attachment: an open file or socket handle, an active `FileLease` scope, or a pending host operation in `waiting`. Such state lives in the host, not in any machine, so the codec has no bytes to copy. The language never reopens a host resource silently. The machine path is bounded and starts at the root. The caller closes or finishes the attachment and retries at a later boundary. A closed handle value does not block the copy.

`SnapshotLimitExceeded` reports a capture past the configured snapshot byte limit.

### 17.5 Restore and fresh authority

`Vm.restore(snap: RunSnapshot[T])` imports the captured image into that VM.

It returns `Result[Run[T],RestoreError]`. A failed restore exposes no partial world.

`Vm.restore_dynamic(snap: VmSnapshot)` imports the distinguished run without its result type.

It returns `Result[Run[DynValue],RestoreError]`. The run delivers its result as one `DynValue`.

`IncompatibleImage` reports a snapshot that selects no run.

`Run[T].stack()` lists the frames of a stopped held run as `List[CodeLocation]`. The top frame comes first.

`sys.vm.restore_vm(snap: VmSnapshot)` creates one stopped `Vm`.

It returns no distinguished run. A failed restore exposes no partial VM.

Policy tables are never serialized.

Each restored run receives a fresh default-deny table.

Each restored machine receives a fresh default-deny table.

Restore creates no authority.

A routed cursor outside the captured world binds to the restoring holder. Dispatch then consults the restoring holder's table.

The cursor restores no old table grant.

The returned root run is holder-controlled.

Restored procs stay behind one world gate. The first root control operation opens that gate.

### 17.6 Paused, pending, and self snapshots

A snapshot taken between instructions restores between those instructions. A snapshot in `asked` preserves operation, arguments, reply type, destination, continuation PC, and ordinal. The holder calls `drive()` once to obtain a fresh `Request` token. No guest instruction runs.

A routed snapshot preserves the descendant target, nested control edges, and next policy location. The holder also calls `drive()` to obtain a fresh token.

A machine in `waiting` holds a pending host operation, which is a live host attachment. It blocks the snapshot with `ResourceActive`. The caller retries after the operation completes.

A proc captured while holder-paused restores in the paused state. When its pauser is a captured machine, the restored pauser holds the restored paused machine and resumes it normally. Otherwise `resume()` through the proc handle reactivates it.

A receiverless self snapshot is captured while `Vm.SnapshotSelf` is pending. The restored root holds that pending request. The restorer answers it through the ordinary `drive` path with a `Result` value of its choice. Execution then continues after the call in both worlds.

### 17.7 Multi-shot restore

Snapshot bytes are immutable. One snapshot may produce many restored worlds.

Each restored world is complete and independent. No machine, mailbox, or resource is shared between two restores, or between a restore and the original. Divergence after restore is ordinary execution.

### 17.8 Decoding and admission

Snapshot loading uses two separate checks.

Decoding protects the host from untrusted bytes.

Admission proves that every structural reference resolves.

Neither check proves the type of a stored value.

The decoder produces an editable `Image`.

Only admission or private in-process capture can produce an immutable `SnapshotImage`.

Restore accepts `SnapshotImage` only.

Decoding checks:

- magic, version, canonical integers, section bounds, and container hash;
- every count against a load limit and against the bytes that remain, before any allocation;
- one representable value for every wire tag;
- one aggregate allocation budget for the complete container;
- no overlapping section or trailing section data.

An `Image` promises nothing about references, machine state, or types.

An editor can create the same invalid state without container bytes.

Admission therefore repeats every required structural check.

Admission uses this rule:

> Editable snapshot data becomes an admitted host image only when its structure resolves.

Structural resolution checks:

- the distinguished run or full-VM selector;
- every machine and object ordinal;
- every function, class, type, and operation identity;
- every installed artifact and aggregate code identity;
- every frame and reachable instruction boundary;
- every frame environment and arena partition;
- every object field, collection element, and closure context;
- every literal and relocation record;
- every machine reference and parent chain;
- every request token and lifecycle record;
- every mailbox, block, pause, and gate record;
- every nested control edge and policy cursor.

Admission checks each installed artifact with the independent verifier.

Admission does not prove value types, progress, scheduler fairness, authority, or host resources.

A structurally valid image can contain a value with the wrong runtime tag.

The interpreter checks each typed value tag before it reads the payload.

A mismatch faults the controlled machine with `TypeMismatch`.

Field reads report `UninitializedField` for the `Uninit` tag.

Graph copies and digests report `BoundaryViolation` for `Uninit`.

The world also checks values at VM boundaries.

The receiving verified instruction supplies the expected boundary type.

Boundary checks cover terminal results, mailbox messages, replies, spawn arguments, mock results, and typed restores.

Each check descends through fields, captures, and collection elements.

It compares a closure with the complete verified closed signature.

Native handles use a shape check and validate their produced values at their next boundary.

The graph copy and type check use separate bounded walks.

One object can appear under several expected types.

Thus, one object-identity walk cannot replace the type walk.

Generic runtime state carries closed type-environment witnesses.

A frame stores its applied type and effect arguments.

A closure stores its creator environment.

An instance stores its concrete class arguments.

A machine stores its result and mailbox types.

One canonical world table stores every closed type and environment.

Index zero names the empty environment.

Each table entry contains no free type variable.

Each entry has a canonical content identity.

Restore interns these records into the target world and remaps every stored index.

Admission checks witness structure, bounds, arity, and acyclicity.

Admission does not prove that execution produced a supplied witness.

Interpreter tag checks remain authoritative inside an externally restored world.

World limits bound type nodes and environment nodes.

Witnesses do not affect value equality, semantic digests, or value identity.

Capture preserves witnesses before their source activation can disappear.

External loading performs these actions:

1. Decode bytes into `Image` under one aggregate budget.
2. Admit the image against exact verified code.
3. Seal the image with its canonical bytes and hash.
4. Return `SnapshotImage`.

In-process capture constructs `SnapshotImage` from stopped verified state.

The capture constructor remains private to snapshot capture.

The image origin supports diagnostics only. It proves no stored value type.

Editing an admitted image produces an untrusted `Image`.

The edited image needs admission before restore.

One `SnapshotImage` can support many restores.

Restore repeats no admission walk.

Canonical bytes carry no admission status.

Another process repeats decoding and admission.

A nested snapshot stays opaque until its own restore.

### 17.9 Canonical form

The canonical snapshot representation uses deterministic section order, little-endian fixed fields where specified, canonical LEB128 counts/integers, object ordinals assigned by root traversal, machine ordinals assigned by deterministic reachability traversal from the root, and BLAKE3-256 domain-separated hashes. Debug/source-map data may be present but does not affect guest semantic identity.

Canonical bytes carry no admission status. The container hash identifies bytes.

Admission status belongs to one process. Snapshot bytes transfer no trust.

Loading the bytes in another process repeats admission (section 17.8).

### 17.10 In-memory branches

`Run.branch()` copies one held run world in memory.

It returns `Result[Run[T],BranchError]`.

The returned run remains under holder ownership.

The operation does not schedule the returned run.

`sys.proc.run` can transfer that run to the scheduler.

The branch shares immutable verified code.

It copies mutable machines, heaps, mailboxes, and VM image state.

The operation does not create snapshot bytes or a snapshot value.

`ResourceActive(path,kind)` reports a live host attachment.

`BranchLimitExceeded` reports a machine, image, heap, or graph limit.

`Run.branch_answer(call,value)` copies a run at a pending call.

It answers only the copied call and returns the copied run.

The source run and its call token stay unchanged.

A stale or foreign call token causes `Fault(InvalidRequestToken)`.

The operation returns the same `BranchError` cases as `branch()`.

---

## 18. Procs and mailboxes

### 18.1 Proc model

A proc is one VM, one private heap, one bounded mailbox type, one terminal result type, and one logical guest thread. Procs share no mutable guest memory; values cross through the boundary codec.

A bare `Proc` superclass is syntax sugar for `Proc[Never]`, meaning no messages:

```lm
class Doubler < Proc
  def on_spawn(self, n: Int): Int
    n * 2
  end
end

h: Handle[Never, Int] = Doubler.spawn(21)
case h.done()
in Ok(v)  then v
in Err(_) then 0
end
```

The proc instance is constructed inside its VM. The spawner receives only a typed `Handle[M,R]`, where `M` is the mailbox message type and `R` is the declared result of `on_spawn`.

### 18.2 General launch

```lm
vm = sys.vm.Vm().activate_or_fault(program, args: ("Ada",))
vm.table().pass(Io.Write)
vm.table().mock(Clock.Now, do || 0 end)

h: Handle[Never, ()] = sys.proc.run(vm)
```

`Proc.Run` with no mailbox argument chooses `M = Never`. The mailbox-bearing native form accepts an explicit `MailboxType[M]` created by proc-class lowering. `Proc.Run` atomically transfers execution ownership to the scheduler. The original VM handle becomes dormant; execution/inspection through it faults until `pause()` returns ownership. These methods are operations and therefore carry their exact `Proc.*` rows; table edits remain legal for revocation.

`sys.proc.run` also accepts a sendable nullary closure.

This form returns `Handle[Never,R]` and uses the closure row as the child birth grant.

### 18.3 `spawn` sugar and birth grant

`Class.spawn(args...)` is compiler sugar available only for a subclass with a valid `on_spawn`. It constructs a VM from the proc class and a typed argument tuple, transfers code/data through the codec, grants the child `Proc` group, creates the declared mailbox, and invokes `Proc.Run`. The return type is `Handle[M,R]` inferred from the proc superclass and `on_spawn` result.

A proc spawned by a persistent VM run inherits that VM image. A nested proc inherits the same image.

The proc uses the inherited image for all slot instructions. The link keeps the image live and survives snapshot restore.

An executing image proc blocks installation and slot replacement. A paused proc permits both operations.

The birth grant is required so mailbox-bearing procs can receive. Since `spawn` itself carries `Proc`, the spawner is statically allowed to pass that group. Additional grants, mocks, limits, or admission checks use the explicit VM path.

### 18.4 Handles and terminal results

The core image defines mailbox delivery results explicitly:

```lm
enum SendResult
  Sent
  Closed
  Fault(fault: Fault)
end
```

`Closed` means the target mailbox no longer accepts messages; `Fault` reports a dead target, cancellation, or another target/host supervisory fault. A message that fails the sender-side boundary check faults the sender instead of becoming `SendResult`. A successful `close` returns `Sent`; repeating it returns `Closed`.

`send` copies the message into the receiving machine (16.1). The receiver owns a fresh graph, and that graph is mutable when the source was mutable. Identity does not cross: two sends of one object deliver two objects, and a later write by the sender never reaches a delivered message.

A `Handle[M,R]` supports:

```lm
h.done(): Result[R, Fault] with Proc.Done
h.pause(): Result[Run[R], ProcError] with Proc.Pause
h.resume(): Result[(), ProcError] with Proc.Resume
h.close(): SendResult with Proc.Close
h.snapshot_wait(fuel: Int): Result[RunSnapshot[R], SnapshotError]
  with Proc.SnapshotWait
```

When `M` is not `Never`, the handle also supports typed `send`. No `send(Any)` escape exists.

`done()` waits for termination and returns the typed result or fault. The boundary checks its value.

Pause stops at a guest boundary and returns the paused `Run[R]`. Resume returns it to scheduler ownership.

`snapshot_wait` parks its caller. The scheduler continues the target world within the fuel budget.

Handles are sendable typed designators. Sending a handle preserves its target and types. A handle inside a snapshot targets a captured machine, because the snapshot world is closed under reachability (17.1).

### 18.5 Typed mailboxes

```lm
enum LogMsg
  Line(text: String)
end

class Logger < Proc[LogMsg]
  lines: [String] = []

  def on_spawn(mut self): [String] with Proc
    loop do
      case self.receive()
      in Msg(Line(text))
        self.lines.push(text)
      in Closed
        return self.lines.freeze()
      end
    end
  end
end

log: Handle[LogMsg, [String]] = Logger.spawn()
log.send(Line("hello"))
log.send(Line("world"))
log.close()
log.done()
```

`receive` is available only on proc `self` and performs `Proc.Recv`, returning:

```lm
enum Recv[M]
  Msg(message: M)
  Closed
end
```

`receive_wait()` returns `Wait[Recv[M]]` with `Proc.RecvWait`. It removes no message before selection commits.

Accepted messages are delivered FIFO by host acceptance order. `close` prevents later acceptance but preserves queued messages; `Closed` arrives after the queue drains. A send to a closed/dead peer returns a dedicated ordinary `SendResult`, unless malformed or holder-local data faults the sender at its boundary.

A proc may hold its own handle, so a send may name the sending machine. That send copies the message inside one heap, so the sender and the mailbox never share one graph.

A mailbox message type must not name a holder-local native class. The checker rejects the proc-class declaration or the mailbox type at compile time. `Handle[M,R]` remains a legal message type, because a handle is a sendable typed designator.

Handles are sendable typed designators, so send rights can travel as data without erasing `M` or `R`. Version 0.2 has no attenuated send-only view.

### 18.6 Failure and parent lifetime

A proc crash is a value for its holder.

Two blocked procs can deadlock. Fuel, timeout operations, or supervision can convert that state into a result or fault.

A child table passes through its parent's table.

Terminal completion does not remove that table while a live child route refers to it.

A missing parent denies future requests.

### 18.7 Distribution

A spawn payload contains a code hash and a typed tuple of sendable values. Version 0.2 provides no remote scheduler.

## 19. Reflection

```lm
parsed = sys.reflect.parse_syntax(source)
```

`Reflect.ParseSyntax` returns one lossless concrete syntax tree, parse status, and diagnostic list.

The syntax values are immutable. They expose no writable compiler or VM state.

Version 0.2 has no general object mirror or dynamic invocation by name.

Sections 20.4 and 23.10 define syntax inspection, construction, and compiler inputs.

---

## 20. Compiler, artifacts, and linker

### 20.1 Compiler object

```lm
src = """
class Greeter
  def greet(self, name: String) with Io.Write
    print("Hello #{name}!")
  end
end

do |name: String| with Io.Write
  Greeter().greet(name)
end
"""

env = CompileEnv(
  List[VerifiedModule](),
  List[(String, String)](),
  List[(String, DefinitionSpec)]()
)
options = CompileOptions(
  is_main: true,
  dynamic_result: false,
  late_definitions: false,
  late_functions: List[String](),
  late_classes: List[String]()
)
result = sys.compiler.compile("greeter", "greeter.lm", src, env, options)
```

`Compiler.Compile` is deterministic under its explicit inputs.

These inputs include names, source bytes, compile bindings, options, compiler identity, core identity, and ABI versions.

Blocking this operation prevents runtime code creation.

### 20.2 Artifact API

`Artifact` is an opaque untrusted code container.

```lm
case artifact.verify()
in Ok(module) then use(module)
in Err(error) then report(error.message)
end
```

`Artifact.verify()` performs `Compiler.Verify` and returns `Result[VerifiedModule,CodeError]`.

The verifier decodes the container and checks every function before it creates `VerifiedModule`.

An artifact can contain definitions, an entry, both, or neither.

Definitions have semantic hashes. The module and exact byte container have separate hashes.

### 20.3 Import slots and interfaces

An import slot includes name, full type/signature, effect row, mutability requirements where relevant, and optional exact code/class hash. An interface file is the canonical subset of artifact metadata needed by downstream compilation. It contains no executable source requirement and no ambient lookup rule.

### 20.4 Linking and typed entry values

```lm
case image.install(module, LinkEnv(providers))
in Ok(instance)
  entry = instance.entry[(String,), ()]()
  greeter = instance.class_def("Greeter")
  (entry.is_ok(), greeter.is_ok())
in Err(_)
  (false, false)
end
```

`Vm.install(module)` returns an `Instance`.

`Vm.install(code)` returns the installed binding selected by that code value.

`Vm.install(function)` is convenience syntax for `Vm.install(codeof(function))`.

The optional `LinkEnv` contains provider instances from that VM.

`FunctionCode[A,T]` and `ClassCode` are portable views into shared verified bytes.

`codeof(function)` creates `FunctionCode[A,T]` for a named monomorphic function.

`codeof(Class)` creates `ClassCode` for a class definition.

`codeof` does not install code and does not require a `Vm`.

Source, syntax, module, and command-line compilation use one binding publication rule.

Every exported function and class publishes stable slot metadata.

Publication does not make a static call late.

Reifying a local definition also finds its local dependency closure.

The closure follows named direct calls, construction, spawning, and nested bodies.

Direct references to each named closure dependency become late in the same compilation.

Definitions outside the closure remain static.

`FunctionCode[A,T].source()` returns `Option[DefinitionSource]`.

`ClassCode.source()` returns `Option[DefinitionSource]`.

`FunctionCode[A,T].definition()` returns `DefinitionSpec`.

`ClassCode.definition()` returns `DefinitionSpec`.

`DefinitionIdentity` contains `module_name`, `qualified_key`, `contract_hash`, and `implementation_hash`.

The module name is a logical namespace. It is not a filesystem path.

The qualified key names one definition in that namespace.

The contract hash identifies body-independent replacement compatibility.

A function contract excludes its binding name.

A class contract includes its qualified nominal family and complete replacement shape.

Function bodies, method bodies, and field default expressions do not enter a contract hash.

The implementation hash identifies the selected executable implementation.

A class implementation hash includes its class structure, generated constructor, methods, and static dependencies.

`DefinitionSpec` contains `identity`, `module_hash`, and `slots`.

The module hash identifies the complete verified source module.

It does not decide definition replacement compatibility.

Each `SlotSpec` stores its contract hash.

The compiler derives its slot key from the qualified binding and contract hash.

`DefinitionSpec.slots` contains the primary definition slot and required related slots.

`ExportEntry.iface_hash` identifies a named source interface for import invalidation.

It is not a definition replacement contract.

`IfaceSlotSpec.contract_hash` equals the corresponding bytecode slot contract hash.

The compiler uses `CompileEnv.definitions` to bind local declarations to these verified contracts.

`DefinitionSource` contains `path`, `syntax`, and `definition`.

The `definition` field contains the same `DefinitionSpec` data that `code.definition()` returns.

The source attachment does not affect semantic or verification hashes.

Source attachments contain diagnostic data and verified definition metadata. They do not affect semantic or verification hashes.

Capturing closures cannot become portable code values.

`VerifiedModule.entry_code[A,T]()` returns portable code for the entry function.

`VerifiedModule.function_code[A,T](name)` returns portable code for a named function.

`VerifiedModule.class_code(name)` returns portable code for a named class.

`Instance.entry[A,T]()` and `Instance.function[A,T](name)` return typed `FunctionDef[A,T]` results.

`Instance.entry_binding[A,T]()` and `Instance.function_binding[A,T](name)` return typed `FunctionBinding[A,T]` results.

`Instance.dynamic_entry()` requests the declared `DynValue` entry form.

`Instance.class_def(name)` returns one opaque `ClassDef` result.

`Instance.class_binding(name)` returns one opaque `ClassBinding` result.

An installed binding retains both a live slot address and its installation's immutable target.

`slot`, `spec`, `instance`, and `target` expose these parts through checked methods.

Activation through a function binding reads its current slot target.

Replacement through two bindings uses the address binding's slot and the target binding's immutable target.

`Vm.change` prepares one checked function or class update without publishing it.

The typed `change_*` methods also cover values and processes.

`Vm.replace_all` validates one list of prepared changes and publishes it atomically.

The operation rejects duplicate slots and stale slot versions.

Section 23.7 defines source bindings, batch replacement, and class revision rules.

Installation validates and commits code atomically. It does not execute the entry function.

Repeated direct installs reuse an exact self-contained artifact instance in one VM image.

An explicit `VerifiedModule` install remains a distinct module installation request.

### 20.5 Rows as verified theorems

The source checker proves each body row. Emitted typed bytecode carries enough metadata for independent verification.

`Compiler.Verify` checks stack, type, call, perform, and row consistency.

Only verified code can enter a VM installation. Runtime policy remains independent.

### 20.6 Compilation diagnostics

Diagnostics are deterministic frozen values with stable code, primary source span, zero or more labeled spans, message, and notes. Given identical source bytes, compile environment interfaces, options, compiler version, and ABI bundle, diagnostic ordering and semantic output are deterministic.

---

## 21. Bytecode and verifier

### 21.1 Execution model

Bytecode is typed stack code. Canonical artifact encoding is compact; load decodes it into fixed-size instruction records or an equivalent directly indexable form. Guest calls push explicit frames. The interpreter never uses host recursion for guest call depth.

### 21.2 Instruction families

Version 0.2 contains at least:

```text
CONST              LOAD_LOCAL          STORE_LOCAL
LOAD_CAPTURE       LOAD_FIELD          STORE_FIELD
LIST_NEW           LIST_GET            LIST_SET
MAP_NEW            MAP_GET             MAP_SET
CONSTRUCT          CONSTRUCT_VALUE     MAKE_CLOSURE
CALL_DIRECT        CALL_VIRTUAL        CALL_VALUE
INTRINSIC          PERFORM             PERFORM_VALUE
JUMP               JUMP_IF_FALSE       SWITCH_TAG
TYPE_TEST          CAST                POP
RETURN              FAULT
```

`CONSTRUCT_VALUE` constructs through a first-class class value while retaining its class signature. `PERFORM_VALUE` calls an identity-indexed first-class `Op[e,F]`; an identity-erased `Operation` cannot reach it.

Selectors, fields, classes, operations, intrinsics, types, and functions are canonical hash/name references in the artifact and numeric slots in loaded code.

### 21.3 Perform instruction

A perform operand identifies the exact operation slot, argument count/layout, reply type, and destination. Executing it writes a pending-request record and continuation PC before any host dispatch. This same record supports automatic dispatch, `drive`, waiting, inspection, and snapshots.

### 21.4 Typed control flow

Every instruction boundary has a statically known operand-stack shape and local type state. Branch targets must agree exactly. Calls must match callable signatures. Field operations must match resolved layouts. Returns must match the function result. A `PERFORM` identity/signature must agree with the claimed row.

### 21.5 Verifier scope

The verifier accepts a canonical artifact plus trusted ABI manifests and checks:

- container canonicality, bounds, hashes, section references, and version compatibility;
- all instruction decoding and target boundaries;
- local/operand initialization and types on every reachable edge;
- direct, virtual, class, closure, and first-class call signatures;
- sealed class layouts, field/class/tag relationships, constructor initialization states, override signature/row compatibility, and enum switch coverage metadata;
- intrinsic IDs/signatures/fuel formulas;
- operation IDs, argument/reply types, and row containment;
- scoped-designator escape and non-storing call positions;
- exception-free control flow and frame/stack maxima;
- constant graph validity and frozen status;
- import and definition reference integrity.

Unreachable code must still decode and satisfy structural constraints; an implementation may omit full type-state traversal of unreachable blocks only if no malformed reference can be hidden there.

### 21.6 Verified-code cache

Successfully verified code is cached by semantic hash plus verifier/ABI version. Loading another artifact that references that hash may reuse the result after checking its bytes/hash binding. A cache entry never substitutes bytes under a different hash.

---

## 22. Rust VM and reference data representation

This section is implementation-specific but deliberately concrete. It defines the reference construction, expected asymptotic behavior, and the performance invariants against which alternate implementations are compared.

### 22.1 Crate and ownership boundaries

The reference workspace separates immutable formats from mutable execution:

```text
lm-abi         canonical core/operation/intrinsic/fault manifests
lm-source      UTF-8 source, scanner, parser, spans, and diagnostics
lm-types       interned types, rows, subtyping, and local inference
lm-hir         resolved typed HIR and control-flow checking
lm-value       Value, TypeId, ObjRef, scalar semantics
lm-bytecode    serialized and decoded bytecode structures
lm-verify      artifact and bytecode verifier
lm-jit         verified native regions and executable memory
lm-link        artifact resolution, relocation, and code namespaces
lm-heap        per-VM heap, object table, collector, native shapes
lm-graph       freeze/copy/digest/boundary/snapshot graph engine
lm-vm          frames, interpreter, pending performs, policy
lm-host        root operations and async completion adapters
lm-proc        scheduler and mailboxes
lm-compiler    scanner through artifact emission
lm-testkit     conformance, corruption, and benchmark support
lm-cli         build/run/test/inspect tools
```

`lm-vm` depends on no filesystem, clock, socket, command-line, or compiler frontend.

`lm-jit` depends on explicit bytecode, verifier, value, and heap ABI data.

`lm-jit` does not depend on `lm-vm`.

`lm-host` receives validated values and designators, never an arbitrary mutable guest reference.

### 22.2 `Value`

`Value` is the canonical 16-byte runtime value.

The implementation uses a C-compatible tagged union with a 64-bit tag.

```text
tag:     u64
payload: u64
```

The stable tags are `Unit`, `Bool`, `Int`, `Float`, `Char`, `Obj`, `Op`, `Callback`, `EmptyCase`, and `Uninit`.

Tags are append-only. A removed variant leaves a reserved tag.

`Int` and `Float` retain all 64 payload bits.

Object references contain two 32-bit fields.

Float values always use the canonical NaN encoding.

Compile-time assertions check the size, alignment, tag width, and payload offset.

Heap arrays and native code use this same representation.

### 22.3 Heap references and object headers

A heap reference is `(slot: u32, generation: u32)`.

Each VM owns its object table.

One table page contains 1,024 stable entries.

A page address never changes after publication.

Each C-compatible entry contains a generation and one tagged state.

The state is `Dead` or `Live`.

A live entry contains one header and one tagged `Object` value.

The header stores the frozen flag, logical byte charge, and shared-allocation key.

The `Object` tag uses a stable 32-bit C representation.

Native code reads only object variants in the declared heap ABI.

Instance fields, list items, tuple items, and closure captures use `ValueArray`.

`ValueArray` is a C-compatible record containing a pointer, length, and capacity.

It owns one process-allocator allocation and supports fallible growth.

Native shape descriptors define tracing, write locations, transfer, snapshots, digest, and cleanup.

Compact text views use a separate fixed-page descriptor table.

Their references use a reserved generation tag and retain normal stale-reference checks.

Guest references cannot name another VM heap.

A boundary transfer creates values in the destination heap.

### 22.4 Allocation and collection

Allocation takes one free entry or appends one object-table entry.

The host process allocator supplies variable object payloads.

The final host binary selects the global process allocator.

Runtime library crates do not select it.

Collection uses stop-the-VM mark and sweep.

The collector does not move live entries or payloads.

Roots include frames, locals, operands, pending requests, native activations, image slots, and scoped host roots.

Marking is iterative and uses the same native shape table as graph operations.

Sweeping drops dead objects, advances their generations, and returns their slots to the free list.

Compact text descriptors use their own bounded sweep.

Shared immutable text and byte allocations use one reference ledger per heap.

There are no guest finalizers or collector reentrancy.

Heap limits are checked before committed growth.

An allocation failure leaves valid machine and heap state.

### 22.5 Code, classes, and generic applications

Verified code, classes, source maps, and core data are immutable shared host objects.

A namespace maps their identities to dense runtime slots.

A decoded instruction is a fixed 16-byte record containing opcode/flags and up to three `u32` operands. Loading resolves constant, code, class, type, selector, field, intrinsic, and operation hashes once. The interpreter does not parse variable-length bytecode or hash names in its hot loop.

One interface call packs 16-bit interface and method indices into one operand. Each related table can contain at most 65,536 addressable entries.

Class slots contain field offsets and flattened dispatch rows.

Virtual dispatch loads the runtime class and selector target.

Generic applications share code and object layout.

Closed type records retain their argument types for reflection and boundary checks.

### 22.6 Frames, locals, and operands

Frames are explicit records:

```text
function version
block and instruction
local base
operand base
capture reference
type environment
```

Locals and operands occupy VM-owned `Value` arenas.

Frames occupy a separate frame vector.

An interpreter call checks limits, reserves storage, writes a frame, and transfers control.

A return truncates the arenas and leaves its result for the caller.

Interpreter calls never consume host call-stack depth.

The interpreter retains indices across any operation that can grow storage or collect.

### 22.7 Interpreter loop and cost model

The interpreter uses one `match` loop over decoded instructions.

`run`, `step`, and `drive` use the same loop with different stop modes.

The loop materializes current position at calls, performs, faults, safepoints, and exits.

Ordinary verified instructions do not run subtype checks. Integer and float operations inspect only the known scalar tag; field and selector offsets are pre-resolved; locals and operands are bounds-safe by verified maxima plus runtime arena limits.

Reference performance invariants:

- no heap allocation per ordinary instruction;
- no event or request allocation inside `run`;
- no host-stack growth with guest call depth or nested VM depth;
- one exact-action and at most one group-action load per perform;
- no textual lookup after load;
- request materialization only when `drive`/`step` exits;
- no heap allocation for `ReplySink` validation;
- snapshots and graph operations proportional to reachable encoded data, not heap capacity.

The benchmark suite measures dispatch, calls, allocation, collections, effects, VM control, snapshots, and proc communication.

### 22.8 Pending performs and typed requests

`VmState` reserves one pending record:

```text
operation_slot
argument_base / argument_count
argument_tuple_type
reply_type
reply_destination
continuation_pc
ordinal
state: none | asked | waiting
host_completion_token      # host-only; never serialized live
```

While executing normally, arguments remain in verified operand slots. `drive` exits after the record is complete and before policy lookup. A `Call` pattern checks the exact operation slot and binds a holder token carrying VM identity, ordinal, argument tuple type, and reply type; `PendingCall.args()` boundary-encodes that tuple lazily. `answer` validates the token again before installing a reply.

A routed request adds a surface, a performing target, and a saved policy cursor. Nested control adds one parent-to-child edge.

`ReplySink` is a stack record. It centralizes one continuation check and allocates no guest or Rust heap object.

The snapshot form serializes semantic fields, never a live completion token. A pending waiting operation is a live host attachment and blocks snapshot creation with `ResourceActive` (17.4, 17.6).

### 22.9 `List`, `Map`, strings, and bytes

A `List[T]` object stores length, capacity, and a reference to a contiguous `Value` buffer. `push` is amortized O(1); indexed access is O(1); insertion/removal is O(n). Frozen lists keep the same representation and reject writes through the common frozen barrier.

A `Map[K,V]` stores entries and semantic hashes in insertion order.

It also stores a derived open-addressed index from private hash to entry position.

Replacing a value retains its position.

Removal changes an entry into a tombstone.

Lookup continues through tombstones, and iteration skips them.

The map compacts when tombstones pass its bounded threshold.

Compaction preserves the relative insertion order of live entries.

Lookup is expected O(1). Removal is amortized O(1). Iteration is O(n).

`K` must implement `Hashable`.

Equal keys must return equal semantic hashes.

A semantic hash must remain stable while its value is frozen.

A user heap key must be frozen before insertion. A mutable key faults with `MutableMapKey`.

The runtime mixes each semantic hash with a private process key.

The process key is not guest state. Snapshots rebuild derived map indexes with the active process key.

Snapshots store each entry semantic hash. Restoration does not call guest code.

Insertion order, equality, serialization, and digest do not depend on bucket order. Fuel charges use logical key size, not actual probe count.

Bool, Int, Float, Text, Char, and Bytes use native hash and equality instructions.

String and Substring are the concrete Text key types.

A frozen user class can implement `Hashable` and become a map key directly.

Other user classes require an explicit successful `freeze` before insertion.

String, Substring, and Bytes use immutable reference-counted byte storage. Each value stores one visible byte range.

A String contains valid UTF-8. A String also caches its scalar count and ASCII state.

A String can retain at most `max(4096, 2 * byte_len)` bytes of backing capacity. Construction and conversion enforce this limit.

A Substring is an explicit view. It can retain an allocation of any size until the view dies.

`Text.to_string` returns its String receiver unchanged. It copies a Substring into bounded String storage.

`Substring.compact` has the same bounded result as `to_string`.

A Bytes slice is also an explicit view. `Bytes.compact` copies the visible bytes into a new allocation.

Text and Bytes can share one physical byte allocation. A heap charges this allocation once for all its local views.

`split` and `lines` can allocate immutable views in one descriptor-page batch.

One owner record retains the shared root for the complete batch.

A nested batch from one compact view reuses that owner record.

Each result remains a normal `Value::Obj` reference.

Collection checks each descriptor generation and releases the owner after its final view dies.

Boundary transfer, compaction, and snapshot encoding materialize an ordinary `Substring` object.

The heap charges the same logical view cost for both storage classes.

`Text.bytes` shares storage. `Bytes.utf8_view` validates UTF-8 and returns a shared Substring.

`Bytes.utf8` validates UTF-8 and returns a bounded String. It copies only when the retention bound requires a copy.

Bytes accepts every byte sequence. Construction does not validate UTF-8.

Each Bytes view caches its UTF-8 validation result. `utf8` and `utf8_view` reuse this result.

Text and Bytes cache their content hashes after the first map lookup. The caches do not affect snapshots or graph digests.

String and Substring use one Text hash domain. Their map equality compares visible UTF-8 content across both concrete types.

`StringBuilder` and `ByteBuffer` are final nominal core classes. Their native payloads stay holder-local.

Each builder uses one private growable buffer. `build` copies the visible content and leaves the builder active.

StringBuilder tracks its scalar count and ASCII state. `finish` transfers this metadata with the buffer.

`ByteBuffer.finish` transfers its buffer. `StringBuilder.finish` transfers its buffer when the String retention bound permits this transfer.

`StringBuilder.finish` compacts excessive retained capacity. Both methods invalidate the builder.

A later builder operation faults with `InvalidVmState`.

`ByteBuffer.build` and `ByteBuffer.finish` never validate UTF-8. Bytes validates UTF-8 only during an explicit text conversion.

### 22.10 Graph engine

One non-recursive engine drives mark, deep freeze, frozen verification, boundary transfer, structural copy, canonical digest, snapshot encoding, and detached inspection. Mode-specific visitors share shape traversal and an identity table but have separate result state; this avoids one giant branch-heavy inner loop while preserving one definition of graph reachability and field order.

- `freeze`: O(V + E), sets bits only after all reachable objects validate;
- transfer/copy: O(V + E + bytes), preserves cycles and sharing;
- digest: O(V + E + bytes), assigns deterministic traversal ordinals and domain-separates backreferences;
- snapshot write: O(reachable encoded bytes plus machine-world state);
- external snapshot load: O(container bytes), producing decoded and structurally admitted state.

Depth never consumes the Rust stack. Every mode has object, edge, byte, and work limits. Transfer and copy commit destination state only after preflight succeeds.

### 22.11 Policy representation

A policy table contains one dense exact-action vector indexed by operation slot and one dense group-action vector indexed by group slot. An action is a compact tagged record for block, pass, or mock. Mock records hold verified code, a sendable capture graph, and a work budget.

The default block action requires no allocation. Live edits replace one action under the table's synchronization primitive. A running VM reads an immutable action snapshot for the current perform; revocation affects the next lookup.

### 22.12 Procs, snapshot barriers, and asynchronous host work

A proc owns one `VmState` on one scheduler task.

The runtime provides deterministic and parallel scheduler modes.

Both modes use one coordinator state machine and one report commit path.

The coordinator owns the complete `World` and every cross-machine commit.

It owns task order, activation stacks, wait indexes, policies, image slots, and snapshot barriers.

A task can drive a stack of held VMs.

Only the active machine enters an execution lease.

An execution lease gives one worker exclusive machine ownership.

A worker receives immutable verified code and bounded execution resources.

It receives no mutable `World` reference and invokes no host operation.

One VM never executes concurrently.

Deterministic mode executes leases on the coordinator thread.

Parallel mode uses a bounded worker pool and one FIFO lease queue.

The default deterministic and parallel poll intervals are 16,384 guest instructions.

An explicit fixed quantum replaces demand-driven polling.

At a poll, a worker continues when no other lease waits.

It rotates its lease when another lease waits.

This rule keeps the native poll cheap when no coordinator work exists.

Parallel mode probes work on the coordinator before it activates the pool.

Boundary-heavy single-task work stays on the coordinator path.

An embedding host can share one worker pool across several worlds.

Each report contains a world identity and a unique lease token.

The coordinator rejects stale, duplicate, or foreign reports.

A worker returns for effects, waits, sends, spawns, nested VM control, policy edits, snapshots, terminal values, and faults.

The coordinator commits that action before it dispatches the task again.

Cross-machine transfer requires both machines to become resident.

The coordinator recalls a leased destination before it commits the transfer.

The source stays blocked until the transaction completes.

The coordinator commit order defines accepted mailbox order.

One sender preserves its message order.

Different senders have no relative ordering guarantee in parallel mode.

Deterministic mode preserves FIFO task and trace order.

Parallel mode preserves results and machine-local instruction order.

It does not promise repeatable cross-task interleavings.

Shared world fuel uses bounded atomic claims.

Unused fuel returns when a lease reports.

Machine heap accounting remains local to its exclusive lease.

World machine, image, child, and wait limits remain coordinator-owned.

Wake indexes name mailbox changes, terminal states, waits, and host completions.

A state change wakes only tasks registered for its key.

The scheduler calls the host wait operation only when no task is ready.

An asynchronous completion sink contains machine identity, request ordinal, and one typed reply encoder.

Completion queues contain no Rust reference into a guest heap.

A snapshot barrier requests each selected lease at a safepoint.

It closes the selected set over machine handles discovered during capture.

It records one mailbox acceptance cut.

The barrier resumes the original world after success or failure.

Host scheduler objects never enter snapshot bytes.

Each VM also owns a host-side resource registry outside its heap.

The registry records resource kind, scope identity, request ordinal, and cleanup state.

Snapshot preflight reads the registry and guest graph to find live host attachments.

### 22.13 Native compilation

The reference runtime provides `Interpreter`, `Auto`, and `Native` engine modes.

The host selects the mode.

Engine policy is not guest state and never enters artifacts or snapshots.

Engine choice cannot change results, faults, host actions, or retired instruction counts.

Verified LMBC is the only native compiler input.

The verifier proves code structure and instruction types.

The JIT consumes verifier program-point metadata.

It does not repeat type checking.

An entry plan names only values that the native region can read.

An entry guard checks their runtime representations before entry.

A failed guard changes no machine state and resumes the interpreter at the same position.

The guard does not inspect dormant locals or unrelated machines.

An externally restored world can contain structurally valid values with wrong tags.

Native entry guards and checked native heap loads contain those values.

The world restore flag also enables checked interpreter boundaries.

That flag does not select the execution engine.

Deterministic and parallel execution use one engine boundary:

```text
run_turn(exclusive machine, immutable code, environments, limits, engine)
  -> turn result
```

Compiled code captures no worker identity or mutable `World` pointer.

One native activation owns all compiled frames until an observable boundary.

It contains contiguous scalar storage, frame records, entry cells, fuel, and one exit record.

Each native frame records its function, resume position, scalar window, operand height, type environment, and physical stack charge.

A direct call loads one stable function entry cell.

A present entry pushes a logical frame and performs one native call.

A missing entry exits before the call retires.

The interpreter executes the unchanged call and can enter native code at another segment head.

A native return removes one native frame and leaves its result for the caller.

Native recursion uses the same convention.

One physical native call chain has a 256 KiB stack budget.

The compiler measures each emitted function frame and adds a fixed ABI reserve.

An excess call exits before retirement and unwinds the physical chain.

The logical native frame table remains live.

Execution retries the call with a fresh physical budget.

The guest frame limit remains authoritative.

Native activation storage grows geometrically outside generated call code.

Storage growth does not materialize canonical `VmState`.

The planner can inline bounded direct callees with compatible summaries.

An inline child retains exact fuel, faults, and parent continuation state.

A region contains one verified function control-flow graph.

A segment is one fixed-cost path between reservation heads and engine boundaries.

Conditional branches split bytecode blocks into segments.

A retryable instruction starts one segment.

A retry resumes at that segment head.

Native code can cross segment edges and loop backedges without returning to Rust.

Every bytecode instruction has one exhaustive treatment:

- direct register code;
- guarded memory access;
- an inline fast path with one typed slow path;
- a native call;
- one fixed typed runtime helper;
- an observable engine exit.

The opcode ledger also records replay, fault-stack, and exit behavior.

A new opcode cannot compile until this ledger classifies it.

No temporary instruction treatment exists.

An observable instruction can exit, execute once in the interpreter, and resume at a native segment head.

Fuel counts retired bytecode instructions.

Native code reserves hard fuel before each segment chain.

Several acyclic segments can share one reservation.

Insufficient hard fuel exits at the current segment head.

The interpreter executes the partial reservation and lands the exact stop.

`vm.step()` and bounded drive retain instruction-exact stopping.

Scheduler polls occur at deterministic reservation heads.

An idle poll rearms inside native code.

A requested poll returns canonical state and the exact retired count.

Guest faults use explicit exit records, not Cranelift traps.

Fault exits preserve the post-instruction position and exact operand consumption.

Generated code reads the canonical `Value`, object table, and `ValueArray` layouts.

It maintains no parallel object representation.

A validated entry address remains stable until a safepoint.

A movable payload pointer lives only between calls or under an explicit stability proof.

Any call that can grow or collect ends that proof.

Native code reloads the payload pointer after the proof ends.

Direct fast paths cover common field, tuple, list, map, builder, and scalar text operations.

Each fast path performs generation, shape, bounds, and frozen checks that its memory access requires.

Slow paths use fixed typed signatures.

No generic operation dispatcher decodes a runtime contract.

A helper remains appropriate only when its work dominates the call cost.

An effect stores canonical request arguments before it leaves generated code.

A scalar effect reply can update a retained native activation.

Inspection, recall, snapshots, faults, engine changes, and canonical stack mutation force materialization.

Materialization reconstructs every logical frame from the activation records.

An ordinary scheduler poll can retain the native activation.

`Auto` starts in the interpreter and samples direct calls and loop backedges.

It uses relaxed counters and no profiling mutex.

A cache lookup occurs before specialization work.

Permanent compiler rejection stops later probes for that function.

Code-cache pressure remains retryable and never becomes a compiler verdict.

Repeated quick exits can disable native entry for one compiled function.

`Native` bypasses productivity policy for differential testing.

One engine cache belongs to an exact namespace layout.

Dense function slots own compiler verdicts and stable entry cells.

The host limits executable code by bytes.

Published native code is immutable and releases executable memory on final drop.

Cold and warm measurements remain separate.

A warm measurement uses one stable namespace and engine.

The JIT reports compilation, entry, retirement, guard, helper, allocation, continuation, materialization, and exit counters.

Differential tests compare complete state across both drive modes and all supported fuel boundaries.

### 22.14 Unsafe-code policy

Unsafe Rust is confined to page allocation, raw byte-copy primitives, and optional C ABI shims. Every unsafe module states its invariants and has Miri/property tests. The verifier, byte decoder, snapshot loader, graph algorithms, policy table, interpreter state machine, and host dispatch are safe Rust. Fuzz builds enable generation checks and expensive heap validation after every instruction transition.

## 23. Standard host operations

Operation names, signatures, groups, hashes, and ABI versions come from the canonical operation manifest. The following is the minimum version 0.2 surface; ordinary error conditions use frozen result values.

### 23.1 I/O

```text
Io.ReadBytes   (Int) -> Result[Bytes, IoError]
Io.Write       (Bytes) -> Result[Int, IoError]
Io.WriteError  (Bytes) -> Result[Int, IoError]
```

Writes can report partial progress.

Line-ending policy belongs to core and standard wrappers.

Reads can suspend.

Each write operation makes one platform write attempt.

The host flushes accepted bytes before it returns `Ok`.

A closed output pipe returns `IoError.BrokenPipe`.

Diagnostic reporting treats its own closed pipe as a completed report.

### 23.2 File system

```text
Fs.Open        (Path, OpenOptions) -> Result[FileHandle, FsError]
Fs.Read        (FileHandle, Int) -> Result[Bytes, FsError]
Fs.Write       (FileHandle, Bytes) -> Result[Int, FsError]
Fs.Seek        (FileHandle, SeekFrom) -> Result[Int, FsError]
Fs.Flush       (FileHandle) -> Result[(), FsError]
Fs.Sync        (FileHandle) -> Result[(), FsError]
Fs.Close       (FileHandle) -> Result[(), FsError]
Fs.CurrentDir  () -> Result[Path, FsError]
Fs.Stat        (Path) -> Result[FileInfo, FsError]
Fs.ReadDir     (Path, Int) -> Result[List[Result[DirEntry, FsError]], FsError]
Fs.CreateDir   (Path) -> Result[(), FsError]
Fs.RemoveFile  (Path) -> Result[(), FsError]
Fs.RemoveDir   (Path) -> Result[(), FsError]
Fs.Rename      (Path, Path, RenameMode) -> Result[(), FsError]
Fs.SyncDir     (Path) -> Result[(), FsError]
```

A live `FileHandle` names one resource entry and one service binding. The binding can belong to the root host or a driver. Every alias closes together. An open entry blocks snapshot creation. A closed handle remains typed machine state and restores as closed. Raw file handles have no checkpoint contract.

File operations can suspend their proc. The host adapter performs blocking platform work outside the scheduler thread.

#### Program inputs

```text
Args.Get  () -> [String]
Env.Get   (String) -> Result[Option[String], EnvError]
```

`Args.Get` returns a fresh guest-owned list.

`Env.Get` returns `Ok(None)` when the name is absent.

### 23.3 Pipes and child programs

```text
Pipe.Open  () -> Result[(PipeReader, PipeWriter), PipeError]
Pipe.Read  (PipeReader, Int) -> Result[Bytes, PipeError]
Pipe.Write (PipeWriter, Bytes) -> Result[Int, PipeError]
Pipe.Close (PipeEnd) -> Result[(), PipeError]

Exec.Spawn     (ExecSpec) -> Result[Child, ExecError]
Exec.Wait      (Child) -> Result[ChildStatus, ExecError]
Exec.Terminate (Child) -> Result[(), ExecError]
Exec.Kill      (Child) -> Result[(), ExecError]
Exec.Close     (Child) -> Result[(), ExecError]
```

`PipeEnd` is the sealed native parent of `PipeReader` and `PipeWriter`.

`Pipe.Read` and `Exec.Wait` can create wait sources.

Live pipe ends and child handles block snapshot creation.

The host invokes no shell unless a library names one explicitly.

`ChildEnv.Inherit` passes the host environment without changes.

`ChildEnv.Exact` clears inherited values before it adds the supplied map.

`ChildEnv.Overlay` inherits values before it adds or replaces supplied entries.

An overlay cannot remove an inherited value.

Child inputs, ownership, limits, and cleanup follow the resource rules in sections 16.4 and 25.5.

### 23.4 Clock and randomness

```text
Clock.Now       () -> Int             # UTC nanoseconds from Unix epoch
Clock.Monotonic () -> Int             # host-monotonic nanoseconds
Clock.Sleep     (Int) -> ()

Rand.Int        (Int, Int) -> Int  # half-open [low, high)
Entropy.Bytes   (Int) -> Result[Bytes, EntropyError]
```

`Rand.Int` validates the range before sampling. Rejection sampling removes modulo bias.

`Entropy.Bytes` uses the host's secure entropy source. It does not advance deterministic `Rand.Int` state.

### 23.5 Terminals and process signals

```text
Tty.IsTerminal (StdStream) -> Bool
Tty.Size       (StdStream) -> Result[TtySize, TtyError]
Tty.EnterRaw   () -> Result[RawMode, TtyError]
Tty.ExitRaw    (RawMode) -> Result[(), TtyError]

Signal.Open    (List[SignalKind]) -> Result[SignalStream, SignalError]
Signal.Next    (SignalStream) -> Result[SignalKind, SignalError]
Signal.Close   (SignalStream) -> Result[(), SignalError]
```

`StdStream` contains `Input`, `Output`, and `Error`.

`TtySize` contains positive `columns` and `rows` values.

`RawMode` stores ownership of one standard-input raw mode.

The host restores the exact saved terminal state when the resource closes.

Machine completion and machine fault also close this resource.

`SignalKind` contains `Interrupt` and `Terminate`.

`SignalStream` delivers signals only at normal host boundaries.

`Signal.Next` can create a wait source.

Cancellation keeps a selected signal in its stream until another request commits it.

Live `RawMode` and `SignalStream` resources block snapshot creation.

Signal delivery, escalation, and cleanup follow the resource rules in sections 16.4 and 25.5.

### 23.6 Networking

```text
Dns.Resolve       (String, Int) -> Result[[SocketAddress], NetError]

Tcp.Connect       (SocketAddress) -> Result[TcpStream, NetError]
Tcp.Listen        (SocketAddress, Int) -> Result[TcpListener, NetError]
Tcp.Accept        (TcpListener) -> Result[(TcpStream, SocketAddress), NetError]
Tcp.Read          (TcpStream, Int) -> Result[TcpRead, NetError]
Tcp.Write         (TcpStream, Bytes) -> Result[Int, NetError]
Tcp.Shutdown      (TcpStream, Shutdown) -> Result[(), NetError]
Tcp.LocalAddress  (TcpResource) -> Result[SocketAddress, NetError]
Tcp.PeerAddress   (TcpStream) -> Result[SocketAddress, NetError]
Tcp.Close         (TcpResource) -> Result[(), NetError]

Tls.Handshake     (TcpStream, String, Int, [Bytes], [Bytes], Int, Int)
                  -> Result[TlsStream, TlsError]
Tls.ServerHandshake
                  (TcpStream, [Bytes], Bytes, [Bytes], Int, Int)
                  -> Result[TlsStream, TlsError]
Tls.Read          (TlsStream, Int) -> Result[TcpRead, TlsError]
Tls.Write         (TlsStream, Bytes) -> Result[Int, TlsError]
Tls.Shutdown      (TlsStream) -> Result[(), TlsError]
Tls.LocalAddress  (TlsStream) -> Result[SocketAddress, TlsError]
Tls.PeerAddress   (TlsStream) -> Result[SocketAddress, TlsError]
Tls.Close         (TlsStream) -> Result[(), TlsError]

Udp.Bind          (SocketAddress) -> Result[UdpSocket, NetError]
Udp.SendTo        (UdpSocket, SocketAddress, Bytes) -> Result[(), NetError]
Udp.RecvFrom      (UdpSocket) -> Result[UdpDatagram, NetError]
Udp.LocalAddress  (UdpSocket) -> Result[SocketAddress, NetError]
Udp.Close         (UdpSocket) -> Result[(), NetError]
```

`TcpResource` is the sealed native parent of `TcpStream` and `TcpListener`.

`TlsStream` is a separate final native resource class.

`UdpSocket` is a separate final native resource class.

Live TCP, TLS, and UDP resources block snapshot creation.

`TcpRead.Data` carries nonempty bytes. `TcpRead.End` reports orderly peer closure.

`TcpStream.read` returns `Result[Bytes,NetError]`.

`TlsStream.read` returns `Result[Bytes,TlsError]`.

Empty bytes report orderly end of input for a positive read count.

These methods implement `ByteReader` with their exact read rows.

`FileHandle`, `PipeWriter`, `TcpStream`, and `TlsStream` implement `ByteWriter`.

`FileHandle` and `PipeReader` also implement `ByteReader`.

A submitted TLS handshake consumes its TCP stream on every result.

`Udp.RecvFrom` can create a wait source.

A UDP receive returns one complete datagram or an error.

UDP operations accept at most 65,535 payload bytes.

Zero-length UDP datagrams are valid.

Certificate roots, server names, versions, ALPN, and buffers are explicit values.

The transparent effect sets include `Tcp.Stream`, `Tcp.Listener`, `Tcp.Client`, and `Tcp.Server`.

They also include `Tls.Stream`, `Tls.Client`, `Tls.Server`, and both HTTP client groups.

The UDP effect sets include `Udp.Socket` and `Udp`.

An effect set expands to a finite exact-operation closure.

It creates no runtime operation and hides no request from a driver.

`Dns.Resolve` uses the operating-system resolver.

The operation can inspect host files, resolver settings, and configured name services.

Every connected or accepted TCP stream has `TCP_NODELAY` enabled.

The host closes the stream when it cannot enable this option.

Network handles follow the resource rules in sections 16.4 and 25.5.

### 23.7 VM operations

Generic signatures below are manifest-level schemas instantiated by the compiler. `A` is an argument-tuple type, `T` is the machine's terminal result, `R` is one pending operation's reply type, and `Fn[A,T,e]` is manifest metanotation for a callable with argument tuple `A`, result `T`, and row `e`.

```text
Vm.New                         () -> Vm
Vm.Activate[A,T,e]             (Vm, Fn[A,T,e], control A)
                                -> Result[Run[T], CodeError]
Vm.ActivateOrFault[A,T,e]      (Vm, Fn[A,T,e], control A) -> Run[T]
Vm.Run[T]                      (Run[T]) -> Result[T, Fault]
Vm.Step[T]                     (Run[T]) -> StepEvent[T]
Vm.Drive[T]                    (Run[T]) -> DriveEvent[T]
Vm.Answer[T,A,R]               (Run[T], PendingCall[A,R], R) -> ()
Vm.Reject[T]                   (Run[T], Request, Fault) -> ()
Vm.Dispatch[T]                 (Run[T], Request) -> ()
Vm.Table[T]                    (Run[T]) -> PolicyTable
Vm.SnapshotHeld[T]             (Run[T])
                                -> Result[RunSnapshot[T], SnapshotError]
Vm.Branch[T]                   (Run[T])
                                -> Result[Run[T], BranchError]
Vm.BranchAnswer[T,A,R]         (Run[T], PendingCall[A,R], R)
                                -> Result[Run[T], BranchError]
Vm.SnapshotSelf                ()
                                -> Result[VmSnapshot, SnapshotError]
Vm.LoadSnapshot                (Bytes)
                                -> Result[VmSnapshot, SnapshotError]
Vm.Restore[T]                  (Vm, RunSnapshot[T])
                                -> Result[Run[T], RestoreError]
Vm.Handles[T]                  (Run[T]) -> List[ResourceHandle]
Vm.Resource[T,R]               (Run[T], R) -> ResourceHandle
Vm.ServeFile[T]                (Run[T], PendingCall[(String, OpenOptions),
                                Result[FileHandle, FsError]]) -> ResourceHandle
Vm.ResourceIsOpen              (ResourceHandle) -> Bool
Vm.ResourceClose               (ResourceHandle) -> Bool
Vm.ResourceKind                (ResourceHandle) -> String
Vm.ResourceSame                (ResourceHandle, ResourceHandle) -> Bool
Vm.DriveWait[T]                (Run[T]) -> Wait[DriveEvent[T]]
Vm.DriveFor[T]                 (Run[T], Int) -> Option[DriveEvent[T]]
Vm.SnapshotWaitHeld[T]         (Run[T], Int)
                                -> Result[RunSnapshot[T], SnapshotError]
Vm.ServeTcpStream[T]           (Run[T], PendingCall, SocketAddress)
                                -> ResourceHandle
Vm.ServeTcpListener[T]         (Run[T], PendingCall[SocketAddress,
                                Result[TcpListener, NetError]])
                                -> ResourceHandle
Vm.ServeTlsStream[T]           (Run[T], PendingCall) -> ResourceHandle
Vm.Artifact                    (Bytes) -> Artifact
Vm.Install[X]                  (Vm, X)
                                -> Result[Installed[X], CodeError]
Vm.InstanceEntry[A,T]          (Instance)
                                -> Result[FunctionDef[A,T], CodeError]
Vm.InstanceFunction[A,T]       (Instance, String)
                                -> Result[FunctionDef[A,T], CodeError]
Vm.InstanceSlotFor             (Instance, SlotSpec)
                                -> Result[Slot, CodeError]
Vm.InstanceSlotSpec            (Instance, String)
                                -> Result[SlotSpec, CodeError]
Vm.ActivateDef[A,T]            (Vm, FunctionDef[A,T] | FunctionBinding[A,T],
                                control A)
                                -> Result[Run[T], CodeError]
Vm.ReplaceFunction[A,T]        (Vm, Slot | FunctionBinding[A,T],
                                FunctionDef[A,T] | FunctionBinding[A,T])
                                -> Result[(), CodeError]
Vm.InstallWith[X]              (Vm, X, LinkEnv)
                                -> Result[Installed[X], CodeError]
Vm.InstanceClass               (Instance, String)
                                -> Result[ClassDef, CodeError]
Vm.ReplaceClass                (Vm, Slot | ClassBinding,
                                ClassDef | ClassBinding)
                                -> Result[(), CodeError]
Vm.ReplaceValue[T]             (Vm, Slot, T) -> Result[(), CodeError]
Vm.ReplaceProcess[M,R]         (Vm, Slot, Handle[M,R])
                                -> Result[(), CodeError]
Vm.ChangeFunction[A,T]         (Vm, Slot | FunctionBinding[A,T],
                                FunctionDef[A,T] | FunctionBinding[A,T])
                                -> Result[SlotChange, CodeError]
Vm.ChangeClass                 (Vm, Slot | ClassBinding,
                                ClassDef | ClassBinding)
                                -> Result[SlotChange, CodeError]
Vm.ChangeValue[T]              (Vm, Slot, T)
                                -> Result[SlotChange, CodeError]
Vm.ChangeProcess[M,R]          (Vm, Slot, Handle[M,R])
                                -> Result[SlotChange, CodeError]
Vm.ReplaceAll                  (Vm, List[SlotChange])
                                -> Result[(), CodeError]
Vm.RunSnapshotBytes[T]         (RunSnapshot[T])
                                -> Result[Bytes, SnapshotError]
Vm.SnapshotBytes               (VmSnapshot)
                                -> Result[Bytes, SnapshotError]
Vm.SnapshotVm                  (Vm)
                                -> Result[VmSnapshot, SnapshotError]
Vm.RestoreVm                   (VmSnapshot) -> Result[Vm, RestoreError]
Vm.ModuleEntryCode[A,T]        (VerifiedModule)
                                -> Result[FunctionCode[A,T], CodeError]
Vm.ModuleFunctionCode[A,T]     (VerifiedModule, String)
                                -> Result[FunctionCode[A,T], CodeError]
Vm.ModuleClassCode             (VerifiedModule, String)
                                -> Result[ClassCode, CodeError]
Vm.InstanceEntryBinding[A,T]   (Instance)
                                -> Result[FunctionBinding[A,T], CodeError]
Vm.InstanceFunctionBinding[A,T](Instance, String)
                                -> Result[FunctionBinding[A,T], CodeError]
Vm.InstanceClassBinding        (Instance, String)
                                -> Result[ClassBinding, CodeError]
Vm.BindingSlot                 (FunctionBinding | ClassBinding)
                                -> Result[Slot, CodeError]
Vm.BindingSpec                 (FunctionBinding | ClassBinding)
                                -> Result[SlotSpec, CodeError]
Vm.BindingInstance             (FunctionBinding | ClassBinding)
                                -> Result[Instance, CodeError]
Vm.BindingFunctionTarget[A,T]  (FunctionBinding[A,T])
                                -> Result[FunctionDef[A,T], CodeError]
Vm.BindingClassTarget          (ClassBinding)
                                -> Result[ClassDef, CodeError]
```

This table is the complete public `Vm` operation set for version 0.2.

`Installed[VerifiedModule]` is `Instance`.

`Installed[FunctionCode[A,T]]` is `FunctionBinding[A,T]`.

`Installed[ClassCode]` is `ClassBinding`.

`Installed[Fn[A,T,e]]` is `FunctionBinding[A,T]` for a supported named function.

The function convenience form uses the corresponding `FunctionCode` result.

A binding's slot is its mutable address.

A binding's target is the immutable definition from its own installation.

`Instance.slot_spec(name)` returns one portable stable slot identity.

`Instance.slot_for(spec)` resolves that identity inside the receiving instance.

Dense slot indices remain internal. No public method accepts one.

A class slot target contains one nominal class identity and one constructor version.

`Vm.ReplaceClass` changes future construction. It does not change existing objects.

Each successful replacement increments the changed slot version.

Each `Vm.Change*` operation captures the current slot version without publishing a target.

`Vm.ReplaceAll` publishes all valid changes together or publishes none.

This section defines the complete slot contracts and replacement rules.

The held, receiverless, and full VM forms use separate exact operation identities. They share one snapshot implementation family.

`Vm.RestoreDynamic` accepts a `VmSnapshot` with a distinguished run.

It returns `Result[Run[DynValue],RestoreError]`.

A full VM snapshot has no distinguished run. `Vm.RestoreVm` restores that image without selecting a run.

`Vm.Handles` returns controls for the live resources in the controlled
machine world. A resource control stays with its holder.

`Vm.Resource` accepts every native host resource value.

Each `Serve` operation requires a compatible current typed call.

`Vm.ResourceSame` matches two controls only while their shared entry
is live. A closed control never matches.

### 23.8 Proc operations

A proc handle carries both mailbox and terminal result types:

```text
Proc.Run[R]         (Run[R]) -> Handle[Never,R]
Proc.RunClosure[R,e] (() -> R with e) -> Handle[Never,R]
Proc.Spawn[M,R,A]   (Class[Proc[M]], control A) -> Handle[M,R]
Proc.Send[M,R]      (Handle[M,R], M) -> SendResult
Proc.Close[M,R]     (Handle[M,R]) -> SendResult
Proc.Recv[M]        (proc self) -> Recv[M]
Proc.RecvWait[M]    (proc self) -> Wait[Recv[M]]
Proc.Done[M,R]      (Handle[M,R]) -> Result[R,Fault]
Proc.Pause[M,R]     (Handle[M,R]) -> Result[Run[R], ProcError]
Proc.Resume[M,R]    (Handle[M,R]) -> Result[(), ProcError]
Proc.SnapshotWait[M,R] (Handle[M,R], Int)
                       -> Result[RunSnapshot[R], SnapshotError]
```

A proc with no mailbox uses `Never` as `M`; such a handle has no callable `send` method.

`Proc.SnapshotWait` first tries an immediate capture. It parks the caller only when a live resource blocks capture.

Fuel counts target-world instructions. Host completion time does not consume fuel.

### 23.9 Wait operations

```text
Wait.Wait[T]          (Wait[T]) -> T
Wait.Choose[A,B]      (Wait[A], Wait[B]) -> Wait[Choice[A,B]]
Wait.Cancel[T]        (Wait[T]) -> Bool
Wait.Any[T]           (List[Wait[T]]) -> (Int,T)
```

Wait tokens are holder-local and one-shot. Section 7.4 defines select syntax.

`Wait.Any` supports a homogeneous runtime-sized wait set.

The operation manifest marks each exact operation that can create a wait source.

Preparation keeps consumable input until selection commits.

Cancellation keeps that input available to the same logical resource.

Readiness commits one source atomically. The scheduler indexes each pending source by its stable wait key.

### 23.10 Compiler and reflection

```text
Compiler.Compile       (String, String, String, CompileEnv, CompileOptions)
                       -> Result[Artifact, CompileErrors]
Compiler.CompileSyntax (String, String, SyntaxNode, CompileEnv, CompileOptions)
                       -> Result[Artifact, CompileErrors]
Compiler.Verify        (Artifact)
                       -> Result[VerifiedModule, CodeError]
Reflect.ParseSyntax    (String) -> SyntaxParse
```

Both compile operations receive a logical module name and a diagnostic source name.

`Compiler.Compile` then receives source text.

`Compiler.CompileSyntax` instead receives one syntax node.

The logical module name creates qualified declaration keys.

The diagnostic source name affects only diagnostics and debug records.

`Compiler.Verify` performs independent bytecode verification. The compiler cannot mint `VerifiedModule` values directly.

`Reflect.ParseSyntax` returns a lossless syntax tree, parse status, and diagnostics.

Syntax values preserve source text, token structure, trivia, and diagnostics. Construction and detachment produce immutable syntax values.

All host-operation argument/reply types are frozen ABI definitions. Operations may add ordinary error arms compatibly only through an ABI version change reflected in identity hashes.

---

## 24. Core image, prelude, and minimal standard library

The distribution must be useful for real command-line programs without turning the prelude into a second language. Core identity, convenience names, pure algorithms, and host integration are separate artifacts.

### 24.1 Core image

The core image is dependency-free and implicitly linked. Its source definitions are compiled and hash-pinned as part of the ABI. It contains:

```lm
enum Option[T]
  Some(v: T)
  None
end

enum Result[T, E]
  Ok(v: T)
  Err(error: E)
end

enum Ordering
  Less
  Equal
  Greater
end

final class Tuple2[A, B]
  def swap(self): (B, A)
    (self[1], self[0])
  end
end

class Range
  start: Int
  stop: Int
  step: Int

  def init(mut self, start: Int, stop: Int, step: Int)
    assert_message(step != 0, "Range step must not be zero")
    self.start = start
    self.stop = stop
    self.step = step
  end
end
```

It also contains VM, proc, snapshot, filesystem, and network boundary values.

These values include portable errors, addresses, TCP helpers, and native TCP and TLS resource classes.

`std.tls` contains TLS configuration values and client helpers.

`std.http` contains bounded HTTP/1.1 values, codecs, and client helpers.

`List`, `Map`, `Text`, its concrete classes, `Char`, and `Bytes` are native core classes in the pinned image.

Core also defines `Set`, `Display`, `PartialEq`, `Hashable`, `Comparable`, `Copyable`, and `Error`.

Conditional conformances give these protocols to eligible collections and algebraic values.

Builders, type descriptors, faults, VMs, snapshots, procs, file leases, and resource handles are also native core classes.

The image seals their complete method tables. Some bodies use intrinsics, while other bodies use ordinary verified bytecode.

### 24.2 Prelude

The prelude introduces the pinned value and resource surface:

```text
(), Never, Bool, Int, Float, Byte, List, Map
Option, Some, None, Result, Ok, Err, Ordering, Unit, Tuple2, ..., Tuple16, Range
StepEvent, DriveEvent, Proc, Recv, SendResult, ProcError
Choice, SnapshotError, RestoreError, BranchError, FsError, OpenOptions, SeekFrom
FileKind, FileInfo, DirEntry, RenameMode
IpAddress, SocketAddress, NetError, TcpRead, Shutdown
TcpResource, TcpStream, TcpListener, Tcp
TlsError, TlsStream
Text, String, Substring, Char, Utf8Error, IndexError, HexError, ParseIntError, ParseFloatError, FloatToIntError, Bytes
StringBuilder, ByteBuffer
Display, PartialEq, Hashable, Comparable, Copyable, Add, Error
Iterator, Iterable, Counted, RandomAccess, ByteReader, ByteWriter
identity, display, hash_of, hash_combine, assert, assert_message
```

`Any` remains an explicit primitive type.

The prelude binds type names and constructors. It grants no operation.

The prelude contains no function that performs I/O.

`identity`, `assert`, and `assert_message` are pinned pure/native core functions; the prelude only imports their names. `assert` and `assert_message` deterministically fault the current machine when their condition is false.

### 24.3 Option and Result methods

Because the core enum method tables are sealed, the common combinators ship with the core image:

```text
Option[T]
  is_some() -> Bool
  is_none() -> Bool
  value_or(default: T) -> T
  map[U,e]((T) -> U with e) -> Option[U] with e
  and_then[U,e]((T) -> Option[U] with e) -> Option[U] with e
  to_result[E](error: E) -> Result[T,E]

Result[T,E]
  is_ok() -> Bool
  is_err() -> Bool
  value_or(default: T) -> T
  map[U,e]((T) -> U with e) -> Result[U,E] with e
  map_error[F,e]((E) -> F with e) -> Result[T,F] with e
  and_then[U,e]((T) -> Result[U,E] with e) -> Result[U,E] with e
  option() -> Option[T]

Result[T,Fault]
  value() -> T
```

Postfix `?` propagates a `Result` error from the nearest callable.

For `value: Result[T,E]`, `value?` evaluates `value` once. It produces the `Ok` payload or returns `Err(error)`.

The enclosing callable must return `Result[U,E]`. The error types must be equal.

The top level cannot use `?`. A closure that uses `?` must declare its `Result` type.

`Result.map`, `map_error`, and `and_then` support explicit error conversion and staged pipelines.

`Result.value()` returns the success value or raises the stored fault.

`raise(fault: Fault): Never` raises an existing fault without replacing its trace.

### 24.4 Random access and native `List[T]`

`RandomAccess` extends `Counted` and `Iterable` with indexed reads.

It provides binary-search defaults for ordered collections.

```text
RandomAccess
  at(self, index: Int) -> Self.Item
  get(self, index: Int) -> Option[Self.Item]
  partition_point[e](self, predicate: (Self.Item) -> Bool with e) -> Int with e
  lower_bound(self, value: Self.Item) -> Int when Self.Item: Comparable
  upper_bound(self, value: Self.Item) -> Int when Self.Item: Comparable
  binary_search(self, value: Self.Item) -> Option[Int] when Self.Item: Comparable
  lower_bound_by[e](self, compare: (Self.Item) -> Ordering with e) -> Int with e
  upper_bound_by[e](self, compare: (Self.Item) -> Ordering with e) -> Int with e
  binary_search_by[e](self, compare: (Self.Item) -> Ordering with e) -> Option[Int] with e
```

The bound methods expect ascending input.

The comparator reports each input element's order relative to the search value.

`lower_bound` returns the first index that is not less than the value.

`upper_bound` returns the first index that is greater than the value.

`partition_point` returns the first index where its predicate is false.

`List[T]` and `ListSlice[T]` implement `RandomAccess`.

List literals have type `List[T]`; `[T]` is canonical type sugar. The minimum sealed method surface is:

```text
List[T]() -> List[T]
list_with_capacity[T](capacity: Int) -> List[T]
list_repeated[T](value: T, count: Int) -> List[T]

len(self) -> Int
capacity(self) -> Int
is_empty(self) -> Bool
get(self, index: Int) -> Option[T]
at(self, index: Int) -> T
first(self) -> Option[T]
last(self) -> Option[T]
set(mut self, index: Int, value: T) -> ()
push(mut self, value: T) -> ()
pop(mut self) -> Option[T]
insert(mut self, index: Int, value: T) -> ()
remove(mut self, index: Int) -> T
swap_remove(mut self, index: Int) -> T
swap(mut self, first: Int, second: Int) -> ()
reserve(mut self, additional: Int) -> ()
truncate(mut self, length: Int) -> ()
clear(mut self) -> ()
copy(self) -> List[T]
slice(self, start: Int, length: Int) -> List[T]
slice_view(self, start: Int, length: Int) -> ListSlice[T]
concat(self, other: List[T]) -> List[T]
extend(mut self, other: List[T]) -> ()
reverse_range(mut self, start: Int, length: Int) -> ()
reverse(mut self) -> ()
retain[e](mut self, keep: (T) -> Bool with e) -> () with e
dedup_adjacent(mut self) -> () when T: PartialEq
windows(self, size: Int) -> List[ListSlice[T]]
flatten(self) -> List[T.Item] when T: Iterable
contains(self, value: T) -> Bool
position[e](self, predicate: (T) -> Bool with e) -> Option[Int] with e
find[e](self, predicate: (T) -> Bool with e) -> Option[T] with e
each[e](self, f: (T) -> () with e) -> () with e
map[U,e](self, f: (T) -> U with e) -> List[U] with e
filter[e](self, f: (T) -> Bool with e) -> List[T] with e
filter_map[U,e](self, f: (T) -> Option[U] with e) -> List[U] with e
fold[U,e](self, initial: U, f: (U,T) -> U with e) -> U with e
any[e](self, f: (T) -> Bool with e) -> Bool with e
all[e](self, f: (T) -> Bool with e) -> Bool with e
sort_by[e](mut self, compare: (T,T) -> Ordering with e) -> () with e
sort(mut self) -> () when T: Comparable
min(self) -> Option[T] when T: Comparable
max(self) -> Option[T] when T: Comparable
freeze(self) -> List[T]
```

Faulting index methods use `IndexOutOfBounds`; allocation failure obeys heap limits. Higher-order methods call the closure in list order and stop immediately on fault.

`swap` exchanges two valid positions. Equal positions do not change the list.

`reverse_range` reverses one valid half-open range. `reverse` applies it to the complete list.

`retain` preserves the order of values accepted by its predicate.

`dedup_adjacent` preserves the first value from each adjacent equal run.

`windows` requires a positive size. It returns overlapping views in source order.

A size greater than the list length gives an empty list.

`flatten` concatenates each nested iterable in source order.

`ListSlice` shares its source list. Structural source changes make later slice operations fault with `CollectionModified`.

`List[T]` conditionally implements `Display`, `PartialEq`, `Hashable`, and `Comparable`.

It implements `Copyable` for every element type.

`Iterable` provides eager defaults from one required `iterator()` method.

The defaults include mapping, filtering, folding, queries, indexed operations, slicing, chunks, joining, and parallel mapping.

They also include `zip`, `flat_map`, and `unique`.

`zip` stops when either input ends. `flat_map` concatenates each mapped iterable.

`unique` preserves the first occurrence of each value. Its item type must implement `Hashable`.

`par_map` uses pure escaping callbacks and the `Proc` effect.

It returns values in source order and raises child faults in source chunk order.

### 24.5 Maps and sets

`Map[K,V]` requires `K: Hashable` and preserves insertion order:

```text
Map[K,V]() -> Map[K,V]
map_with_capacity[K,V](capacity: Int) -> Map[K,V]
len / is_empty / has
get(key: K) -> Option[V]
at(key: K) -> V
put(mut self, key: K, value: V) -> Option[V]
get_or_insert_with[e](mut self, key: K, f: () -> V with e) -> V with e
remove(mut self, key: K) -> Option[V]
clear(mut self) -> ()
copy(self) -> Map[K,V]
keys(self) -> MapKeys[K,V]
values(self) -> MapValues[K,V]
entries(self) -> MapEntries[K,V]
keys_list(self) -> List[K]
values_list(self) -> List[V]
entries_list(self) -> List[(K,V)]
each[e](self, f: (K,V) -> () with e) -> () with e
map_values[U,e](self, f: (K,V) -> U with e) -> Map[K,U] with e
retain[e](mut self, f: (K,V) -> Bool with e) -> () with e
freeze(self) -> Map[K,V]
```

Map equality ignores insertion order.

Map hashing also ignores insertion order.

`Map[K,V]` implements `Display` when both type arguments implement `Display`.

It implements `PartialEq` and `Hashable` when `V` implements each protocol.

It implements `Copyable` for every value type.

For a text key type, `has`, `get`, `at`, and indexing accept Text.

`Map[String,V].put` accepts Text. It creates one bounded String only for a missing key.

Other map insertions require K.

Core defines `Set[T: Hashable]` as an ordinary final class over `Map[T,()]`.

It preserves first insertion order.

It provides `add`, `remove`, `has`, `clear`, `reserve`, `copy`, `values`, `each`, and `retain`.

It also provides `union`, `intersection`, `difference`, `is_subset`, `is_superset`, and `is_disjoint`.

Set equality ignores insertion order.

Set hashing ignores insertion order.

`Set[T]` implements `Display` when `T` implements `Display`.

It implements `PartialEq`, `Hashable`, and `Copyable` for every valid element type.

The core surface follows.

```text
Set[T]() -> Set[T]
set_from_list[T](values: List[T]) -> Set[T]
len(self) -> Int
is_empty(self) -> Bool
has(self, value: T) -> Bool
add(mut self, value: T) -> Bool
remove(mut self, value: T) -> Bool
clear(mut self) -> ()
reserve(mut self, additional: Int) -> ()
copy(self) -> Set[T]
values(self) -> List[T]
each[e](self, f: (T) -> () with e) -> () with e
add_all(mut self, values: List[T]) -> ()
retain[e](mut self, f: (T) -> Bool with e) -> () with e
union(self, other: Set[T]) -> Set[T]
intersection(self, other: Set[T]) -> Set[T]
difference(self, other: Set[T]) -> Set[T]
is_subset(self, other: Set[T]) -> Bool
is_superset(self, other: Set[T]) -> Bool
is_disjoint(self, other: Set[T]) -> Bool
```

A deque is not part of the core image.

### 24.6 Strings, bytes, builders, and formatting

`Text` is a sealed abstract core class. `String` and `Substring` are its only concrete classes.

`String` and `Substring` are final. Programs cannot construct `Text`, `Substring`, or `Char` with an ordinary class call.

String literals and builders produce String values. Text slices and UTF-8 byte views produce Substring values.

Char is a final core class with an immediate VM representation. A Char payload contains one Unicode scalar value.

`Text.len` counts Unicode scalar values. `Text.byte_len` counts UTF-8 bytes.

Scalar positions are the default text positions. Explicit byte methods use byte positions.

The common Text surface follows.

```text
len() -> Int
byte_len() -> Int
to_string() -> String
is_empty() -> Bool
at(index: Int) -> Option[Char]
slice(start: Int, length: Int) -> Result[Substring,IndexError]
slice_bytes(start: Int, length: Int) -> Result[Substring,Utf8Error]
find(needle: Text) -> Option[Int]                 # scalar position
find_bytes(needle: Text) -> Option[Int]           # byte position
each[e](f: (Char) -> () with e) -> () with e
map[e](f: (Char) -> Char with e) -> String with e
starts_with(prefix: Text) -> Bool
ends_with(suffix: Text) -> Bool
contains(needle: Text) -> Bool
bytes() -> Bytes
split(separator: Text) -> List[Substring]
split_once(separator: Text) -> Option[(Substring, Substring)]
lines() -> List[Substring]
trim() -> Substring
trim_start() -> Substring
trim_end() -> Substring
pad_start(width: Int) -> String
pad_end(width: Int) -> String
strip_prefix(prefix: Text) -> Option[Substring]
strip_suffix(suffix: Text) -> Option[Substring]
to_lower_ascii() -> String
to_upper_ascii() -> String
replace(needle: Text, replacement: Text) -> String
parse_int(radix: Int) -> Result[Int,ParseIntError]
parse_float() -> Result[Float,ParseFloatError]
__eq__(other: Text) -> Bool
__lt__(other: Text) -> Bool
__le__(other: Text) -> Bool
__gt__(other: Text) -> Bool
__ge__(other: Text) -> Bool
```

`at`, `slice`, `find`, `each`, and `map` use Unicode scalar positions. `at` returns None for an invalid position.

`slice` reports `IndexError.OutOfBounds` for an invalid scalar range. A successful slice shares storage.

`slice_bytes` reports `Utf8Error.OutOfBounds` for an invalid range. It reports `Utf8Error.InvalidBoundary` when a boundary splits one scalar.

`find_bytes` supports byte-oriented parsers. It avoids the scalar-position conversion that `find` requires.

One rule sets the result type of every extraction method. A method that narrows its receiver gives a `Substring` and copies nothing. A method that builds new content gives a `String`. So `split`, `lines`, `trim`, and the two `strip_` methods give views. Case conversion, replacement, and padding give durable values.

Every method above is total, under the rule of section 12.1. `split` with an empty separator matches at every scalar boundary and gives one empty piece at each end. `replace` with an empty needle inserts at every scalar boundary. `parse_int` reports `ParseIntError.BadRadix` for a radix outside 2 to 36.

Padding widths count Unicode scalar values. A width below the current length adds no spaces.

Padding adds U+0020 SPACE characters.

`parse_float` accepts decimal text, `NaN`, `inf`, `+inf`, and `-inf`.

A decimal has an optional sign, digits, an optional point, and an optional exponent.

At least one digit must appear before or after the point.

An exponent starts with `e` or `E`. It has an optional sign and at least one digit.

Parsing accepts no whitespace or underscore separators.

Parsing rounds a finite decimal to the nearest binary64 value. A tie selects the value with an even significand.

It reports `ParseFloatError.Invalid` for other text.

A finite decimal that exceeds binary64 reports `ParseFloatError.Overflow`.

`lines` accepts a line feed with or without a leading carriage return. A final line feed ends the last line and adds no empty piece.

`split_once`, `strip_prefix`, and `strip_suffix` give a valid piece by construction, so they report absence through `Option` and never report a boundary error. A parser that uses them handles no failure that its own input cannot cause.

Interpolation accepts any `Text`. A `Substring` appends to the builder without a copy.

The implementation uses one lazy sparse scalar index for each text root. It records every 64th scalar position.

The first indexed operation can build this index in O(n) time. A scalar boundary lookup scans at most 63 scalars.

`each` is the primary scalar traversal operation. `map` transforms each scalar and returns a new String.

Both methods decode each scalar once. They use a forward UTF-8 byte cursor.

Text ordering compares Unicode scalar values lexicographically. Text equality compares visible scalar sequences without normalization.

Loom performs no automatic Unicode normalization. A library can provide normalization and grapheme-cluster operations.

Text adds these methods.

```text
concat(other: Text) -> String
__add__(other: Text) -> String
```

`Text + Text` produces a bounded String. Concatenation creates new storage.

Substring adds these methods.

```text
compact() -> String
```

Both methods enforce the String retention bound. They can return shared storage when that storage already meets the bound.

Char has this surface.

```text
codepoint() -> Int
utf8_len() -> Int
is_ascii() -> Bool
__eq__(other: Char) -> Bool
__lt__(other: Char) -> Bool
__le__(other: Char) -> Bool
__gt__(other: Char) -> Bool
__ge__(other: Char) -> Bool
```

`Text.at` allocates no Char object. Its successful path allocates only the `Option.Some` result object.

Core defines `Utf8Error`, `IndexError`, `HexError`, `ParseIntError`, and `ParseFloatError`.

The core Bytes surface follows.

```text
len() -> Int
is_empty() -> Bool
at(index: Int) -> Int
get(index: Int) -> Option[Int]
slice(start: Int, length: Int) -> Result[Bytes,IndexError]
compact() -> Bytes
concat(other: Bytes) -> Bytes
starts_with(prefix: Bytes) -> Bool
find(needle: Bytes) -> Option[Int]
hex() -> String
Bytes.from_hex(text: Text) -> Result[Bytes,HexError]
utf8() -> Result[String,Utf8Error]
utf8_view() -> Result[Substring,Utf8Error]
text() -> String
text_range(start: Int, length: Int) -> String
intern_text_range(pool: Map[String,String], start: Int, length: Int) -> String
__add__(other: Bytes) -> Bytes
__and__(other: Bytes) -> Bytes
__or__(other: Bytes) -> Bytes
__xor__(other: Bytes) -> Bytes
__invert__() -> Bytes
__eq__(other: Bytes) -> Bool
__lt__(other: Bytes) -> Bool
__le__(other: Bytes) -> Bool
__gt__(other: Bytes) -> Bool
__ge__(other: Bytes) -> Bool
```

`at` faults with `IndexOutOfBounds` for an invalid index. `get` returns `None` for an invalid index.

`slice` returns `Err(IndexError.OutOfBounds)` for an invalid range. A successful slice shares immutable storage.

`compact` copies the visible bytes into a new allocation. Use it to release a large retained allocation.

`find` returns a byte offset. `hex` uses lowercase hexadecimal text.

`Bytes.from_hex` accepts uppercase and lowercase digits.

It returns `HexError.OddLength` or `HexError.InvalidDigit(index)` for invalid text.

`utf8` reports invalid encoding through its result. It returns a bounded String.

`utf8_view` reports invalid encoding through its result. It returns a shared Substring without a content copy.

`text` is a compatibility conversion that faults with `BadCast`. It returns a bounded String after successful validation.

`text_range` validates one byte range and its UTF-8 encoding.

It faults with `IndexOutOfBounds` for an invalid range.

It faults with `BadCast` for invalid UTF-8.

It creates one bounded String without an intermediate Bytes object.

`intern_text_range` implements the byte-range entry in the closed `BorrowedKey` relation.

It probes an owned String pool with one validated UTF-8 byte range.

A hit returns the stored String without a guest allocation.

A miss creates one bounded String and stores it as both key and value.

`+`, the bitwise operators, equality, and ordering use the paired-underscore `Bytes` methods.

The binary bitwise methods require equal lengths.

They fault with `LengthMismatch` when the lengths differ.

`!=` negates the `PartialEq` result.

The ordering methods use unsigned byte order.

The final nominal builders have the following surface.

```text
StringBuilder.append(text: Text) -> StringBuilder
StringBuilder.append_int(value: Int) -> StringBuilder
StringBuilder.append_float(value: Float) -> StringBuilder
StringBuilder.append_bool(value: Bool) -> StringBuilder
StringBuilder.push_char(value: Char) -> StringBuilder
StringBuilder.len() -> Int
StringBuilder.byte_len() -> Int
StringBuilder.clear() -> StringBuilder
StringBuilder.build() -> String
StringBuilder.finish() -> String

ByteBuffer.append(byte: Int) -> ByteBuffer
ByteBuffer.extend(bytes: Bytes) -> ByteBuffer
ByteBuffer.reserve(additional: Int) -> ByteBuffer
ByteBuffer.set(index: Int, byte: Int) -> ByteBuffer
ByteBuffer.capacity() -> Int
ByteBuffer.truncate(length: Int) -> ByteBuffer
ByteBuffer.clear() -> ByteBuffer
ByteBuffer.len() -> Int
ByteBuffer.build() -> Bytes
ByteBuffer.finish() -> Bytes
```

The builders use ordinary class types in bytecode and module interfaces. Native payload tags implement their storage.

`StringBuilder.len` counts Unicode scalar values. `StringBuilder.byte_len` counts UTF-8 bytes.

`build` copies the current content and leaves the builder active. Later writes do not change an earlier result.

`ByteBuffer.finish` transfers the private buffer into the result.

`StringBuilder.finish` transfers the buffer when its retained capacity meets the String bound. It otherwise compacts the result.

Both methods then invalidate the builder.

Any operation on a finished builder faults with `InvalidVmState`.

`ByteBuffer.set` requires a valid index and a byte from 0 through 255.

An invalid index faults with `IndexOutOfBounds`. An invalid byte faults with `IntegerOverflow`.

`ByteBuffer.truncate` rejects a negative length. A length above the current length has no effect.

File and network operations exchange Bytes. An in-process host boundary can share immutable Bytes storage.

`ByteBuffer.build` and `ByteBuffer.finish` never perform a text conversion.

Interpolation calls `Display.append_to` on one shared builder.

Core values provide pinned formatting implementations.

User classes format through an explicit `Display` conformance.

#### 24.6.1 Regular expressions

`Regex` is a final core class with an immutable compiled representation.

A `re"..."` literal is checked during compilation and compiled once for each loaded code namespace.

The syntax supports concatenation, alternation, repetition, character classes, captures, anchors, and Unicode properties.

The syntax does not support backreferences, look-around assertions, or conditional expressions.

Matching uses Unicode UTF-8 mode and leftmost-first choice.

All reported positions are half-open UTF-8 byte offsets.

Empty matches occur only at UTF-8 boundaries.

The implementation uses finite automata and does not use recursive backtracking during a search.

A pattern contains at most 65,536 UTF-8 bytes, 64 syntax levels, and 128 capture slots.

The capture limit includes the complete match.

Compiled automata and lazy caches have fixed memory limits.

A verified module retains at most 64 MiB of compiled literal data.

The compiler rejects an invalid or excessive literal.

Dynamic compilation reports the same conditions through `RegexError`.

The core surface follows.

```text
Regex.compile(pattern: Text) -> Result[Regex, RegexError]
source(self) -> String
is_match(self, text: Text) -> Bool
find(self, text: Text) -> Option[RegexMatch]
captures(self, text: Text) -> Option[RegexMatch]
count(self, text: Text) -> Int
split(self, text: Text) -> List[Substring]
replace_all(self, text: Text, replacement: Text) -> String

start_byte(self) -> Int
end_byte(self) -> Int
text(self) -> String
group_count(self) -> Int
group(self, index: Int) -> Option[Substring]
named(self, name: Text) -> Option[Substring]
```

`find` and `captures` return the first match with its captures.

Capture index zero contains the complete match.

`group_count` includes capture index zero.

An absent optional capture gives `None`.

`split` returns shared text views between non-overlapping matches.

Replacement text accepts `$1`, `$name`, `${name}`, and `$$` references.

An absent or unknown capture contributes empty text.

A replacement contains at most 4,096 literal and capture parts.

`replace_all` faults with `HeapLimit` when its bounded result cannot fit.

`RegexError.Invalid` reports invalid syntax.

`RegexError.LimitExceeded` reports a fixed compilation limit.

### 24.7 Numeric and range utilities

The core `Int` surface adds these explicit operations:

```text
bit_and(other: Int) -> Int
bit_or(other: Int) -> Int
bit_xor(other: Int) -> Int
bit_not() -> Int
shl(amount: Int) -> Int
shr(amount: Int) -> Int
ushr(amount: Int) -> Int
wrapping_add(other: Int) -> Int
wrapping_sub(other: Int) -> Int
wrapping_mul(other: Int) -> Int
rotate_left(amount: Int) -> Int
rotate_right(amount: Int) -> Int
count_ones() -> Int
leading_zeros() -> Int
trailing_zeros() -> Int
signum() -> Int
to_float() -> Float
```

The core `Float` surface adds these explicit operations:

```text
is_nan() -> Bool
is_finite() -> Bool
is_infinite() -> Bool
abs() -> Float
min(other: Float) -> Float
max(other: Float) -> Float
sqrt() -> Float
floor() -> Float
ceil() -> Float
round() -> Float
trunc() -> Float
copy_sign(sign: Float) -> Float
mul_add(multiplier: Float, addend: Float) -> Float
pow(exponent: Float) -> Float
exp() -> Float
exp2() -> Float
exp_m1() -> Float
ln() -> Float
log2() -> Float
log10() -> Float
ln_1p() -> Float
cbrt() -> Float
hypot(other: Float) -> Float
sin() -> Float
cos() -> Float
tan() -> Float
asin() -> Float
acos() -> Float
atan() -> Float
atan2(other: Float) -> Float
sinh() -> Float
cosh() -> Float
tanh() -> Float
asinh() -> Float
acosh() -> Float
atanh() -> Float
bits() -> Int
to_int() -> Result[Int,FloatToIntError]
fixed(digits: Int) -> String
Float.from_bits(bits: Int) -> Float
```

`Float` implements `Display`, `Hashable`, and `SignedNumber`.

Display uses the shortest decimal text that round-trips through binary64 parsing.

Float equality treats all NaNs as equal. It also treats both signed zeros as equal.

Float hashing follows those equality rules.

Ordered operators use IEEE ordered comparisons. They return false when either value is NaN.

`compare` defines a total order for collections.

It treats both zeros as equal and places NaN after every number.

`fixed` writes exactly `digits` decimal places.

It rounds the binary64 value to the nearest decimal result. A tie selects an even final digit.

NaN and infinities use their `Display` text without decimal places.

A negative `digits` value faults with `InvalidPrecision`.

`Int` and `Float` provide the numeric methods in section 6.4.

`Range(start, stop, step)` rejects zero step. `Range.each`, `to_list`, `contains`, and `len` use checked arithmetic. A `for` expression traverses a range directly.

### 24.8 Value utilities

```lm
freeze[T](value: T): T
digest[T](value: T): Digest
is_frozen[T](value: T): Bool
deep_equal[T](a: T, b: T): Bool
```

`deep_equal` requires frozen digestible graphs.

It uses digests as a fast reject, then performs cycle-safe structural comparison.

Digest equality alone is not proof.

### 24.9 Paths, I/O, and files

Core defines `Path` and `PathStyle` because filesystem operations use their nominal identity.

A `Path` stores exact text and either POSIX or Windows syntax.

The default host accepts only its native style.

A custom filesystem driver defines its accepted styles.

The `Fs` grant supplies authority. A `Path` supplies no authority.

`std/io` contains thin wrappers:

```lm
write_all(bytes: Bytes): Result[(), IoError] with Io.Write
write_error_all(bytes: Bytes): Result[(), IoError] with Io.WriteError
print(text: Text): Result[(), IoError] with Io.Write
print_error(text: Text): Result[(), IoError] with Io.WriteError
read_to_end(max_bytes: Int): Result[Bytes, IoError] with Io.ReadBytes
```

`std/fs` provides scoped and durable file helpers:

```lm
with_file(path, options, body)
read_dir_sorted(path, max_entries)
write_file_all(file, bytes)
durable_replace(directory, temporary_path, target_path, bytes)
```

`with_file` always closes a successfully opened handle.

It returns the body error before a later close error.

A body fault terminates the machine normally.

The host resource registry closes the handle during VM cleanup.

Cleanup invokes no guest callback and preserves the first fault.

`FileHandle` has explicit read, write, seek, flush, sync, and close methods.

A live handle blocks snapshot creation.

A closed handle remains machine state and returns `FsError.Closed`.

`read_dir_sorted` sorts valid UTF-8 names.

It keeps every invalid entry as an inner error.

`durable_replace` writes, flushes, syncs, renames, and syncs the parent directory.

There are no finalizers. Scoped cleanup is host-managed. Raw handle ownership remains explicit.

### 24.10 Time, randomness, networking, and process inputs

`std.time` defines frozen `Duration`, `Timestamp`, and `Instant` values with nanosecond precision.

`Duration` stores a signed count. Unit constructors reject conversion overflow.

`Timestamp` stores UTC nanoseconds from the Unix epoch. `now` performs `Clock.Now`.

`Instant` stores a reading from one monotonic effect provider. `monotonic` performs `Clock.Monotonic`.

Programs compare instant readings from the same provider. `elapsed_since` rejects reversed readings.

`sleep` performs `Clock.Sleep`. It rejects a negative duration before it performs the operation.

`Date`, `TimeOfDay`, `UtcOffset`, and `DateTime` use the proleptic Gregorian calendar.

Calendar years range from 1 through 9999. UTC offsets have minute precision and cannot exceed 23:59.

`parse_rfc3339` accepts uppercase or lowercase time and UTC markers. It accepts one through nine fractional digits.

The parser rejects leap seconds. `format_rfc3339` emits the shortest exact fractional part and preserves the stored offset.

`std.random.Random` uses SplitMix64. A seed selects one portable sequence.

Its integer sampling uses rejection. It does not use biased remainder reduction.

`std.random` also provides host-backed integer ranges, Boolean selection, list `choose`, and Fisher-Yates `shuffle`.

Host-backed selection uses the exact `Rand.Int` row. Secure bytes and entropy seeding use `Entropy.Bytes`.

`std.path` provides lexical path operations with explicit POSIX or Windows rules.

Filesystem access preserves the supplied spelling.

Call `normalize` explicitly to remove `.` segments and resolve possible `..` segments.

Normalization does not access a filesystem.

Path normalization is not an authorization check. Filesystem policy must validate the resolved host resource.

`std.url` parses RFC 3986 URI references and absolute URLs. It preserves encoded components.

URL resolution removes dot segments according to RFC 3986. Percent decoding has separate byte and UTF-8 forms.

Core network code defines DNS, TCP, and native TLS stream operations.

`std.tls` wraps TLS configuration and client operations.

`std.http` implements bounded HTTP/1.1 messages and direct clients.

`Http.send_url` accepts an absolute HTTP or HTTPS `Url`.

It derives the host, port, request target, and default TLS server name.

`std.term` contains pure terminal control bytes and bounded key decoding.

`decode_key` consumes one complete terminal input sequence.

It scans CSI parameter, intermediate, and final bytes according to ECMA-48.

An unsupported complete sequence becomes `Unknown` and consumes the complete sequence.

An incomplete CSI, SS3, or alternate scalar becomes `NeedMore` until the caller finishes the escape.

`Ctrl` stores a lowercase character for a control letter.

`Alt` stores one printable Unicode scalar.

Byte `0x08` and byte `0x7f` remain `Backspace`.

Terminal input, output, timers, and size queries retain their exact operation rows.

A live TCP or TLS handle is a host attachment and blocks snapshot creation.

Cleartext HTTP uses `Http.CleartextClient`.

Secure HTTP uses `Http.Client` and an explicit `TlsClientConfig`.

Proxy policy, redirects, cookies, decompression, and connection pools remain separate code.

`Args.Get` returns command-line arguments through the `sys.args()` surface. `Env.Get` reads one environment value. `Fs.CurrentDir` reads the current directory.

### 24.11 Base64

`std/base64` provides the standard padded RFC 4648 alphabet.

```lm
encode(bytes: Bytes) -> String
decode(text: Text) -> Result[Bytes,Base64Error]
```

`Base64Error` contains `InvalidLength`, `InvalidByte(index)`, and `InvalidPadding`.

The decoder rejects whitespace, missing padding, invalid bytes, and noncanonical unused bits.

The module uses ordinary Loom code over `Bytes` and integer bit operators.

### 24.12 JSON

The distribution includes a small `std/json` module.

It makes file and network examples practical without new runtime machinery.

```lm
enum Json
  Null
  Boolean(value: Bool)
  Number(value: Float)
  Text(value: String)
  ListValue(value: List[Json])
  Object(value: Map[String, Json])
end
```

`JsonError` contains `Invalid(offset,message)`, `LimitExceeded(message)`, and `NonFiniteNumber`.

`parse` returns `Result[Json,JsonError]`.

`stringify` is pure and deterministic.

Parsing checks byte, item, and depth limits before recursive work.

Input contains at most 16,777,216 bytes.

Nesting depth is at most 128.

One value contains at most 1,000,000 parsed items.

Objects preserve insertion order.

A duplicate object key replaces its earlier value.

Stringification rejects non-finite numbers.

JSON uses ordinary Loom code over `String`, `List`, and `Map`.

### 24.13 Typed VM utilities

The standard library does not reintroduce an `Answer(Any)` decision enum or a variadic helper that would require type packs. Exact-operation elimination is already ordinary and small enough to package in user code:

```lm
def answer_write[T](
  vm: Run[T],
  request: Request,
  mut captured: [Bytes]
): Bool with Vm.Answer
  case request
  in Call(Io.Write, call, (bytes,))
    captured.push(bytes)
    vm.answer(call, Ok(bytes.len()))
    true
  in _
    false
  end
end
```

A policy can define one such function for each operation that it owns.

The ordinary `Call` pattern rule type-checks this function.

This rule adds no variadic generics, tuple spreading, or dependent native rule.

`std/vm` provides fuel builders, limit builders, terminal-result mapping, and snapshot file helpers.

### 24.14 Procs

`std/proc` supplies explicit supervision, bounded send loops, close/drain, cancellation-message conventions, and result aggregation. It does not add shared memory or hide proc effects. `Handle[M,R]` preserves message and result types through `send`, `done`, `pause`, `resume`, transfer, and snapshot restore.

### 24.15 Compiler, reflection, and testing

`std/compiler.compile(source)` can supply an empty `CompileEnv` and default options.

Installation helpers can build `LinkEnv` values from module instances. Dynamic tools must use `DynValue` explicitly.

`std/reflect` formats syntax trees and diagnostics. Version 0.2 has no general value mirror or dynamic invocation.

`std/test` represents each test body as a frozen descriptor. The descriptor carries its function type, row, code hash, and captures.

The runner executes each case in a child VM. It configures an explicit table and records `Done` or `Fault`.

The runner can use `drive` for deterministic operation transcripts.

The compiler test harness has UI diagnostics, compile-pass, run-pass, run-fail, bytecode-verifier, artifact/snapshot corruption, conformance, fuzz-regression, and benchmark suites.

### 24.16 Deliberate omissions

The minimal library does not include an iterator trait hierarchy, async/await, database client, GUI, or locale framework.

It also omits HTTP/2, HTTP/3, TLS servers, automatic redirects, cookies, proxies, and decompression.

Richer facilities remain ordinary packages over explicit effects.

## 25. Host and intrinsic ABI

### 25.1 Canonical manifests

A release ships four canonical manifests:

```text
core.abi         primitive/native declarations and pinned core-image hashes
operations.abi   groups, operation identities, signatures, ordinary errors
intrinsics.abi   intrinsic identities, signatures, purity, fuel formulas
faults.abi       stable fault codes and diagnostic fields
```

Each manifest has a canonical byte encoding and digest. Artifact and snapshot headers name compatible ABI digests. Changing an operation signature, group membership, intrinsic semantics, or stable fault meaning requires a new ABI version and therefore new identities where behavior changes.

### 25.2 Operation identity

An exact operation identity is the domain-separated hash of ABI version, group name, member name, complete parameter/result type encoding, and semantic revision. The hash is the portable identity; loaded runtimes resolve it to a dense operation slot.

The callable `sys` object and non-callable descriptor constants are generated from the same manifest, preventing disagreement between rows, table entries, and host dispatch.

### 25.3 Host implementation contract

A host implementation receives:

- root/parent binding and exact operation slot;
- arguments validated according to each manifest parameter mode (`value`, `transfer`, `designator`, `inspect`, or `control`);
- controlled VM identity and cancellation context;
- a single-use completion sink typed to the declared reply.

It may complete synchronously or asynchronously. It must invoke the sink at most once. A successful reply is validated according to the manifest result mode before installation. Host failure outside the declared ordinary result becomes `HostFault`.

The host never receives arbitrary writable pointers into the guest heap. An `inspect` parameter is exposed only through a bounded read-only traversal interface whose lifetime ends with the host call/completion handoff. Zero-copy optimizations may borrow immutable backing storage only for the duration defined by the ABI and cannot make sharing observable.

### 25.4 Pure intrinsic contract

An intrinsic is deterministic, has the empty row, cannot suspend, and cannot call host operations. Its manifest specifies exact signature, semantic revision, and fuel formula. It receives checked guest values and may allocate only through the controlled VM heap under limits.

Native collection, text, byte, builder, graph, numeric, and type-test operations use intrinsics or kernel instructions.

They are not host operations. Their faults are deterministic language faults.

### 25.5 Native classes, graph shapes, and resource registry

Every native heap class registers one immutable shape descriptor describing traced references, frozen-write locations, canonical field order, snapshot classification, snapshot encoding, boundary policy, digestibility, and cleanup behavior. The snapshot classification is machine state or host attachment (16.4). A native class that cannot participate consistently must be holder-local or a host attachment; it cannot masquerade as an ordinary sendable object.

No live completion callback enters snapshot bytes. A pending suspending operation is a host attachment and blocks snapshot creation.

Each VM has a host-side resource registry outside the guest heap. It records resource kind, owning VM, scope identity, pending operation ordinal, and cleanup state. Snapshot preflight reads the registry and the guest graph. A pending operation can retain live external state after its guest wrapper becomes unreachable.

VM termination closes registered scoped resources. It invokes no guest callback. Cleanup failure does not replace an existing machine fault.

### 25.6 ABI initialization

Before loading guest code, the host verifies its compiled-in/generated tables against manifest digests and assigns dense slots. Duplicate hashes, signature mismatches, or missing required core operations/intrinsics abort host initialization rather than producing a partially compatible runtime.

---

## 26. Packages, build system, and command line

### 26.1 Package model

A package root is one source module. Supporting modules compile independently and use explicit compile and link bindings. Source modules use `use`. Dependency edges live in the manifest and artifact import slots.

A minimal `lm.package` is deterministic data:

```text
name = "hello"
root = "src/main.lm"
source = ["src/**/*.lm"]
dependencies = { "text" = { path = "../text" } }
```

The exact manifest syntax is a tooling format, not guest-language syntax. Dependency names map to explicit compile-environment values. Cyclic package dependencies are rejected, even though definitions inside one module may be mutually recursive.

### 26.2 Build keys and cache

A compile cache key covers source bytes, compiler semantic hash/version, options, target ABI digests, and each imported interface semantic hash/pinned code hash. Paths and timestamps do not affect semantic keys.

Cached artifacts are rechecked for container integrity and hash/key match before use. Verified-code cache and build cache are distinct: one certifies bytecode validity; the other avoids recompilation.

### 26.3 CLI

The reference `lm` tool provides:

```text
lm check [path]
lm build [path] [--release]
lm run [path|artifact] [-- arg...]
lm test [path] [filters]
lm inspect <artifact|snapshot>
lm disasm <artifact>
lm snapshot verify <file>
lm snapshot run <file>
```

`check` parses, resolves, and checks types without producing an installed executable. `build` writes canonical artifact and interface files atomically.

`run` links the module and invokes its zero-parameter entry function. The function has the type `() -> T with e`.

A returned callable is an ordinary terminal value. `lm run` does not invoke it by a special rule.

The entry can use `sys.args()` to read strings after `--`. The call needs the `Args` row and policy grant.

`run` constructs a root VM and applies an explicit host policy profile. The artifact row does not grant operations.

The runner accepts explicit machine, image, child, and wait limits.
The CLI exposes these limits through the options in section 14.11.

### 26.4 Root policy profiles

A CLI profile is host configuration, not guest authority. The default profile grants console I/O needed by the command, denies filesystem/network/process access unless requested, sets finite limits, and reports terminal faults. A reproducible/test profile mocks or manually drives clock/random/console operations.

Rows may be displayed to help a human audit a requested profile, but row membership never automatically grants an operation.

### 26.5 Artifact and interface files

Conventional outputs:

```text
build/<target>/<package>.lma     canonical artifact
build/<target>/<package>.lmi     canonical public interface
build/<target>/<package>.map     optional debug/source map
```

Debug maps are keyed by semantic code hash and excluded from semantic identity. Tools tolerate their absence.

### 26.6 Embedding API

The Rust reference host exposes narrow APIs to initialize an ABI bundle, load/verify artifacts and snapshots, create root bindings, configure limits/tables, drive VMs, register host operation implementations, and resolve code hashes. An optional generated C ABI shim exposes opaque handles over the same Rust API. Embedders cannot install unverified executable bytes directly into a `VmState`.

---

## 27. Security and resource requirements

### 27.1 Default denial and capability possession

Every fresh VM table denies all operations. Authority-bearing control objects are passed explicitly: full VM/table handles stay holder-local; proc handles represent send/control rights; resource handles are host designators. Merely possessing `sys`, a class, an operation object, a descriptor, or a row grants nothing.

### 27.2 Admission

A host admits code by one or more explicit rules: known semantic hash, successful bytecode verification plus accepted imports/row bound, signature policy, or an application-specific audit record. Verification proves structural/type/effect claims; it does not decide whether the host grants a requested operation.

### 27.3 Limits

Before allocating from untrusted artifact/snapshot/boundary input, implementations check byte counts, object counts, machine counts, mailbox counts, nesting/work limits, frame/operand maxima, string/collection sizes, and checked arithmetic. Runtime limits cover fuel, heap, stack, pending boundary bytes, snapshot bytes, mailbox bytes/messages, mock work, and host-specific quotas.

A malformed external input returns a load/verify error or faults the controlled boundary; it must not crash the host, overflow arithmetic, allocate beyond declared limits, or create unchecked code/state.

### 27.4 Revocation and fail-closed behavior

Policy edits apply to future performs. Parent/root disappearance, missing code, live host attachments at a snapshot, invalid state, and host registry mismatches fail closed. Snapshot restore creates no authority. A blocked operation is a machine fault, not a value visible to code inside that machine.

### 27.5 No ambient recovery hooks

There are no finalizers, signal handlers, exception hooks, destructor callbacks, dynamic loader callbacks, or implicit module initializers that execute guest code outside normal verified calls/operations. Host cleanup cannot reenter a dead guest.

### 27.6 Side channels and host policy

This specification defines logical authority and isolation, not constant-time execution or denial of timing/memory-pressure side channels between VMs sharing a process. Hosts requiring stronger separation run VMs in separate processes or hardware isolation while retaining the same artifact/operation protocol.

---

## 28. Conformance suite

A conforming implementation passes tests for at least:

1. identical semantic hashes for canonical equivalent compiler output;
2. rejection of malformed, truncated, overlong, and noncanonical artifact encodings;
3. verifier detection of stack, local, type, call, field, intrinsic, perform, row, and scoped-designator inconsistencies;
4. class sealing, initialization safety, override row narrowing, and enum exhaustiveness;
5. exact/group/default table precedence, pure mocks, pass-chain authority, and live revocation;
6. `run`, one-instruction `step`, `drive`, `answer`, `reject`, `dispatch`, waiting, and illegal-state transitions;
7. no host-stack growth proportional to guest call depth;
8. nested-VM default denial and transitive grant charging;
9. deep freeze, cycles, sharing, map order, digest stability, and frozen write barriers;
10. boundary copy of mutable graphs; rejection of scoped and holder-local values; sendable proc-handle transfer;
11. snapshot round trips at every instruction boundary and in `asked`; world closure over reachable machines;
12. one-time snapshot admission followed by guarded execution without repeated whole-image checks;
13. proc isolation, FIFO acceptance, close/drain, pause/resume, dead-peer results, and terminal transfer checks;
14. machine-reference relocation and complete world independence across multi-shot restore;
15. host-attachment preflight and precise blocker paths;
16. reflection and stack views containing no writable guest references;
17. deterministic diagnostics, compile environments, interface/build keys, and byte-for-byte reproducible artifacts;
18. fuel, heap, frame, operand, boundary, mailbox, mock, and snapshot limits;
19. fuzzing of scanners, parsers, artifact/snapshot readers, verifier, boundary codec, graph walker, machine references, handle relocation, and interpreter state transitions;
20. cross-platform ABI vectors for hashes, numbers, UTF-8, floats, manifests, artifacts, snapshots, and value digests.

---

# Appendix A: Surface grammar

This EBNF-like grammar is normative with the clarifications below. `NL` denotes one or more valid expression separators.

```ebnf
module          = opt_separators, { definition, separators },
                  block, EOF ;

definition      = interface_decl | class_decl | enum_decl | function_decl
                | const_decl ;

const_decl      = "const", IDENT, ":", type, "=", expression ;

interface_decl  = "interface", IDENT, [ generic_params ],
                  [ interface_parents ], separators,
                  { ( associated_requirement | interface_method ), separators },
                  "end" ;
interface_parents = ":", interface_ref, { ",", interface_ref } ;
associated_requirement = "type", IDENT, [ bound_clause ] ;
interface_method = "def", IDENT, "(", method_parameters, ")",
                   [ ":", type ], [ effect_clause ] ;

class_decl      = [ class_modifier ], "class", IDENT, [ generic_params ], [ "<", type ],
                  [ implements_clause ], separators,
                  { ( field_decl | method_decl ), separators },
                  "end" ;
class_modifier  = "final" | "frozen" ;
implements_clause = "implements", conformance, { ",", conformance } ;
conformance     = interface_ref, [ premise_clause ] ;
premise_clause  = "when", premise, { ",", premise } ;
premise         = IDENT, bound_clause ;

field_decl      = IDENT, ":", type, [ "=", expression ] ;

method_decl     = "def", IDENT, [ generic_params ], "(", method_parameters, ")",
                  [ ":", type ], [ effect_clause ], [ premise_clause ], separators,
                  block, "end" ;

method_parameters = self_parameter, [ ",", parameters ] ;
self_parameter  = [ "mut" ], "self" ;

function_decl   = "def", IDENT, [ generic_params ], "(", [ parameters ], ")",
                  [ ":", type ], [ effect_clause ], separators,
                  block, "end" ;

enum_decl       = "enum", IDENT, [ generic_params ],
                  [ implements_clause ], separators,
                  { enum_arm, separators },
                  { associated_binding, separators },
                  { method_decl, separators },
                  "end" ;

enum_arm        = IDENT, [ "(", [ field_parameters ], ")" ] ;
associated_binding = "type", IDENT, "=", type ;

generic_params  = "[", generic_param, { ",", generic_param }, "]" ;
generic_param   = IDENT, [ bound_clause ] | "effect", IDENT ;
bound_clause    = ":", interface_ref, { "+", interface_ref } ;

interface_ref   = qualified_name, [ type_args ], { interface_row_arg } ;
interface_row_arg = "with", ( row_item | "(", [ row_items ], ")" ) ;

parameters      = parameter, { ",", parameter } ;
parameter       = { parameter_modifier }, IDENT, ":", type ;
parameter_modifier = "mut" | "escaping" ;
field_parameters= field_parameter, { ",", field_parameter } ;
field_parameter = IDENT, ":", type ;

effect_clause   = "with", ( row_item, { ",", row_item }
                             | "(", [ row_items ], ")" ) ;
row_items       = row_item, { ",", row_item } ;
row_item        = qualified_name | IDENT ;

type            = primary_type, [ function_type_tail ] ;
primary_type    = qualified_name, [ type_args ]
                | "Self", [ ".", IDENT ]
                | "[", type, "]"
                | "{", type, ":", type, "}"
                | "(", [ type_list ], ")"
                | "Op", "[", row_item, ",", type, "]" ;
function_type_tail = "->", type, [ effect_clause ] ;
type_args       = "[", type, { ",", type }, "]" ;
type_list       = type, { ",", type } ;
qualified_name  = IDENT, { ".", IDENT } ;

block           = [ expression, { separators, expression }, opt_separators ] ;
separators      = ( NL | ";" ), { NL | ";" } ;
opt_separators  = { NL | ";" } ;

expression      = assignment ;
assignment      = IDENT, [ ":", type ], "=", assignment
                | postfix, "=", assignment
                | logic_or ;

logic_or        = logic_and, { "or", logic_and } ;
logic_and       = equality, { "and", equality } ;
equality        = comparison,
                  { ( "==" | "!=" ), comparison
                  | ( "is" | "as" ), type } ;
comparison      = bit_or, { ( "<" | "<=" | ">" | ">=" ), bit_or } ;
bit_or          = bit_xor, { "|", bit_xor } ;
bit_xor         = bit_and, { "^", bit_and } ;
bit_and         = shift, { "&", shift } ;
shift           = additive, { ( "<<" | ">>" | ">>>" ), additive } ;
additive        = multiplicative, { ( "+" | "-" ), multiplicative } ;
multiplicative  = unary, { ( "*" | "/" | "%" ), unary } ;
unary           = ( "not" | "-" | "~" ), unary | postfix ;

postfix         = primary,
                  { generic_apply_suffix | call_suffix | field_suffix | index_suffix
                  | propagate_suffix },
                  [ trailing_closure, { propagate_suffix } ] ;
generic_apply_suffix = "[", type, { ",", type }, "]" ;
call_suffix     = "(", [ arguments ], ")" ;
field_suffix    = ".", IDENT ;
index_suffix    = "[", expression, "]" ;
propagate_suffix= "?" ;
trailing_closure= closure ;

arguments       = argument, { ",", argument } ;
argument        = [ IDENT, ":" ], expression ;

primary         = literal
                | IDENT
                | "self"
                | "super", ".", IDENT, "(", [ arguments ], ")"
                | "(", expression, [ ",", [ expression, { ",", expression } ] ], ")"
                | list_literal
                | map_literal
                | closure
                | if_expr
                | while_expr
                | for_expr
                | loop_expr
                | case_expr
                | return_expr
                | break_expr
                | "continue" ;

list_literal    = "[", [ expression, { ",", expression } ], "]" ;
map_literal     = "{", [ map_entry, { ",", map_entry } ], "}" ;
map_entry       = expression, ":", expression ;

closure         = do_closure | brace_closure ;
do_closure      = "do", "|", [ parameters ], "|", [ ":", type ],
                  [ effect_clause ],
                  ( separators, block | expression ), "end" ;
brace_closure   = "{", "|", [ parameters ], "|", [ ":", type ],
                  [ effect_clause ],
                  ( separators, block | expression ), "}" ;

if_expr         = "if", expression, branch_body,
                  { "elsif", expression, branch_body },
                  [ "else", else_body ], "end" ;

while_expr      = "while", expression, header_loop_body, "end" ;
for_expr        = "for", IDENT, [ ",", IDENT ], "in", expression,
                  header_loop_body, "end" ;
loop_expr       = "loop", loop_body, "end" ;
header_loop_body = separators, block | "do", separators, block ;
loop_body       = separators, block | "do", opt_separators, block ;

case_expr       = "case", expression, separators,
                  case_arm, { separators, case_arm }, opt_separators, "end" ;
case_arm        = "in", pattern, branch_body ;
branch_body     = ( "then", opt_separators | separators ), block ;
else_body       = [ separators ], block ;

pattern         = "_"
                | IDENT
                | pattern_literal
                | tuple_pattern
                | qualified_name, "(", [ pattern, { ",", pattern } ], ")" ;
tuple_pattern   = "(", pattern, ( ",", [ pattern, { ",", pattern } ] | { ",", pattern } ), ")" ;
pattern_literal = [ "-" ], INT | CHAR | STRING | "true" | "false" | "()" ;

return_expr     = "return", [ expression ] ;
break_expr      = "break", [ expression ] ;

literal         = INT | FLOAT | CHAR | STRING | BYTES | REGEX
                | "true" | "false" | "()" ;
```

### A.1 Clarifications

- `method_parameters` always starts with untyped source `self` or `mut self`; its containing class supplies the type. There are no source static methods.
- A parameter modifier can occur once. The two modifiers can use either order.
- `escaping` is valid only when the parameter has a direct function type.
- Classes and enums declare only type parameters. Top-level functions and methods may additionally declare `effect` parameters.
- Interface applications use invariant type and effect arguments.
- A missing interface `with` clause supplies empty rows.
- Each interface `with` clause supplies one effect argument in declaration order.
- Parentheses are required for an empty interface row or a row with several items.
- The `+` token separates interface bounds.
- A comma separates class conformances.
- A conformance or method premise starts after `when`.
- A premise subject must name one containing type parameter.
- `frozen class` implies `final class`.
- `final` and `frozen` are identifiers unless `class` follows them.
- A constant value contains literals and tuples only. A numeric literal can have a leading minus.
- An interpolation body uses the normal scanner and permits balanced nested braces.
- A comma separates parent interfaces.
- `()` is unit. `(T,)` and `(T,U)` are tuple types; the same parenthesized list followed by `->` is a function parameter list. A one-element tuple requires the trailing comma.
- `do || ... end` and `{ || ... }` are empty-parameter closures. A closure may put exactly one body expression on the header line; a multi-expression body starts after a separator.
- A left brace followed by a pipe starts a brace closure. Other braces start a map literal. `{}` is an empty map.
- A trailing closure is valid only after a postfix chain that contains a call suffix. It becomes the final call argument. It must start on the same line as that chain. Only `?` can follow it.
- `then` opens an ordinary expression body. Assignment and control transfers need no special arm rule.
- A bracket suffix is generic application only where static resolution permits it and normally precedes a call; otherwise it is indexing. Ambiguous source is rejected.
- A postfix assignment target must be a writable field. An arbitrary call result is not writable.
- Enum arms must precede associated bindings and methods. Associated bindings must precede methods.
- Expected context recognizes a zero-field constructor such as `None`. Another bare name is a binding pattern.
- Class fields and methods may be interleaved. Field layout follows inherited fields then local field source order.
- The built-in static typing rule for `PolicyTable.pass` is specified in section 11.5 and is not expressible fully in the ordinary grammar/type language.
- The scoped-designator escape rule is specified in section 5.13 and is not a general source lifetime syntax.

---

# Appendix B: Canonical row and type text

Tools print types and rows canonically:

```text
List[String]                  prints as [String]
Map[String, Int]              prints as {String: Int}
() -> ()                      omits an empty with-clause
(Bytes) -> Result[Int, IoError] with Io.Write
(T) -> U with e
Op[Clock.Now, () -> Int]
```

Canonical artifact rows expand groups to exact ABI operation identities and sort by canonical hash. Diagnostics may retain authored group spelling. Type parameters are numbered by declaration order in semantic encodings, independent of source variable spelling.

---

# Appendix C: Complete typed manual-driving example

```lm
def supervise(
  program: () -> String with Io.Write, Clock.Now
): Result[String, Fault] with Vm, Io.Write
  vm = sys.vm.Vm().activate_or_fault(program, args: ())
  captured: [String] = []

  loop do
    case vm.drive()
    in Asked(q)
      case q
      in Call(Io.Write, call, (bytes,))
        text = bytes.utf8().expect("the output is UTF-8")
        captured.push(text)
        vm.answer(call, Ok(bytes.len()))
      in Call(Clock.Now, call, ())
        vm.answer(call, 1_700_000_000)
      in _
        vm.reject(q, Fault.denied("the supervisor permits print and time only"))
      end
    in Done(value)
      print("captured #{captured.len()} writes\n").expect("the output writes")
      return Ok(value)
    in Fault(fault)
      return Err(fault)
    end
  end
end
```

No `Any` appears in the reply path. Matching the exact operation recovers its argument tuple and reply type; the runtime still validates machine identity, ordinal, and one-time use. The child receives neither `Io.Write` nor `Clock.Now` through its table. The holder's summary print is its own effect and needs authority in the holder's table.

---

# Appendix D: Deliberate non-goals for version 0.2

- dynamic selector calls, `method_missing`, class reopening, or mutable code;
- ad-hoc unions/intersections, nullable references, traits, or user variance;
- exceptions, rescue, unwinding, destructors, or finalizers;
- general borrow checking or user-defined lifetime parameters;
- shared-memory guest threads;
- stateful policy-table callbacks;
- ambient imports, mutable globals, or effectful module initialization;
- transparent serialization or silent reopening of live OS resources;
- partial snapshots that exclude reachable machines;
- sharing one live machine between a restored world and its original;
- record/replay layers, reply channels, attenuated handles, or remote scheduling;
- guest-visible JIT controls or JIT-dependent semantics;
- guarantees against microarchitectural or process-wide timing side channels.
