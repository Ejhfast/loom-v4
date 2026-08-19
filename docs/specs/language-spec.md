# Language Specification

Status: version 0.2 design specification  
Source form: UTF-8 text, conventional extension `.lm`  
Artifact form: canonical bytecode module, conventional extension `.lma`  
Snapshot form: serialized machine image, conventional extension `.lms`

This specification defines an object language with a reified compiler, reified virtual machines, immutable code identity, explicit effect rows, runtime policy tables, snapshots, and isolated procs. “Must” and “must not” are normative. Text labeled *implementation note* describes the reference implementation without changing observable semantics.

---

## 1. Governing model

Two rules govern the language.

**Types describe; the VM decides.** The type/effect checker proves static facts. An effect row is an upper bound on operations code can request. A type never grants authority. Every actual operation request is decided by the controlled machine's policy table or by the holder manually driving that machine.

**Machine state is data.** Guest frames, locals, operands, program counters, pending requests, and suspended continuations are explicit VM-owned records. Guest calls never rely on a live host-language call stack. Stepping, snapshots, nested VMs, migration, inspection, and procs all follow from this representation.

There is one guest-to-host boundary primitive: calling an operation object. Printing, files, clocks, randomness, networking, compilation, VM control, snapshots, reflection, and proc communication all use it.

### 1.1 Semantic, library, and implementation layers

A conforming distribution keeps five semantic layers distinct.

1. **Language primitives.** Syntax and structural types that cannot be written as ordinary declarations: unit, `Never`, `Any`, scalar machine types, tuples, function types, operation types, local mutation capability, and the bytecode/runtime machinery required to execute them.
2. **Core image.** A pinned, dependency-free artifact compiled from ordinary language source plus declarations for native classes. It defines nominal types required by public signatures, including `Option`, `Result`, `Ordering`, `Pair`, `Range`, VM/proc event enums, and portable error values. These are not parser keywords and are not magic enum layouts. Their structural hashes are part of the language ABI, and an artifact names each of them through a stable core role slot (5.2).
3. **Prelude.** A deliberately small set of names implicitly introduced during name resolution. The prelude re-exports selected primitive, native-core, and core-image names; it does not define their identity and does not automatically import general algorithms or host wrappers.
4. **Standard library.** Explicitly linked ordinary modules for collections algorithms, text, formatting, paths, files, time, random, networking, JSON, VM helpers, proc supervision, compilation, reflection, and testing. Standard-library code can call pure intrinsics or explicit host operations but cannot bypass rows or policy.
5. **Host operations.** Fixed members of `sys.*`. They may suspend, are policy-gated, and their exact identities appear in rows.

This separation breaks the bootstrap cycle cleanly. The stage-0 compiler knows only primitive types and the declarative native-class manifest. It compiles the core image first. Host-operation signatures are then resolved against the core role slots of the artifact, so an operation may return `Result[Option[String], IoError]` without making `Option` or `Result` compiler built-ins.

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
lm-vm + lm-bytecode + lm-verify + lm-value + lm-graph
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

Spaces, tabs, carriage returns, and newlines separate tokens. A newline or semicolon terminates a statement when not inside delimiters, a string, or an unfinished operator expression.

A line comment starts with `#` and extends to the newline. There are no block comments in version 0.2.

### 2.3 Keywords

```text
and as break case class continue def do effect else elsif end enum
false if in loop mut not or return self super then true use while with
```

`sys` is a prebound ordinary value, not a keyword.

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

A string is immutable UTF-8 text:

```lm
"hello"
"line one\nline two"
"Hello {name}!"
```

Escapes are `\\`, `\"`, `\'`, `\n`, `\r`, `\t`, `\0`, and `\u{HEX}`. `{ expression }` interpolates a compiler-known pure textual conversion. `{{` and `}}` encode literal braces. Version 0.2 provides interpolation for core scalar values, `String`, `Bytes`, `Digest`, and `Fault`.

Triple-quoted strings preserve line breaks and use the same escaping and interpolation rules. A byte string is immutable bytes:

```lm
b"LM\0\x01"
```

Byte strings accept `\xNN` and reject interpolation.

### 2.6 Punctuation and operators

Punctuation:

```text
( ) [ ] { } , : . ; |
```

Operators:

```text
= == != < <= > >= + - * / %
```

A left brace followed by a pipe starts a brace closure. Every other left brace starts a map literal. The empty form `{}` remains an empty map.

`and`, `or`, and `not` are short-circuit Boolean operators. User-defined operator overloading is absent.

---

## 3. Modules, compilation environments, and linking

### 3.1 Source module

A source module is a sequence of top-level definitions followed by at most one trailing expression:

```lm
class Greeter
  def greet(self, name: String): String
    "Hello {name}!"
  end
end

def twice(x: Int): Int
  x * 2
end

do |name: String|: String
  Greeter().greet(name)
end
```

Top-level definitions are `class`, `enum`, and `def`. There are no mutable module variables, top-level assignment slots, effectful initializers, or runtime namespace installation. All definitions are exported by source name. The optional trailing expression becomes the module entry value.

Inside a package, one module holds the program entry: `src/main.lm`. Every other module must end without a trailing expression. The file tree under `src/` is the module tree, and the module path across packages carries the package name of the manifest (`docs/specs/sidecar/packages.md`).

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

The `use` declaration is the source-level surface of this rule. A `use` line binds one dotted path to a short name. A `use` of another module compiles to a named import slot, and the build tool fulfills it. `use` never grants authority and never changes an effect row. The package layout, the manifest, and the resolution roots are defined in `docs/specs/sidecar/packages.md`.

A `use` path starts at a root name. The root set is fixed per module: the dependency keys of the manifest, this package's own top-level modules, `std`, and `sys`. A collision inside the root set is a compile error, and the fix is a manifest rename; resolution never picks silently. A path that names a module binds that module, and every export of it resolves under the bound name. A path that names one export of a module binds that export.

One import slot names the providing module, the exported name, the kind, and the pinned interface hash. A compiler checks the importing module against the interface alone, and never against the implementation of the provider. The linker resolves each slot and rejects a provider whose interface hash differs from the pin.

### 3.4 Primitive compile operation

The primitive compiler API uses a typed heterogeneous environment object rather than `{String: Any}`:

```lm
env = CompileEnv()
assert(env.bind("Json", Json).is_ok())
assert(env.bind("Config", config.freeze()).is_ok())

env = env.freeze()
result = sys.compiler.compile(source, env, CompileOptions())
```

Conceptually:

```text
CompileEnv.bind[T](mut self, name: String, value: T)
  -> Result[(), CompileEnvError]
Compiler.Compile(String, CompileEnv, CompileOptions)
  -> Result[Artifact, CompileErrors]
```

`bind` records the value's exact static signature, optional code hash, and a frozen control-envelope representation. Binding two values to one name or freezing an incompatible value is an ordinary `CompileEnvError`. The environment itself is holder-side control data and is not a general guest map.

The compiler records only referenced import slots and never captures the supplied value into the artifact. The `std/compiler` wrapper `compile(source)` supplies an empty frozen environment and default options. Truly dynamic compiler tooling uses `DynValue`, described in section 5.6, rather than widening normal APIs to `Any`.

### 3.5 Pure entry construction

Linking evaluates the trailing expression to construct the entry value, so that expression must have the empty effect row. Startup work is represented by returning a closure:

```lm
do || with Io.Print
  sys.io.print("started\n")
end
```

Creating this closure is pure; calling it requests `Io.Print`.

### 3.6 Linking

```lm
bindings = LinkEnv()
assert(bindings.bind("Json", Json).is_ok())
assert(bindings.bind("Config", config.freeze()).is_ok())

case artifact.link(bindings.freeze())
in Ok(linked)
  parser = linked.definition(
    "parse",
    expected: type_descriptor[(String) -> Result[Json, JsonError]]()
  )
  program = linked.entry(
    expected: type_descriptor[() -> () with Io.Print]()
  )
  # parser and program are typed Result values
  ()
in Err(errors)
  # report the LinkErrors value
  ()
end
```

`LinkEnv.bind[T]` returns `Result[(),LinkEnvError]`; `Artifact.link` returns `Result[LinkedModule,LinkErrors]`. The typed environment must contain exactly one frozen compatible value per import slot. Linking validates signatures and pinned hashes, creates local class/function values, evaluates the pure entry expression, deep-freezes the result, and returns a `LinkedModule`. `definition` and `entry` return typed `Result` values rather than an erased lookup. Linking installs nothing globally.

Missing, extra, incompatible, or mutable bindings produce `LinkErrors` in the trusted API. Injecting malformed linked state into a VM faults with `LinkMismatch`.

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
- A **VerificationHash** stays stable through a class rename and a function rename, and it moves on a selector rename. A class key, a definition name, and a function binding all live in the export section, which the verifier never reads. A selector name lives in the semantic region, which the verifier does read.

Structural refinement cannot always give each member a unique label. The stable partition of this rule is bisimulation: two members keep one label exactly when they are bisimilar. Bisimulation is coarser than isomorphism, so the rule may give one label to two members an isomorphism test separates. One label stays sound, because a member is a deterministic system with ordered successors: two bisimilar members have identical unfoldings, so they compute the same thing. Members with one label share one StructuralHash, and their QualifiedKey values keep them distinct wherever distinctness is observable.

Refinement runs on untrusted input before the verifier, so its work is bounded twice: once per component and once per module. A component or a module past its bound rejects with a clear diagnostic. The bound is large enough that no source program reaches it.

A **method** takes part in its class identity as the pair of the selector name and the implementing function identity. Selector identity is therefore name-based and independent of any method body. An override with a different body keeps the selector name.

An **InterfaceHash** covers only the exported name, the kind, and the full signature, with class references by qualified name. It covers no method body and no function body. Import slots pin interface hashes. An edit to an exported body therefore moves the StructuralHash of that body and no interface hash, and no dependent module recompiles. The linker resolves an import slot to a definition, and it rejects a slot whose provider interface hash differs from the pin.

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
4. **enum constructors:** canonically qualified names such as `Option.Some` and `RunResult.Fault`.

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
| `Type[T]` | Typed runtime type descriptor |
| `TypeView` | Erased frozen type descriptor for diagnostics/dynamic checks |
| `Class[T]` | Class value constructing `T` |
| `PolicyTarget` | Sealed non-callable parent used by non-granting table edits |
| `Operation` / `OperationGroup` | Exact-operation and group policy descriptors; both subtype `PolicyTarget` |
| `DynValue` | Explicit type/value package for dynamic APIs |
| `ValueView` | Opaque frozen diagnostic view of a value |

**Core-image nominal types** are ordinary source definitions with pinned hashes.

The minimum set includes `Option`, `Result`, `Choice`, `Ordering`, `Pair`, `Range`, `RunResult`, `StepEvent`, `DriveEvent`, `Recv`, and `ProcResult`.

It also includes portable operation errors and typed VM request tokens.

**Host and holder types** include `Vm[T]`, `Wait[T]`, snapshots, proc handles, policy tables, file handles, and socket handles.

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

An artifact carries a **core role table**: one class slot per stable core role, for example `Option`, `Option.Some`, and `Option.None`. The compiler fills the table, the linker relocates it, and the verifier proves the kind, the generic arity, the parent slot, and the exact field layout of every filled slot. A rule that needs a core family, such as the pending-call type of a `Call` pattern, reads a slot. It reads no name and no hash, so a rename changes nothing the verifier reads, and an artifact with no source resolves its core from its own bytes. A family whose parent slot is filled must fill every arm slot.

Pattern matching and exhaustiveness use the same enum machinery as user enums. The host ABI reads the same slots, so `Io.ReadLine` and user code cannot silently disagree about what `Result` means.

The prelude merely puts `Option`, `Some`, `None`, `Result`, `Ok`, `Err`, `Ordering`, `Pair`, `Range`, `List`, and `Map` into unqualified scope. Removing a name from a future prelude revision does not change its core identity.

### 5.3 Nominal classes and inheritance

A class introduces a nominal instance type and a value of type `Class[InstanceType]`:

```lm
class Animal
end

class Dog < Animal
end
```

`Dog <: Animal`. Inheritance is single. A class identity is its normalized sealed definition closed over dependency hashes. An instance's runtime class slot resolves to that identity.

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

Class arguments are invariant. Version 0.2 has no generic bounds, traits, higher-kinded parameters, specialization syntax, or user-declared variance. Top-level functions and methods may declare type and effect parameters.

Generic definitions are checked once with type variables and share one bytecode body. Loaded type applications receive dense `TypeId` and class-instantiation slots used by reflection, boundary validation, and field signatures. Ordinary value slots remain uniformly represented, so `List[Int]` and `List[String]` use the same list code and buffer shape. Version 0.2 does not monomorphize or unbox generic elements; a later optimizer may specialize while preserving the verified generic body as the deoptimization target.

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

### 5.6 `Any`, `DynValue`, and deliberate dynamic boundaries

Every ordinary value can widen to `Any`, but normal generic APIs must use a type parameter rather than `Any`. In particular, list algorithms, `freeze`, `digest`, `deep_equal`, VM results, proc messages, compile environments, and operation replies preserve their caller's type.

`Any` is a primitive name but prelude and standard APIs do not return it merely for convenience. It appears only in code that intentionally does a dynamic type test. Narrowing is explicit:

```lm
if value is String
  text = value as String
end
```

`is` is pure. `as` returns the same value or faults with `BadCast`.

Truly dynamic APIs use an opaque package:

```text
dyn_pack[T](value: T) -> DynValue
DynValue.type(self) -> TypeView
dyn_unpack[T](value: DynValue, expected: Type[T]) -> Option[T]
type_descriptor[T]() -> Type[T]
```

`type_descriptor[T]()` is a pure compiler-known constructor for the canonical typed descriptor of `T`; it allocates no user-visible dynamic type object on repeated use and is the witness passed to typed linker and dynamic-unpack APIs.

`DynValue` is frozen and preserves the hidden exact type. It does not implicitly subtype every `T`, so dynamic data cannot infect a surrounding generic type by accident. Reflection and diagnostics use `ValueView`, which exposes type, bounded formatting, digest when available, and structural children as more views; it cannot be cast back into a live guest value.

### 5.7 Function, operation, and effect-variable types

A function type includes parameters, result, and row:

```lm
(String, Int) -> Bool
(String) -> () with Io.Print
(T) -> U with e
```

The checker normalizes source function syntax to the structural form `Fn[A,R,e]`, where `A` is the fixed argument tuple, `R` the result, and `e` the row. `Fn` is ABI/type-checker metanotation rather than an additional source type name; it lets native APIs such as `EmptyVm.from_fn` use ordinary first-order generics instead of a variadic or dependent typing rule. Function parameters are contravariant, results covariant, and effects covariant by set inclusion.

An operation object has an identity-indexed callable type:

```lm
Op[Io.Print, (String) -> ()]
Op[e, (String) -> ()]
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

## 6. Expressions and statements

Every construct is an expression, though many evaluate to `()`.

### 6.1 Blocks, assignment, and calls

A body is a sequence of expressions. Its value is the last expression, or `()` if empty. `return` exits the nearest callable.

Assignment declares/rebinds a local or writes a permitted field/index. It evaluates to `()`.

```lm
x = 1
x = 2
self.name = "Ada"
```

Calls use parentheses. Arguments evaluate left to right; the receiver evaluates first. Labeled arguments follow positional arguments and match declared parameter names:

```lm
f(1, 2)
obj.method(1)
vm.from_fn(program, args: ("Ada",))
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

The two trailing forms have identical precedence and evaluation order. A call accepts at most one trailing closure. A trailing closure must start on the same line as the end of the call. A newline after the call ends the statement first (2.2). No postfix suffix may follow a trailing closure. There is no overload resolution.

### 6.2 Closures

```lm
increment = do |x: Int|: Int
  x + 1
end

printer = do |text: String| with Io.Print
  sys.io.print(text)
end

thunk = do || 42 end
```

A brace closure is an equivalent spelling:

```lm
increment = { |x: Int|: Int x + 1 }
thunk = { || 42 }
```

Both forms lower to the same typed HIR node and bytecode form. They have identical capture, result, row, and evaluation rules. A closure is a sealed function object containing code identity and captures. Omitting `with` means empty row.

A monomorphic top-level function name produces a zero-capture function value.
A generic function name needs a direct call in this version.

### 6.3 Fields, `self`, and `super`

`receiver.field` is statically resolved. `self` exists only in methods. A mutating method declares `mut self`. `super.method(args)` calls the immediate superclass implementation with the same receiver and a compile-time selector.

### 6.4 Arithmetic, comparison, and equality

`Int`, `Bool`, and `String` use final core method tables. The checker maps each supported source operator to one sealed method.

```text
-a      -> a.__neg__()
not a   -> a.__not__()
a + b   -> a.__add__(b)
a - b   -> a.__sub__(b)
a * b   -> a.__mul__(b)
a / b   -> a.__div__(b)
a % b   -> a.__rem__(b)
a == b  -> a.__eq__(b)
a != b  -> a.__ne__(b)
a < b   -> a.__lt__(b)
a <= b  -> a.__le__(b)
a > b   -> a.__gt__(b)
a >= b  -> a.__ge__(b)
```

Each core method body names one pure intrinsic manifest entry. Static resolution and trivial-body inlining emit the canonical instruction.

`Text + Text` uses `Text.__add__` and produces String.

Any class may declare these hooks. The operator reads the hook from the class of the left operand, and the call takes the ordinary method path:

- the declared parameter type checks the right operand;
- the declared result type is the type of the operator expression, and it needs no relation to the operand types;
- the declared effect row is charged to the caller, so `a + b` can perform an operation, and the caller must hold the row;
- a `final` class calls directly, and any other class dispatches virtually.

A class that declares no hook keeps the rules below. The operator sugar adds a spelling and removes no rule.

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

Version 0.2 states no rule about the meaning of a hook. `__eq__` need not be symmetric, and `__lt__` need not order anything. A later interface or protocol feature can require such properties of a class that claims them.

`__eq__` governs `==` and `!=` alone. `Map` keys, `digest`, and `std.value.deep_equal` never call a hook.

Text map keys use their visible UTF-8 content. A `String` key and a `Substring` key match when their visible content matches.

`has`, `get`, `at`, and map indexing accept Text for any text-keyed map. `put` still requires the declared key type.

Other classes use structural identity for map keys. A class hook can therefore disagree with map lookup and deep equality.

`and` and `or` remain control-flow operators. They evaluate the right operand only when required.

For `Int`, `+`, `-`, and `*` are checked; `/` truncates toward zero; `%` has the dividend's sign; divide-by-zero and the one overflowing division case fault. For `Float`, `+`, `-`, `*`, and `/` follow the deterministic binary64 rules in section 2.4; division by zero produces the corresponding infinity or NaN and `%` is not defined. There is no implicit numeric conversion.

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

Strongest to weakest: postfix call/field/index and a trailing closure; unary `not`/`-`; multiplicative; additive; ordering; equality/`is`/`as`; `and`; `or`; assignment. Assignment is right-associative; other binary operators are left-associative.

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

### 7.2 Loops

```lm
while condition
  body()
end

loop do
  body()
end
```

`break` exits the nearest loop; `continue` begins its next iteration. A loop has the type `()`.

A loop whose condition is the literal `true` and whose body holds no `break` of that loop has the type `Never` instead. `loop do ... end` is the same statement as `while true`, so both take this rule. `Never` is a subtype of every type, so such a loop ends a body of any declared result type. The statement after it is unreachable, and an unreachable statement is a compile error.

The body decides nothing here. A body that returns on one path and repeats on another still never reaches the statement after the loop. `break` is the only normal exit.

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

Arms are tested in source order. An arm may use `then` with one expression or a newline body. Cases over enums and `Bool` are checked for exhaustiveness; cases over other types require a wildcard or binding arm. Duplicate unreachable arms are compile errors.

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

The compiler lowers select to `Wait.choose`, `Wait.wait`, and `Choice`. Section 23.7 defines their operations.

The runtime tests ready arms in source order whenever the proc resumes.

---

## 8. Classes and objects

### 8.1 Declaration and sealing

```lm
class Hello
  name: String = ""

  def set_name(mut self, name: String)
    self.name = name
  end

  def say_name(self) with Io.Print
    sys.io.print("Hello {self.name}!")
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
sys.net      sys.proc     sys.vm       sys.compiler
sys.reflect
```

The ABI also supplies effect/policy descriptor constants `Io`, `Fs`, `Clock`, `Rand`, `Net`, `Proc`, `Vm`, `Compiler`, and `Reflect`, plus exact constants such as `Io.Print` and `Clock.Now`.

A group constant is an `OperationGroup`; an exact constant is an `Operation`. `sys.io.print` is the callable `Op[Io.Print, (String) -> ()]` corresponding to descriptor `Io.Print`. Scope grants nothing.

Casing separates the two roles. A callable member of `sys` uses the
snake_case form of its descriptor name: `sys.io.print` performs
`Io.Print`, and `sys.io.read_line` performs `Io.ReadLine`. The
mapping is mechanical. Descriptors keep initial capitals, and they
appear wherever code names, grants, mocks, or matches an operation.
Exactly one `sys` member is capitalized: the machine constructor
`sys.vm.Vm()`, whose name is the constructed type. Every other
member is a snake_case verb, including members that return objects,
such as `sys.reflect.mirror(obj)`. The rule of thumb: lowercase
performs the operation; a capitalized name talks about it.

### 11.2 Perform

```lm
sys.io.print("Hello")
```

Calling an operation object executes one `PERFORM`. The VM records exact identity, arguments, expected reply type, destination, and continuation PC, then either dispatches automatically or exposes the request to a manual driver. No other guest mechanism reaches host semantics.

### 11.3 Rows and checking

A row is a comma-separated set of exact identities, groups, and effect variables:

```lm
def print_name(self) with Io.Print
  sys.io.print(self.name)
end

def copy(src: String, dst: String) with Fs
  # body
end

def apply[T, U, effect e](x: T, f: (T) -> U with e): U with e
  f(x)
end
```

A group denotes its fixed ABI operation set. Omitting `with` means empty row.

For each body the checker unions direct performs, declared rows of statically selected calls, effect variables of called higher-order values, and initializer rows. The declared row must be a superset. Checking is local; no whole-program inference is required.

An override may not widen its row. Therefore a virtual call through a supertype is charged safely from the supertype signature.

### 11.4 Dynamic choice without dynamic selectors

```lm
routes: {String: () -> () with Io} = {
  "health": do || sys.io.print("ok") end,
  "help": do || sys.io.print("help") end
}

routes[route]()
```

The selected closure carries its row in its function type. There is no operation that invokes a method by computed name.

### 11.5 Row inclusion and grants

Rows are ordered by operation-set inclusion:

```text
empty row <: Io.Print <: Io <: Io, Fs
```

Admission checks use subsumption.

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

A fault contains at least:

```text
code: FaultCode
message: String
operation: Option[Operation]
data: {String: DynValue}  # frozen, bounded
trace: [FrameView]        # frozen, bounded
```

Hosts may redact message and trace details while preserving the stable code.

### 12.3 Stable codes

| Code | Cause |
|---|---|
| `PolicyDenied` | blocked or ungranted operation |
| `FrozenWrite` | write into a frozen object |
| `OutOfFuel` | instruction/intrinsic budget exhausted |
| `HeapLimit` | heap limit exceeded |
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
in Done(v)  then use(v)
in Fault(f) then recover(f)
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

Editing a table while a proc runs affects future lookups; it does not retroactively cancel a host operation already accepted unless that operation's own semantics expose cancellation.

### 13.5 Manual policy

`drive()` stops its direct machine before lookup. A descendant request also stops when a pass reaches the active driver.

The holder can answer, reject, or dispatch the request. `dispatch()` applies the stopped table for a direct request.

For a routed request, `dispatch()` continues after the pass that reached the driver. Tables remain the only automatic policy mechanism.

---

## 14. Virtual machine object

### 14.1 Native class and result parameter

`Vm` is native. Its instances are ordinary holder values; control methods are operations in group `Vm`. Every nested machine executes on the same native interpreter rather than recursively interpreting an interpreter.

The public families preserve the final result type:

```text
EmptyVm
Vm[T]
StepEvent[T]
RunResult[T]
DriveEvent[T]
Snapshot[T]
SnapshotImage
```

There is no execute-an-unknown-signature shortcut. `from_artifact` requires a `LinkedEntry[A,R]` obtained by matching the artifact entry against a concrete `Type[Fn[A,R,e]]` witness. A machine has type `Vm[DynValue]` only when the admitted program itself explicitly returns `DynValue`.

### 14.2 Construction and loading

```lm
empty = sys.vm.Vm()
vm = empty.from_fn(program, args: ("Ada",))
```

`Vm.New` creates an `EmptyVm` with a fresh heap, frames, limits, and default-deny table. `from_fn[A,R,e](self, program: Fn[A,R,e], args: A) -> Vm[R]` is an ordinary generic native declaration over the normalized structural function form: it checks the supplied tuple, transfers code/captures/arguments through the boundary codec, creates the initial frame without executing, transitions the native receiver out of the empty state, and returns `Vm[R]`. An aliased stale `EmptyVm` handle is harmless: any second load attempt is rejected by the runtime state check.

`from_artifact` accepts only a typed `LinkedEntry[A,R]`. Tooling may inspect an entry through `TypeView`, but it must check a concrete function descriptor before obtaining a loadable entry; version 0.2 has no identity-erased dynamic invocation.

### 14.3 States

| State | Meaning |
|---|---|
| `empty` | no entry loaded; public handle type is `EmptyVm` |
| `ready` | paused and holder-controlled |
| `running` | executing on a host thread |
| `asked` | `drive` stopped before dispatch |
| `waiting` | dispatched host completion pending |
| `proc_owned` | scheduler owns execution |
| `done` | terminal value stored |
| `faulted` | terminal fault stored |

There is at most one pending perform record.

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

enum RunResult[T]
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

The wider erased surface stays deferred. `q.op(): Operation` needs an identity-erased operation value, which version 0.2 does not define; `q.args_view()` and `q.reply_type()` need `ValueView` and `TypeView` with them.

To read arguments or answer, the holder matches the request against an exact typed operation object:

```lm
case q
in Call(Io.Print, call, (text,))   # the tuple is (String,)
  captured.push(text)
  vm.answer(call, ())              # reply is statically ()
in Call(Clock.Now, call, ())
  vm.answer(call, 123)
in _
  vm.dispatch(q)
end
```

`Call(op, call, args)` names one exact operation of the manifest, binds the `PendingCall`, and matches `args` against the argument tuple. The operation set is open, so a `case` over a `Request` always needs a final wildcard arm, and two arms that name one operation report the second as unreachable.

Call a continuation method on the same `Vm` receiver that produced the event. The route proves that the descendant request reached this receiver.

The `Call` pattern has a narrow compiler-known type rule. Its first position is an exact `Operation` descriptor known to the checker, such as `Io.Print`. If the manifest signature of that descriptor is `(A...) -> R`, the arm binds a `PendingCall[(A...), R]` and matches its third position against `(A...)`. The callable `sys` member is not used here: matching is descriptor work, and the compiler supplies the typed signature from the manifest. `PendingCall[A,R]` exposes:

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

Loading a nonempty VM also faults the caller without changing the controlled machine. Repeating `drive` performs the token recovery described above.

Terminal execution calls return the stored terminal event idempotently.

### 14.10 Reentrancy, inspection, and ownership

A control method on a currently `running` VM, or from guest code executing inside that same VM, faults. Execution and inspection methods also fault while `proc_owned`. `table()` and edits through an already obtained table handle are the explicit synchronized exception, permitting live revocation.

A routed request parks its descendant activation chain. Only the holder of the driven surface can consume that route.

`stack()` is valid only while not running or proc-owned and returns deep-frozen `FrameView` values: function identity/name, PC, source location if present, locals as `ValueView`, and a bounded operand summary. No live guest reference escapes. The name of a frame is the set of names that bind its function value (3.7), because two equal bodies share one code object and keep both names.

At most one host thread owns execution. Guest execution remains one logical thread.

### 14.11 Fuel and limits

A VM has instruction/intrinsic fuel, heap-byte limit, frame/operand limit, boundary-byte limit, mailbox limit, and snapshot-byte limit. One bytecode instruction consumes one fuel unit; pure intrinsics have deterministic published charges based on logical input size rather than host hash-table probe count.

A parent granting child resources reserves them from its own budget. A root host may mint resources. Exceeding a limit faults only that VM.

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
  case sys.vm.Vm().from_fn(do || 21 end, args: ()).run()
  in Done(v)  then v
  in Fault(_) then 0
  end
end

def f1(e: () -> Int with Vm): Int with Vm
  expr = do || with Vm
    x = e()
    x + x
  end

  vm = sys.vm.Vm().from_fn(expr, args: ())
  vm.table().pass(Vm)
  case vm.run()
  in Done(v)  then v
  in Fault(_) then 0
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

### 17.1 The machine world

Machine state is data (section 1). A snapshot copies one machine world at one moment and persists the copy. Restore builds an independent world from the copy, and that world continues the same way. This section defines the world, the moment, and the two conditions that block a copy.

```lm
case vm.snapshot()
in Ok(snap)
  case sys.vm.Vm().restore(snap)
  in Ok(vm2) then vm2.run()
  in Err(error) then report_restore(error)
  end
in Err(error)
  report_snapshot(error)
end
```

A held snapshot names one paused `Vm[T]` as its root. A receiverless self snapshot names the performing machine as its root.

The snapshot world contains the root and every reachable machine. Handles, nested control edges, and routed requests establish reachability.

Heap, frame, closure, mailbox, pending, and terminal values can contain handles. Reachability is transitive.

Running procs, paused procs, terminal procs, and held nested machines all ride along.

The world is closed by construction. Reachability follows the handles, so every handle in the capture targets a captured machine. A reference that leaves the world is not representable. The design therefore needs no ownership records, no external references, and no restore-time resolution. What cannot exist needs no tracking.

The surface spellings lower to distinct exact identities `Vm.SnapshotHeld` and `Vm.SnapshotSelf`. A held call returns `Result[Snapshot[T],SnapshotError]`. A self call returns `Result[SnapshotImage,SnapshotError]`, because the calling function cannot name the enclosing machine result type.

External bytes first pass through `sys.vm.load_snapshot(bytes)`. The loader decodes and admits the bytes once and returns `Result[SnapshotImage,SnapshotError]`.

A guest `SnapshotImage` always has admitted host backing. Every path that builds one runs admission (section 17.8) or copies a stopped verified world. Editable snapshot data is a host state with no guest spelling, and it never backs a guest value. `Snapshot[T]` is a typed view over one `SnapshotImage`, so it adds no other state.

```text
SnapshotImage.result_type(self) -> TypeView
SnapshotImage.cast_result[T](self, expected: Type[T])
  -> Result[Snapshot[T], SnapshotTypeError]
SnapshotImage.to_bytes(self) -> Bytes
Snapshot[T].to_bytes(self) -> Bytes
```

### 17.2 World contents

A snapshot contains format and ABI versions, code manifests, type tables, heaps, frames, limits, fuel, and machine states.

It also contains pending requests, nested control edges, routed requests, mailboxes, terminal results, machine references, and a container hash.

It excludes policy tables, root grants, live host callbacks, host thread identity, executor tasks, mutex/channel storage, wake objects, and live OS handles. It can include closed resource handles.

The encoder assigns one canonical machine ordinal to each captured machine. A handle in snapshot bytes stores that ordinal and its static type. Restore relocates every handle to the corresponding restored machine. This covers handles in heaps, frames, locals, operands, closure captures, mailbox values, pending arguments, and terminal results. Relocation is implementation work and is not observable from guest code.

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

`restore(snap: Snapshot[T])` is valid only on an `EmptyVm`. It builds the complete world and returns `Result[Vm[T],RestoreError]`. `RestoreError` includes at least `RestoreLimitExceeded`. A failed restore exposes no partial world.

Policy tables are never serialized. Each restored machine receives a fresh default-deny table. Internal pass chains refer to the new parent tables. Restore creates no authority.

A routed cursor outside the captured world binds to the restoring holder. Dispatch then consults the restoring holder's table.

The cursor restores no old table grant.

The returned root VM is holder-controlled. Restored procs are scheduler-owned but stopped behind one world gate. The first root `run`, `step`, or `drive` opens that gate.

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

Loading has two stages, and they prove different properties. Decoding protects the host from the byte stream. Admission establishes the interpreter invariant. An editor can build the same invalid state with no container behind it, so admission never trusts a decode result.

Decoding checks:

- magic, version, canonical integers, section bounds, and container hash;
- every count against a load limit and against the bytes that remain, before any allocation;
- one representable value for every wire tag.

Decoding produces editable snapshot data. That data promises nothing about references, machine state, or types.

Admission uses this rule:

> Editable snapshot data becomes a `SnapshotImage` only when its structure resolves and every live declared type is accurate.

"Declared type" includes the type the bytecode verifier proves at a saved program point. It never means only a type label the data carries.

Structural resolution checks the root machine, every machine and object ordinal, every code and class identity, every frame at a reachable instruction boundary, the frame partition of the local and operand arenas, every object field and element count, every capture context, every literal, every parent chain, every machine reference, and the lifecycle records of every machine.

Type accuracy checks every initialized local, every operand of each stopped frame, every closure capture, every initialized instance field, every pending argument, every accepted mailbox value, every terminal result, every typed native value, and every reachable collection element. Admission derives each type from verified code and resolved layouts. It applies every generic substitution before it checks a value, and it treats an uninitialized slot as a state, not as a type wildcard. A native value that names another machine or record takes its type from that target.

Admission proves no other property. It does not prove termination, useful control state, scheduler fairness, request-token history, external authority, or target-world resources. A strange but structurally valid typed state remains legal.

Restore accepts an admitted `SnapshotImage` alone. Execution, answering, and later snapshotting repeat no structural check and no type check. The write barrier and normal dynamic checks remain active because they enforce runtime semantics, not snapshot trust.

An in-process snapshot of a stopped verified world holds the same invariant by construction, so its capture path repeats no graph check. Origin grants no other trust: both paths produce the same `SnapshotImage` guarantees.

A nested snapshot stays opaque. Admission matches its declared root result type and admits its body at its own restore.

### 17.9 Canonical form

The canonical snapshot representation uses deterministic section order, little-endian fixed fields where specified, canonical LEB128 counts/integers, object ordinals assigned by root traversal, machine ordinals assigned by deterministic reachability traversal from the root, and BLAKE3-256 domain-separated hashes. Debug/source-map data may be present but does not affect guest semantic identity.

Canonical bytes carry no admission status. The container hash identifies bytes, and the admission status is a fact of one process. Writing a `SnapshotImage` to bytes therefore transfers no trust, and loading those bytes in another process repeats admission (section 17.8).

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
in Done(v)  then v
in Fault(_) then 0
end
```

The proc instance is constructed inside its VM. The spawner receives only a typed `Handle[M,R]`, where `M` is the mailbox message type and `R` is the declared result of `on_spawn`.

### 18.2 General launch

```lm
vm = sys.vm.Vm().from_fn(program, args: ("Ada",))
vm.table().pass(Io.Print)
vm.table().mock(Clock.Now, do || 0 end)

h: Handle[Never, ()] = sys.proc.run(vm)
```

`Proc.Run` with no mailbox argument chooses `M = Never`. The mailbox-bearing native form accepts an explicit `MailboxType[M]` created by proc-class lowering. `Proc.Run` atomically transfers execution ownership to the scheduler. The original VM handle becomes dormant; execution/inspection through it faults until `pause()` returns ownership. These methods are operations and therefore carry their exact `Proc.*` rows; table edits remain legal for revocation.

### 18.3 `spawn` sugar and birth grant

`Class.spawn(args...)` is compiler sugar available only for a subclass with a valid `on_spawn`. It constructs a VM from the proc class and a typed argument tuple, transfers code/data through the codec, grants the child `Proc` group, creates the declared mailbox, and invokes `Proc.Run`. The return type is `Handle[M,R]` inferred from the proc superclass and `on_spawn` result.

The birth grant is required so mailbox-bearing procs can receive. Since `spawn` itself carries `Proc`, the spawner is statically allowed to pass that group. Additional grants, mocks, limits, or admission checks use the explicit VM path.

### 18.4 Handles and terminal results

The core image defines the supervisory values explicitly:

```lm
enum ProcResult[R]
  Done(value: R)
  Fault(fault: Fault)
end

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
h.done(): ProcResult[R] with Proc.Done
h.pause(): Result[Vm[R], ProcError] with Proc.Pause
h.resume(): Result[(), ProcError] with Proc.Resume
h.close(): SendResult with Proc.Close
h.snapshot_wait(fuel: Int): Result[Snapshot[R], SnapshotError]
  with Proc.SnapshotWait
```

When `M` is not `Never`, it also supports `h.send(message: M): SendResult with Proc.Send`. No `send(Any)` escape exists. `done()` blocks the holder operation until terminal and returns `Done(value)` or `Fault(fault)`. The value is transfer-checked. Pause requests synchronize at a guest instruction/operation boundary and return the underlying paused VM; resume moves it back to scheduler ownership.

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

Handles are sendable typed designators, so send rights can travel as data without erasing `M` or `R`. Attenuated send-only views are deferred.

### 18.6 Failure and parent lifetime

A proc crash is a value for its holder. Two blocked procs may deadlock; fuel, explicit timeout operations, or supervision converts that condition into policy-specific results or faults. A child table passes through the live parent's table. Parent death removes those pass-throughs and future requests fail closed.

### 18.7 Distribution

A spawn payload is already a code hash plus a typed tuple of sendable values, and operations/messages already cross one codec. A future remote scheduler may transport the same protocol without changing language semantics; distribution is not required in version 0.2.

## 19. Reflection

```lm
mirror = sys.reflect.mirror(obj)
```

`Mirror` returns frozen structural views: runtime class identity, declared fields and values, code/signature metadata, and permitted frame information. It never yields writable references into the inspected heap.

Reflection is an ordinary operation, appears in rows, and is table-gated. There is no dynamic invocation by string/symbol, no selector mutation, and no reflection API that bypasses field/method visibility or frozen boundaries.

---

## 20. Compiler, artifacts, and linker

### 20.1 Compiler object

```lm
src = """
class Greeter
  def greet(self, name: String) with Io.Print
    sys.io.print("Hello {name}!")
  end
end

do |name: String| with Io.Print
  Greeter().greet(name)
end
"""

env = CompileEnv().freeze()
result = sys.compiler.compile(src, env, CompileOptions())
```

`Compiler.Compile` is one deterministic operation whose ordinary result is `Result[Artifact, CompileErrors]`. It depends only on source bytes, compile-environment interfaces/hashes, options, compiler semantic hash, core-image hash, and operation/intrinsic ABI versions. Blocking it prevents runtime code minting.

### 20.2 Artifact API

A valid artifact exposes frozen metadata:

```lm
artifact.defs()
artifact.imports()
artifact.entry_type()
artifact.row()
artifact.hash()
artifact.bytecode()
```

An artifact may contain definitions, an entry, both, or neither. Definitions have independent semantic hashes; the module has a semantic hash; the exact byte container has a corruption hash.

### 20.3 Import slots and interfaces

An import slot includes name, full type/signature, effect row, mutability requirements where relevant, and optional exact code/class hash. An interface file is the canonical subset of artifact metadata needed by downstream compilation. It contains no executable source requirement and no ambient lookup rule.

### 20.4 Linking and typed entry values

```lm
bindings = LinkEnv()
assert(bindings.bind("Config", config.freeze()).is_ok())

case artifact.link(bindings.freeze())
in Ok(linked)
  greeter = linked.definition(
    "Greeter",
    expected: type_descriptor[Class[Greeter]]()
  )
  entry = linked.entry(
    expected: type_descriptor[(String) -> () with Io.Print]()
  )
  # greeter and entry are typed Result values
  ()
in Err(errors)
  # report the LinkErrors value
  ()
end
```

`LinkEnv.bind[T]` is the linker analogue of `CompileEnv.bind[T]`; both return typed ordinary errors and avoid a heterogeneous `Map[String,Any]`. `Artifact.link` returns `Result[LinkedModule,LinkErrors]`. `LinkedModule.entry` and `definition` require a typed `Type[T]` witness and return `Result[T,LinkError]`. Dynamic tools may request `DynValue` explicitly.

Linking is pure native work. Bindings are validated, local definitions materialized as frozen code/class values, and the pure entry expression evaluated and frozen. Loading or linking never mutates a process-global namespace.

### 20.5 Rows as verified theorems

The source checker proves each body row. Emitted typed bytecode carries enough metadata for a verifier to re-derive stack/type/call/perform consistency and ensure the claimed row contains all possible performs. Verification happens before an untrusted code hash enters the verified-code cache. Thereafter the hash pins the proof; runtime policy remains independent.

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
lm-value       Value, TypeId, ObjRef, scalar semantics
lm-bytecode    serialized and decoded bytecode structures
lm-verify      artifact and bytecode verifier
lm-heap        per-VM heap, object table, collector, native shapes
lm-graph       freeze/copy/digest/boundary/snapshot graph engine
lm-vm          frames, interpreter, pending performs, policy
lm-host        root operations and async completion adapters
lm-proc        scheduler and mailboxes
lm-compiler    scanner through artifact emission
lm-cli         build/run/test/inspect tools
```

`lm-vm` depends on no filesystem, clock, socket, command-line, or compiler frontend. `lm-host` receives validated values and designators, never an arbitrary mutable guest reference.

### 22.2 `Value`

On 64-bit hosts the baseline representation is a stable 16-byte tagged record rather than a Rust enum with unspecified layout or immediate NaN boxing:

```text
payload: u64
kind:    u32
aux:     u32
```

`payload` holds the full bits of `Int` or `Float`, a scalar value, or a 64-bit object/handle reference. `kind` selects unit, bool, integer, float, byte, char, heap object, code/class value, or native handle family. `aux` stores a small subtype/type slot or zero.

The 16-byte choice keeps full 64-bit integers and canonical float bits without pointer-width assumptions, makes snapshot/debug dumps straightforward, and confines tag decoding to generated helpers. A NaN-boxed or 8-byte mode is a future measured optimization, not a premise of the language.

### 22.3 Heap references and object headers

A heap reference is a packed `(slot: u32, generation: u32)`. Each VM owns an object-slot table whose live entry points into one of that VM's allocation pages. The generation catches stale internal handles during debug and fuzz builds. Guest references cannot name another VM's table; crossing a boundary always uses the codec.

The baseline object header is 16 bytes:

```text
class_slot: u32
byte_size:  u32
shape_slot: u16
flags:      u16    # frozen, marked, native, digest-cached, ...
gc_word:    u32
```

Ordinary instance payloads are contiguous `Value` fields in inherited-then-source order. Native objects use an immutable shape descriptor that supplies tracing, write locations, transfer, snapshot policy, snapshot encoding, digest, and destruction hooks.

The object table adds one indexed load to field access but keeps the rest of the runtime safe Rust, allows page movement or compaction later, makes ownership checks explicit, and avoids self-referential Rust structures. A direct-pointer mode is permitted only after benchmarks show that indirection dominates.

### 22.4 Allocation and collection

Each VM uses segmented bump pages for the fast allocation path and a per-size free list for swept objects. Collection is stop-the-VM mark/sweep because guest execution has one owner thread. Roots are module values, frames, locals/operands, the pending perform, native handles, and host-held temporary roots registered through scoped guards.

Marking is iterative and shares the native shape table with the graph engine. Sweeping increments dead slot generations and returns storage to page/free lists. There are no guest finalizers and therefore no collector reentrancy. The baseline collector is non-moving; a young copying generation may be added behind `ObjRef` without changing guest semantics.

Allocation is amortized O(1). A collection is O(live objects + heap pages). Heap limits are checked before committing page growth, so failure leaves the VM in a valid state.

### 22.5 Code, classes, and generic applications

Verified `Code`, class definitions, source maps, and core-image data are immutable `Arc`-backed host objects shared across VMs by semantic hash. A VM's load table maps those hashes to dense `u32` slots.

A decoded instruction is a fixed 16-byte record containing opcode/flags and up to three `u32` operands. Loading resolves constant, code, class, type, selector, field, intrinsic, and operation hashes once. The interpreter does not parse variable-length bytecode or hash names in its hot loop.

Class slots contain field offsets and a flattened selector-to-code table. Virtual dispatch is two indexed loads: runtime class slot, then selector slot. Generic applications share code and object layout but have distinct interned `TypeId` records containing argument type IDs for reflection and boundary validation.

### 22.6 Frames, locals, and operands

Frames are explicit fixed records, approximately 32–40 bytes in the reference build:

```text
code_slot
pc
local_base / local_count
operand_base / current_height
caller_frame
return_destination
source_cursor
```

Locals and operands occupy one VM-owned `Vec<Value>` arena; frames occupy a separate `Vec<Frame>`. A guest call checks limits, reserves arena space, writes a frame, and jumps. A return truncates the arenas and writes the result into the caller destination. Host recursion never represents a guest call, including native VM nesting.

The Rust loop avoids holding references into these vectors across allocation or an operation call. It works with indices and reloads after helpers that may grow storage or collect.

### 22.7 Interpreter loop and cost model

The interpreter is a generated `match` loop over decoded opcodes. `run`, `step`, and `drive` call the same loop with different stop modes. The loop keeps the current frame index and PC in local Rust variables and writes them back only at safepoints, calls, performs, faults, or exits.

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

The benchmark suite records dispatch, direct/virtual call, allocation, list traversal, map lookup, perform pass/block/mock, `drive` interception, nested-VM run, freeze, snapshot write/load, and proc send/receive. Regressions are compared against committed distributions, not a single machine-specific nanosecond threshold.

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

A `Map[K,V]` stores entries in insertion order plus an open-addressed index from hash to entry position. Replacing a value retains the original position; removal leaves/reuses an internal tombstone while public iteration remains dense and deterministic. Lookup is expected O(1), iteration O(n).

Map keys must be frozen and digestible at insertion. The runtime uses a process-keyed 64-bit lookup hash cached on immutable keys.

The process key is not guest state. Snapshots rebuild derived map indexes with the active process key.

Insertion order, equality, serialization, and digest do not depend on bucket order. Fuel charges use logical key size, not actual probe count.

This version accepts Bool, Int, Text, and Bytes map keys. String and Substring are the concrete Text key types.

String, Substring, and Bytes use immutable reference-counted byte storage. Each value stores one visible byte range.

A String contains valid UTF-8. A String also caches its scalar count and ASCII state.

A String can retain at most `max(4096, 2 * byte_len)` bytes of backing capacity. Construction and conversion enforce this limit.

A Substring is an explicit view. It can retain an allocation of any size until the view dies.

`Substring.to_string` and `Substring.compact` return a String with bounded retention. They copy only when the bound requires a copy.

A Bytes slice is also an explicit view. `Bytes.compact` copies the visible bytes into a new allocation.

Text and Bytes can share one physical byte allocation. A heap charges this allocation once for all its local views.

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
- external snapshot load: O(container bytes) once, producing trusted state.

Depth never consumes the Rust stack. Every mode has object, edge, byte, and work limits. Transfer and copy commit destination state only after preflight succeeds.

### 22.11 Policy representation

A policy table contains one dense exact-action vector indexed by operation slot and one dense group-action vector indexed by group slot. An action is a compact tagged record for block, pass, or mock. Mock records hold verified code, a sendable capture graph, and a work budget.

The default block action requires no allocation. Live edits replace one action under the table's synchronization primitive. A running VM reads an immutable action snapshot for the current perform; revocation affects the next lookup.

### 22.12 Procs, snapshot barriers, and asynchronous host work

A proc owns one `VmState` on one scheduler task. The baseline scheduler uses one deterministic FIFO ready queue. It runs each ready task for one fixed instruction quantum. Wake indexes name mailbox changes, terminal states, and host completions. A state change wakes only tasks in its matching index. The scheduler waits on the host only when no task is ready. One VM never executes concurrently.

Task keys and wake keys contain plain identifiers and counters. A future worker or process scheduler can use the same records.

Each proc has a stable opaque reference with a generation for dead-proc detection. Handle transfer preserves the reference.

A snapshot barrier pauses the reachable machines at safepoints, closes the set over the handles found in their state, and records one mailbox acceptance cut. It resumes the original world after success or failure. Host scheduler objects never enter snapshot bytes. Every control call on a machine serializes through the scheduler, so the barrier never races a holder.

Asynchronous host operations receive a single-use completion sink containing only controlled-VM ID, pending ordinal, and typed reply encoder. Completion queues never hold Rust references into the guest heap. Pause/resume transfers scheduler ownership at an interpreter safepoint.

Each VM also owns a host-side resource registry outside the guest heap. The registry records resource kind, scope identity, pending ordinal, and cleanup state. Snapshot preflight reads this registry and the guest graph to find live host attachments.

### 22.13 Unsafe-code policy

Unsafe Rust is confined to page allocation, raw byte-copy primitives, and optional C ABI shims. Every unsafe module states its invariants and has Miri/property tests. The verifier, byte decoder, snapshot loader, graph algorithms, policy table, interpreter state machine, and host dispatch are safe Rust. Fuzz builds enable generation checks and expensive heap validation after every instruction transition.

## 23. Standard host operations

Operation names, signatures, groups, hashes, and ABI versions come from the canonical operation manifest. The following is the minimum version 0.2 surface; ordinary error conditions use frozen result values.

### 23.1 I/O

```text
Io.Print       (String) -> ()
Io.Error       (String) -> ()
Io.ReadLine    () -> Result[Option[String], IoError]
Io.ReadBytes   (Int) -> Result[Bytes, IoError]
```

`Print` and `Error` accept text exactly as supplied. Line-ending policy belongs to wrappers. Reads may suspend.

### 23.2 File system

```text
Fs.Open        (String, OpenOptions) -> Result[FileHandle, FsError]
Fs.Read        (FileHandle, Int) -> Result[Bytes, FsError]
Fs.Write       (FileHandle, Bytes) -> Result[Int, FsError]
Fs.Seek        (FileHandle, SeekFrom) -> Result[Int, FsError]
Fs.Flush       (FileHandle) -> Result[(), FsError]
Fs.Close       (FileHandle) -> Result[(), FsError]
Fs.Stat        (String) -> Result[FileInfo, FsError]
Fs.ReadDir     (String) -> Result[[DirEntry], FsError]
Fs.CreateDir   (String) -> Result[(), FsError]
Fs.Remove      (String) -> Result[(), FsError]
Fs.Rename      (String, String) -> Result[(), FsError]
```

A live `FileHandle` names one resource entry and one service binding. The binding can belong to the root host or a driver. Every alias closes together. An open entry blocks snapshot creation. A closed handle remains typed machine state and restores as closed. The standard library never reopens a raw file handle silently. A later version may define a checkpointable file type with an explicit restore contract.

File operations can suspend their proc. The host adapter performs blocking platform work outside the scheduler thread.

### 23.3 Clock and randomness

```text
Clock.Now       () -> Int             # UTC nanoseconds from Unix epoch
Clock.Monotonic () -> Int             # host-monotonic nanoseconds
Clock.Sleep     (Int) -> Result[(), ClockError]

Rand.Bytes      (Int) -> Result[Bytes, RandError]
Rand.Int        (Int, Int) -> Result[Int, RandError]  # half-open [low, high)
```

Range validation is ordinary deterministic checking before host entropy use.

### 23.4 Networking

```text
Net.Resolve       (String, String) -> Result[[SocketAddress], NetError]
Net.Connect       (SocketAddress) -> Result[TcpHandle, NetError]
Net.Listen        (SocketAddress, Int) -> Result[ListenerHandle, NetError]
Net.Accept        (ListenerHandle) -> Result[Pair[TcpHandle, SocketAddress], NetError]
Net.Read          (TcpHandle, Int) -> Result[Bytes, NetError]
Net.Write         (TcpHandle, Bytes) -> Result[Int, NetError]
Net.Shutdown      (TcpHandle) -> Result[(), NetError]
Net.Close         (NetHandle) -> Result[(), NetError]
```

`NetHandle` is the sealed native resource parent of `TcpHandle` and `ListenerHandle`; it is unrelated to the `Any` top type. Live TCP streams and listeners are host attachments and block snapshot creation while open. TLS, DNS policy, proxies, and certificates are library/host extensions, not ambient behavior.

### 23.5 VM operations

Generic signatures below are manifest-level schemas instantiated by the compiler. `A` is an argument-tuple type, `T` is the machine's terminal result, `R` is one pending operation's reply type, and `Fn[A,T,e]` is manifest metanotation for a callable with argument tuple `A`, result `T`, and row `e`.

```text
Vm.New                   () -> EmptyVm
Vm.FromFn[A,T,e]     (EmptyVm, Fn[A,T,e], control A) -> Vm[T]
Vm.FromArtifact[A,T]     (EmptyVm, LinkedEntry[A,T], control A) -> Vm[T]
Vm.Step[T]               (Vm[T]) -> StepEvent[T]
Vm.Run[T]                (Vm[T]) -> RunResult[T]
Vm.Drive[T]              (Vm[T]) -> DriveEvent[T]
Vm.DriveWait[T]          (Vm[T]) -> Wait[DriveEvent[T]]
Vm.Answer[T,A,R]         (Vm[T], PendingCall[A,R], R) -> ()
Vm.Reject[T]             (Vm[T], Request, Fault) -> ()
Vm.Dispatch[T]           (Vm[T], Request) -> ()
Vm.Stack[T]              (Vm[T]) -> [FrameView]
Vm.Table[T]              (Vm[T]) -> PolicyTable
Vm.Handles[T]            (Vm[T]) -> [ResourceHandle]
Vm.Resource[T]           (Vm[T], FileHandle) -> ResourceHandle
Vm.ServeFile[T]           (Vm[T], PendingCall[(String, OpenOptions),
                           Result[FileHandle, FsError]]) -> ResourceHandle
Vm.ResourceIsOpen        (ResourceHandle) -> Bool
Vm.ResourceClose         (ResourceHandle) -> Bool
Vm.ResourceKind          (ResourceHandle) -> String
Vm.ResourceSame          (ResourceHandle, ResourceHandle) -> Bool
Vm.SetLimits[T]          (Vm[T], Limits) -> ()
Vm.AddFuel[T]            (Vm[T], Int) -> ()
Vm.SnapshotHeld[T]       (Vm[T])
                          -> Result[Snapshot[T], SnapshotError]
Vm.SnapshotSelf          ()
                          -> Result[SnapshotImage, SnapshotError]
Vm.LoadSnapshot          (Bytes)
                          -> Result[SnapshotImage, SnapshotError]
Vm.Restore[T]            (EmptyVm, Snapshot[T])
                          -> Result[Vm[T], RestoreError]
```

The held and receiverless forms use separate exact operation identities because their honest result types differ, while sharing one serializer/host implementation family. `SnapshotImage.cast_result(type_descriptor[T]())` checks the hidden result `TypeId` and returns `Result[Snapshot[T],SnapshotTypeError]`; typed restore accepts only the checked view.

`Vm.Handles` returns controls for the live resources in the controlled
machine world. A resource control stays with its holder.

`Vm.ResourceSame` matches two controls only while their shared entry
is live. A closed control never matches.

### 23.6 Proc operations

A proc handle carries both mailbox and terminal result types:

```text
Proc.Run[R]         (Vm[R]) -> Handle[Never,R]
Proc.Spawn[M,R,A]   (Class[Proc[M]], control A) -> Handle[M,R]
Proc.Send[M,R]      (Handle[M,R], M) -> SendResult
Proc.Close[M,R]     (Handle[M,R]) -> SendResult
Proc.Recv[M]        (proc self) -> Recv[M]
Proc.RecvWait[M]    (proc self) -> Wait[Recv[M]]
Proc.Done[M,R]      (Handle[M,R]) -> ProcResult[R]
Proc.Pause[M,R]     (Handle[M,R]) -> Result[Vm[R], ProcError]
Proc.Resume[M,R]    (Handle[M,R]) -> Result[(), ProcError]
Proc.SnapshotWait[M,R] (Handle[M,R], Int)
                       -> Result[Snapshot[R], SnapshotError]
```

A proc with no mailbox uses `Never` as `M`; such a handle has no callable `send` method.

`Proc.SnapshotWait` first tries an immediate capture. It parks the caller only when a live resource blocks capture.

Fuel counts target-world instructions. Host completion time does not consume fuel.

### 23.7 Wait operations

```text
Wait.Wait[T]          (Wait[T]) -> T
Wait.Choose[A,B]      (Wait[A], Wait[B]) -> Wait[Choice[A,B]]
Wait.Cancel[T]        (Wait[T]) -> Bool
```

Wait tokens are holder-local and one-shot. Section 7.4 defines select syntax.

`docs/specs/sidecar/waits.md` defines readiness, drive leases, and scheduler indexes.

### 23.8 Compiler and reflection

```text
Compiler.Compile  (String, CompileEnv, CompileOptions)
                  -> Result[Artifact, CompileErrors]
Reflect.Mirror[T] (T) -> Mirror[T]
```

`Mirror[T]` returns detached `ValueView` children and typed metadata; it never widens the inspected value to `Any` or exposes a live guest reference.

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

class Pair[A, B]
  first: A
  second: B

  def init(mut self, first: A, second: B)
    self.first = first
    self.second = second
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

It also contains `StepEvent`, `RunResult`, `DriveEvent`, `Recv`, `ProcResult`, `SendResult`, `PendingCall`, `SnapshotImage`, `SnapshotError`, `RestoreError`, portable operation error enums, `OpenOptions`, `SeekFrom`, `FileInfo`, `SocketAddress`, `Duration`, `Instant`, `CompileOptions`, and related ABI records.

`List`, `Map`, `Text`, its concrete classes, `Char`, and `Bytes` are native core classes in the pinned image.

Builders, type descriptors, faults, VMs, snapshots, procs, file leases, and resource handles are also native core classes.

The image seals their complete method tables. Some bodies use intrinsics, while other bodies use ordinary verified bytecode.

### 24.2 Prelude

The prelude introduces only names used in nearly every module:

```text
(), Never, Bool, Int, Float, Byte, Text, String, Substring, Char, Bytes
List, Map, Option, Some, None, Result, Ok, Err
Ordering, Pair, Range
identity, assert, assert_message
```

`Any` remains an explicit primitive type, while `DynValue`, VM/proc/compiler/reflection types, file/network types, and all effectful wrappers require explicit qualification or bindings. The prelude contains no I/O function and grants no operation.

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
```

There is no postfix propagation operator in version 0.2; explicit `case` remains the universal control form.

### 24.4 Native `List[T]`

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
reserve(mut self, additional: Int) -> ()
truncate(mut self, length: Int) -> ()
clear(mut self) -> ()
copy(self) -> List[T]
slice(self, start: Int, length: Int) -> List[T]
concat(self, other: List[T]) -> List[T]
extend(mut self, other: List[T]) -> ()
reverse(mut self) -> ()
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
freeze(self) -> List[T]
```

Faulting index methods use `IndexOutOfBounds`; allocation failure obeys heap limits. Higher-order methods call the closure in list order and stop immediately on fault.

### 24.5 Maps and sets

`Map[K,V]` requires frozen digestible keys at insertion and preserves insertion order:

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
keys(self) -> List[K]
values(self) -> List[V]
entries(self) -> List[Pair[K,V]]
each[e](self, f: (K,V) -> () with e) -> () with e
map_values[U,e](self, f: (K,V) -> U with e) -> Map[K,U] with e
retain[e](mut self, f: (K,V) -> Bool with e) -> () with e
freeze(self) -> Map[K,V]
```

For a text key type, `has`, `get`, `at`, and indexing accept Text. Insertion still requires K.

`std/set` defines `Set[T]` as an ordinary sealed class over `Map[T,()]`, with `add`, `remove`, `has`, `union`, `intersection`, `difference`, `is_subset`, and ordered `values`. A deque is not core; `std/deque` may be added as a package without affecting language semantics.

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
strip_prefix(prefix: Text) -> Option[Substring]
strip_suffix(suffix: Text) -> Option[Substring]
to_lower_ascii() -> String
to_upper_ascii() -> String
replace(needle: Text, replacement: Text) -> String
parse_int(radix: Int) -> Result[Int,ParseIntError]
__eq__(other: Text) -> Bool
__ne__(other: Text) -> Bool
__lt__(other: Text) -> Bool
__le__(other: Text) -> Bool
__gt__(other: Text) -> Bool
__ge__(other: Text) -> Bool
```

`at`, `slice`, `find`, `each`, and `map` use Unicode scalar positions. `at` returns None for an invalid position.

`slice` reports `IndexError.OutOfBounds` for an invalid scalar range. A successful slice shares storage.

`slice_bytes` reports `Utf8Error.OutOfBounds` for an invalid range. It reports `Utf8Error.InvalidBoundary` when a boundary splits one scalar.

`find_bytes` supports byte-oriented parsers. It avoids the scalar-position conversion that `find` requires.

One rule sets the result type of every extraction method. A method that narrows its receiver gives a `Substring` and copies nothing. A method that builds new content gives a `String`. So `split`, `lines`, `trim`, and the two `strip_` methods give views, and `to_lower_ascii`, `to_upper_ascii`, and `replace` give durable values.

Every method above is total, under the rule of section 12.1. `split` with an empty separator matches at every scalar boundary and gives one empty piece at each end. `replace` with an empty needle inserts at every scalar boundary. `parse_int` reports `ParseIntError.BadRadix` for a radix outside 2 to 36, because a radix reaches a program from data.

`lines` accepts a line feed with or without a leading carriage return. A final line feed ends the last line and adds no empty piece.

`split_once`, `strip_prefix`, and `strip_suffix` give a valid piece by construction, so they report absence through `Option` and never report a boundary error. A parser that uses them handles no failure that its own input cannot cause.

Interpolation accepts any `Text`. A `Substring` appends to the builder without a copy.

The implementation uses one lazy sparse scalar index for each text root. It records every 64th scalar position.

The first indexed operation can build this index in O(n) time. A later scalar boundary lookup scans at most 63 scalars.

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
to_string() -> String
compact() -> String
```

Both methods enforce the String retention bound. They can return shared storage when that storage already meets the bound.

Char has this surface.

```text
codepoint() -> Int
utf8_len() -> Int
is_ascii() -> Bool
__eq__(other: Char) -> Bool
__ne__(other: Char) -> Bool
__lt__(other: Char) -> Bool
__le__(other: Char) -> Bool
__gt__(other: Char) -> Bool
__ge__(other: Char) -> Bool
```

`Text.at` allocates no Char object. Its successful path allocates only the `Option.Some` result object.

Core defines `Utf8Error` and `IndexError`. Float parsing remains deferred.

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
utf8() -> Result[String,Utf8Error]
utf8_view() -> Result[Substring,Utf8Error]
text() -> String
__add__(other: Bytes) -> Bytes
__eq__(other: Bytes) -> Bool
__ne__(other: Bytes) -> Bool
__lt__(other: Bytes) -> Bool
__le__(other: Bytes) -> Bool
__gt__(other: Bytes) -> Bool
__ge__(other: Bytes) -> Bool
```

`at` faults with `IndexOutOfBounds` for an invalid index. `get` returns `None` for an invalid index.

`slice` returns `Err(IndexError.OutOfBounds)` for an invalid range. A successful slice shares immutable storage.

`compact` copies the visible bytes into a new allocation. Use it to release a large retained allocation.

`find` returns a byte offset. `hex` uses lowercase hexadecimal text.

`utf8` reports invalid encoding through its result. It returns a bounded String.

`utf8_view` reports invalid encoding through its result. It returns a shared Substring without a content copy.

`text` is a compatibility conversion that faults with `BadCast`. It returns a bounded String after successful validation.

`+`, `==`, `!=`, and the four ordering operators use the paired-underscore Bytes hook methods. The ordering hooks carry the unsigned byte rule of section 6.4.

The final nominal builders have the following surface.

```text
StringBuilder.append(text: Text) -> StringBuilder
StringBuilder.push_char(value: Char) -> StringBuilder
StringBuilder.len() -> Int
StringBuilder.byte_len() -> Int
StringBuilder.clear() -> StringBuilder
StringBuilder.build() -> String
StringBuilder.finish() -> String

ByteBuffer.append(byte: Int) -> ByteBuffer
ByteBuffer.extend(bytes: Bytes) -> ByteBuffer
ByteBuffer.reserve(additional: Int) -> ByteBuffer
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

File and network operations exchange Bytes. An in-process host boundary can share immutable Bytes storage.

`ByteBuffer.build` and `ByteBuffer.finish` never perform a text conversion.

Interpolation lowers to `std/fmt` append operations. The core scalar/string/bytes/digest/fault set has pinned formatting implementations. Other types format only through explicit functions because version 0.2 has no traits.

### 24.7 Numeric and range utilities

`std/math` supplies type-specific pure integer/float `min`, `max`, and `clamp`; `abs`; checked/wrapping/saturating integer operations; `gcd`; `pow_int`; and float rounding, roots, exponentials, logarithms, and trigonometric functions with specified binary64 behavior. With no traits or overloads, these functions are explicitly typed rather than pretending to be universally generic.

The distributed floating algorithms are version-pinned and bit-reproducible across conforming targets; implementations do not delegate observable semantics to an unconstrained platform `libm`. Correctly rounded basic arithmetic remains as specified in section 2.4, while transcendental accuracy bounds and special cases are published with the module version.

`Range(start, stop, step)` rejects zero step. `Range.each`, `to_list`, `contains`, and `len` use checked arithmetic. There is no special `for` syntax in version 0.2; `range.each` or a `while` loop is explicit.

### 24.8 Value utilities

```lm
freeze[T](value: T): T
digest[T](value: T): Digest
is_frozen[T](value: T): Bool
deep_equal[T](a: T, b: T): Bool
```

`deep_equal` requires frozen digestible graphs. It uses digests as a fast reject, then cycle-safe structural comparison; digest equality alone is not proof. `std/value` also exposes bounded `inspect(value): ValueView` and canonical encode/decode for data types explicitly marked by the core shape table.

### 24.9 Paths, I/O, and files

`std/path` is pure. `Path` normalizes separators lexically, joins components, extracts parent/name/extension, and never consults the host filesystem.

`std/io` contains thin wrappers:

```lm
print(text: String) with Io.Print
println(text: String) with Io.Print
eprint(text: String) with Io.Error
eprintln(text: String) with Io.Error
read_line(): Result[Option[String], IoError] with Io.ReadLine
read_all(max_bytes: Int): Result[Bytes, IoError] with Io.ReadBytes
```

`std/fs` makes scoped access the standard file path:

```lm
files.with_open(path, options) { |file|
  file.read_all(max_bytes: 1_000_000)
}
```

Conceptually:

```text
with_open[R,e](
  path: Path,
  options: OpenOptions,
  body: (FileLease) -> R with e
) -> Result[R,FsError] with Fs.Open, Fs.Close, e
```

`FileLease` is a scoped designator. It offers `read`, `read_exact`, `read_all`, `read_text`, `write`, `write_all`, `flush`, and `seek`. It has no public `close` method. An open failure returns `Err` without calling the body. A normal body return closes the lease before returning. A close failure returns `Err` instead of the body value.

`with_open` never flattens the body result. A body that returns `Result` gives the caller a nested `Result`. The caller matches both layers with nested patterns (9.2).

A body fault terminates the machine normally. The host-side resource registry closes the lease during VM cleanup. Cleanup invokes no guest callback and does not replace the original terminal fault.

The advanced API remains explicit:

```text
open_handle(path, options)
  -> Result[FileHandle,FsError] with Fs.Open
```

`FileHandle` has explicit read, write, seek, flush, and close methods. A live `FileHandle` is a host attachment and blocks snapshot creation. A closed handle remains in machine state and returns `FsError.Closed`. A host extension may define a distinct checkpointable file type with an explicit restore contract in a later version.

Top-level helpers include `read`, `read_text`, `write`, `write_text`, `stat`, `read_dir`, `create_dir`, `remove`, and `rename`. They use scoped handles internally and retain the exact underlying rows.

There are no finalizers. Scoped cleanup is host-managed. Raw handle ownership remains explicit.

### 24.10 Time, randomness, networking, and process inputs

`std/time` defines frozen `Duration` and `Instant`, checked conversion helpers, `now`, `monotonic`, and `sleep`. `std/random` provides `bytes`, half-open integer ranges, Boolean selection, list `choose`, and Fisher-Yates `shuffle`, with exact `Rand` rows.

`std/net` wraps resolve/connect/listen/accept/read/write/shutdown/close for TCP. A live TCP handle is a host attachment and blocks snapshot creation. TLS, HTTP, and DNS policy are separate packages because they introduce substantial policy and dependency choices.

Command-line arguments are passed as the root entry tuple by the CLI rather than read ambiently. `Process.EnvGet` and `Process.CurrentDir` are optional explicit host operations; `std/process` wraps them when the host ABI enables that group.

### 24.11 JSON

A small `std/json` module is part of the distribution because it makes file/network examples real without adding runtime machinery:

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

`parse` returns `Result[Json,JsonError]`; `stringify` is pure and deterministic. Parsing is iterative, depth/byte limited, and preserves object insertion order. JSON is standard-library code over `String`, `List`, and `Map`, not a VM intrinsic.

### 24.12 Typed VM utilities

The standard library does not reintroduce an `Answer(Any)` decision enum or a variadic helper that would require type packs. Exact-operation elimination is already ordinary and small enough to package in user code:

```lm
def answer_print[T](
  vm: Vm[T],
  request: Request,
  mut captured: [String]
): Bool with Vm.Answer
  case request
  in Call(Io.Print, call, (text,))
    captured.push(text)
    vm.answer(call, ())
    true
  in _
    false
  end
end
```

A policy can define one such function per operation whose behavior it owns. This remains fully type-checked by the ordinary `Call` pattern rule and does not add variadic generics, tuple spreading, or a third dependent native rule. `std/vm` instead provides fuel/limit builders, terminal-result mapping, snapshot-image file helpers, and bounded request logging through `ValueView`.

### 24.13 Procs

`std/proc` supplies explicit supervision, bounded send loops, close/drain, cancellation-message conventions, and result aggregation. It does not add shared memory or hide proc effects. `Handle[M,R]` preserves message and result types through `send`, `done`, `pause`, `resume`, transfer, and snapshot restore.

### 24.14 Compiler, reflection, and testing

`std/compiler.compile(source)` supplies an empty `CompileEnv` and default options. Builders expose typed `bind[T]` and link helpers; dynamic plugin tooling must use `DynValue` explicitly.

`std/reflect` formats mirrors and frame views without dynamic invocation. `std/test` represents each test body as a frozen descriptor carrying its exact function type, row, code hash, and captures. The runner executes every case in a child VM, configures an explicit table, records `Done`/`Fault`, and may use `drive` for deterministic operation transcripts.

The compiler test harness has UI diagnostics, compile-pass, run-pass, run-fail, bytecode-verifier, artifact/snapshot corruption, conformance, fuzz-regression, and benchmark suites.

### 24.15 Deliberate omissions

The minimal standard library does not include an iterator trait hierarchy, async/await, regex engine, TLS/HTTP stack, database client, package registry client, GUI, locale framework, or automatic serialization derivation. Without traits, eager collection methods are clearer than a nominal iterator protocol; richer facilities remain ordinary packages.

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

A package root is one source module in version 0.2. Supporting modules compile independently and are supplied as explicit compile/link bindings to the root. Surface `import` syntax is deferred; dependency edges live in the manifest/build graph and artifact import slots.

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

`check` parses/resolves/types/checks without producing an installed executable. `build` writes canonical artifact/interface files atomically. `run` links the module and accepts exactly three entry shapes:

- a frozen non-callable value, which is reported directly and accepts no command-line arguments;
- `() -> T with e`, invoked with the empty argument tuple;
- `([String]) -> T with e`, invoked with one frozen list containing the strings after `--`.

Other callable signatures are valid for embedding but `lm run` reports a signature error. For callable entries, `run` constructs a root VM, applies an explicit host policy profile, and invokes the entry. It does not infer grants solely from the artifact row; a profile must choose them.

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
12. one-time snapshot load verification followed by trusted resume without repeated whole-image checks;
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

This EBNF-like grammar is normative with the clarifications below. `NL` denotes one or more valid statement separators.

```ebnf
module          = opt_separators, { definition, separators },
                  [ expression, opt_separators ], EOF ;

definition      = class_decl | enum_decl | function_decl ;

class_decl      = [ "final" ], "class", IDENT, [ type_params ], [ "<", type ], separators,
                  { ( field_decl | method_decl ), separators },
                  "end" ;

field_decl      = IDENT, ":", type, [ "=", expression ] ;

method_decl     = "def", IDENT, [ generic_params ], "(", method_parameters, ")",
                  [ ":", type ], [ effect_clause ], separators,
                  block, "end" ;

method_parameters = self_parameter, [ ",", parameters ] ;
self_parameter  = [ "mut" ], "self" ;

function_decl   = "def", IDENT, [ generic_params ], "(", [ parameters ], ")",
                  [ ":", type ], [ effect_clause ], separators,
                  block, "end" ;

enum_decl       = "enum", IDENT, [ type_params ], separators,
                  { enum_arm, separators },
                  { method_decl, separators },
                  "end" ;

enum_arm        = IDENT, [ "(", [ field_parameters ], ")" ] ;

type_params     = "[", IDENT, { ",", IDENT }, "]" ;
generic_params  = "[", generic_param, { ",", generic_param }, "]" ;
generic_param   = IDENT | "effect", IDENT ;

parameters      = parameter, { ",", parameter } ;
parameter       = [ "mut" ], IDENT, ":", type ;
field_parameters= field_parameter, { ",", field_parameter } ;
field_parameter = IDENT, ":", type ;

effect_clause   = "with", row_item, { ",", row_item } ;
row_item        = qualified_name | IDENT ;

type            = primary_type, [ function_type_tail ] ;
primary_type    = qualified_name, [ type_args ]
                | "[", type, "]"
                | "{", type, ":", type, "}"
                | "(", [ type_list ], ")"
                | "Op", "[", row_item, ",", type, "]" ;
function_type_tail = "->", type, [ effect_clause ] ;
type_args       = "[", type, { ",", type }, "]" ;
type_list       = type, { ",", type } ;
qualified_name  = IDENT, { ".", IDENT } ;

block           = { expression, separators } ;
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
comparison      = additive, { ( "<" | "<=" | ">" | ">=" ), additive } ;
additive        = multiplicative, { ( "+" | "-" ), multiplicative } ;
multiplicative  = unary, { ( "*" | "/" | "%" ), unary } ;
unary           = ( "not" | "-" ), unary | postfix ;

postfix         = primary,
                  { generic_apply_suffix | call_suffix | field_suffix | index_suffix },
                  [ trailing_closure ] ;
generic_apply_suffix = "[", type, { ",", type }, "]" ;
call_suffix     = "(", [ arguments ], ")" ;
field_suffix    = ".", IDENT ;
index_suffix    = "[", expression, "]" ;
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
                | loop_expr
                | case_expr
                | return_expr
                | "break"
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

if_expr         = "if", expression, separators, block,
                  { "elsif", expression, separators, block },
                  [ "else", separators, block ], "end" ;

while_expr      = "while", expression, separators, block, "end" ;
loop_expr       = "loop", "do", separators, block, "end" ;

case_expr       = "case", expression, separators,
                  case_arm, { separators, case_arm }, separators, "end" ;
case_arm        = "in", pattern,
                  ( "then", expression | separators, block ) ;

pattern         = "_"
                | IDENT
                | literal
                | tuple_pattern
                | qualified_name, "(", [ pattern, { ",", pattern } ], ")" ;
tuple_pattern   = "(", pattern, ( ",", [ pattern, { ",", pattern } ] | { ",", pattern } ), ")" ;

return_expr     = "return", [ expression ] ;

literal         = INT | FLOAT | CHAR | STRING | BYTES
                | "true" | "false" | "()" ;
```

### A.1 Clarifications

- `method_parameters` always starts with untyped source `self` or `mut self`; its containing class supplies the type. There are no source static methods.
- Classes and enums declare only type parameters. Top-level functions and methods may additionally declare `effect` parameters.
- `()` is unit. `(T,)` and `(T,U)` are tuple types; the same parenthesized list followed by `->` is a function parameter list. A one-element tuple requires the trailing comma.
- `do || ... end` and `{ || ... }` are empty-parameter closures. A closure may put exactly one body expression on the header line; a multi-expression body starts after a separator.
- A left brace followed by a pipe starts a brace closure. Other braces start a map literal. `{}` is an empty map.
- A trailing closure is valid only after a postfix chain that contains a call suffix. It becomes the final call argument. It must start on the same line as that chain, and no suffix may follow it.
- A bracket suffix is generic application only where static resolution permits it and normally precedes a call; otherwise it is indexing. Ambiguous source is rejected.
- A postfix assignment target must be a writable field or index, not an arbitrary call result.
- Enum arms must precede enum methods. A zero-field constructor such as `None` is recognized from expected/scrutinee context; another bare name is a binding pattern.
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
(String) -> () with Io.Print
(T) -> U with e
Op[Clock.Now, () -> Int]
```

Canonical artifact rows expand groups to exact ABI operation identities and sort by canonical hash. Diagnostics may retain authored group spelling. Type parameters are numbered by declaration order in semantic encodings, independent of source variable spelling.

---

# Appendix C: Complete typed manual-driving example

```lm
def supervise(
  program: () -> String with Io.Print, Clock.Now
): RunResult[String] with Vm, Io.Print
  vm = sys.vm.Vm().from_fn(program, args: ())
  captured: [String] = []

  loop do
    case vm.drive()
    in Asked(q)
      case q
      in Call(Io.Print, call, (text,))
        captured.push(text)
        vm.answer(call, ())
      in Call(Clock.Now, call, ())
        vm.answer(call, 1_700_000_000)
      in _
        vm.reject(q, Fault.denied("the supervisor permits print and time only"))
      end
    in Done(value)
      sys.io.print("captured {captured.len()} writes\n")
      return Done(value)
    in Fault(fault)
      return Fault(fault)
    end
  end
end
```

No `Any` appears in the reply path. Matching the exact operation recovers its argument tuple and reply type; the runtime still validates machine identity, ordinal, and one-time use. The child receives neither `Io.Print` nor `Clock.Now` through its table. The holder's summary print is its own effect and needs authority in the holder's table.

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
- JIT/tiered execution, although verified interpreter state is designed to permit later deoptimization;
- guarantees against microarchitectural or process-wide timing side channels.
