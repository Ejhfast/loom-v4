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

Source syntax, static checking, artifact and snapshot validity, canonical hashing, operation identities, boundary behavior, policy behavior, and fault behavior are normative. Collector strategy, host thread-pool shape, internal caching, and physical sharing of frozen storage are not observable.

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

Inside a package, one module holds the program entry: `src/main.lm`. Every other module must end without a trailing expression. The file tree under `src/` is the module tree, and the module path across packages carries the package name of the manifest (`docs/specs/packages.md`).

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

The `use` declaration is the source-level surface of this rule. A `use` line binds one dotted path to a short name. A `use` of another module compiles to a named import slot, and the build tool fulfills it. `use` never grants authority and never changes an effect row. The package layout, the manifest, and the resolution roots are defined in `docs/specs/packages.md`.

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

The linker compares two classes on QualifiedKey and StructuralHash (3.7, 8.6). The table is exhaustive:

| QualifiedKey | StructuralHash | Result |
| --- | --- | --- |
| same | same | merge into one class |
| same | different | reject: conflicting implementations |
| different | same | keep distinct |
| different | different | keep distinct |

The second row rejects two implementation versions of one qualified name. The rejection names both providers and the rebuild. The third row keeps `mathlib.Vec2` and `app.Point` distinct although their structures are equal.

Two functions with one StructuralHash are one function in the merged program. A function carries no QualifiedKey, so content decides. The core image every module carries therefore becomes one core, and a core value keeps its class across a module boundary.

### 3.7 Definition and module identity

Four identities answer four questions about one definition: **QualifiedKey**, **StructuralHash**, **InterfaceHash**, and **VerificationHash**. Section 8.6 states them for a class. Each consumer names the one it needs, and no consumer reads a value another consumer owns.

A **QualifiedKey** is the fully qualified declaration path of a class, for example `mathlib.geometry.Point`. The package name of the manifest supplies the root, never the dependency key. Two classes are the same nominal class when their QualifiedKey values are equal. A function carries no QualifiedKey: a function is identified by content alone.

A **StructuralHash** covers canonical bytecode and constants, full signature and effect row, referenced definition identities, import requirements and pinned hashes, compiler ABI version, and intrinsic semantics version. It never covers the definition's own name.

**The naming rule.** A declaration name never enters a structural definition hash. A name may enter an interface hash, a namespace hash, or a qualified key. The shorter claim "no name in any hash" is wrong: an interface hash must contain names, because an importer agrees with a named API.

A reference to a class inside canonical bytecode names that class by QualifiedKey, never by the structural identity of the referenced class. Two signatures that name two structurally identical classes therefore receive different structural hashes. This rule stays inside the naming rule, because it covers a referenced nominal identity and never the declaration's own name.

Canonical bytecode is a dedicated identity encoding, not the loading encoding. It replaces every module-global index — function, class, type, string, application, and selector — with content identity, a qualified key, inline content, or structural encoding. Definition hashes therefore do not depend on definition order in the source or on pool interning order.

For mutually recursive definitions, the compiler finds strongly connected components. A component labels its members by structural refinement, and no name and no source order enters the rule:

1. The first label of a member is the hash of the member bytes, with every reference inside the component replaced by one fixed placeholder.
2. The next label of a member is the hash of its current label plus the current labels of the members it references. References keep their position order inside the member; a member never sorts its own references, because `f(g(x))` and `g(f(x))` differ.
3. Refinement stops as soon as the label partition stops refining. The round count is capped at the member count.
4. The final label is the StructuralHash of the member. The component hash is the hash of the sorted final labels.

The set of components is a property of the graph, so the emission order of the component walk is invisible in every hash. A rename therefore moves no definition hash, inside a cyclic component or outside one.

Structural refinement cannot always give each member a unique label. Two mutually recursive definitions with equal bodies stay symmetric through every round. This is a property of graph automorphism, not a defect of the rule: no order-invariant rule separates such members without an external identity. Symmetric members share one StructuralHash, and their QualifiedKey values keep them distinct wherever distinctness is observable.

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
| `Char` | Unicode scalar value |
| `(T,)`, `(T, U)`, ... | Fixed-arity structural tuples |
| `(A, B) -> R with e` | Function type with effect row |
| `Op[id, (A, B) -> R]` | Identity-indexed operation type |

**Native core classes** have ordinary nominal identities but runtime-supplied storage or methods:

| Type | Meaning |
|---|---|
| `String` | Immutable UTF-8 text |
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

**Core-image nominal types** are ordinary source definitions with pinned hashes. The minimum set includes `Option`, `Result`, `Ordering`, `Pair`, `Range`, `RunResult`, `StepEvent`, `DriveEvent`, `Recv`, `ProcResult`, portable operation error enums, and the typed request-token declarations used by VM control.

**Host and holder types** such as `EmptyVm`, `Vm[T]`, typed `Snapshot[T]`, erased-but-contained `SnapshotImage`, `Handle[M,T]`, `PolicyTable`, file handles, and socket handles are native nominal classes with explicit boundary rules.

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

An artifact carries a **core role table**: one class slot per stable core role, for example `Option`, `Option.Some`, and `Option.None`. The compiler fills the table, the linker relocates it, and the verifier proves the kind, the generic arity, the parent slot, and the exact field layout of every filled slot. A rule that needs a core family, such as the return type of `Request.as_call`, reads a slot. It reads no name and no hash, so a rename changes nothing the verifier reads, and an artifact with no source resolves its core from its own bytes. A family whose parent slot is filled must fill every arm slot.

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

`()` is unit, not a zero-field heap tuple. Tuple elements are covariant and addressed only by compile-time position. Tuples are used for lightweight returns, map entries, and typed operation argument packs. Their maximum portable arity is 16; larger records should be classes.

### 5.6 `Any`, `DynValue`, and deliberate dynamic boundaries

Every ordinary value can widen to `Any`, but normal generic APIs must use a type parameter rather than `Any`. In particular, list algorithms, `freeze`, `digest`, `deep_equal`, VM results, proc messages, compile environments, and operation replies preserve their caller's type.

`Any` is a primitive name but prelude and standard APIs do not return it merely for convenience. It should appear only in code intentionally doing dynamic type tests. Narrowing is explicit:

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

The checker normalizes source function syntax to the structural form `Fn[A,R,e]`, where `A` is the fixed argument tuple, `R` the result, and `e` the row. `Fn` is ABI/type-checker metanotation rather than an additional source type name; it lets native APIs such as `EmptyVm.from_object` use ordinary first-order generics instead of a variadic or dependent typing rule. Function parameters are contravariant, results covariant, and effects covariant by set inclusion.

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
| overrides preserve call and row substitutability | termination, fuel use, heap use, or asymptotic cost |
| every possible perform is contained in the declared row | absence of faults, deadlock, host failure, or timing side channels |
| typed manual replies match the selected operation at the source level | a stale or cross-VM request token; the runtime still validates it |
| emitted bytecode has typed stack/local states | correctness of externally supplied bytes until the verifier accepts them |

### 5.14 Checker and verifier construction

The compiler represents primitive, nominal, tuple, function, operation, and type-variable forms as an interned immutable type DAG. Names resolve to dense definition IDs before type checking. Typed HIR stores resolved field, selector, class, operation, and intrinsic IDs; later phases never repeat textual lookup.

Each callable is lowered to a control-flow graph. Definite assignment, return analysis, and stack-shape planning are forward dataflow problems with finite states. Effect rows are sorted small sets of operation IDs with an inline representation for common rows and an interned representation for larger rows.

The source checker and bytecode verifier are independent implementations over different representations. The verifier reconstructs local/operand type states at block entries, checks joins, calls, fields, intrinsics, performs, and claimed rows, and rejects malformed external code before it enters the verified-code cache. Source types are erased from the execution hot path to dense slots after verification; ordinary instruction dispatch performs no general subtype lookup.

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
vm.from_object(program, args: ("Ada",))
```

There is no overload resolution.

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

A closure is a sealed function object containing code identity and captures. Omitting `with` means empty row.

### 6.3 Fields, `self`, and `super`

`receiver.field` is statically resolved. `self` exists only in methods. A mutating method declares `mut self`. `super.method(args)` calls the immediate superclass implementation with the same receiver and a compile-time selector.

### 6.4 Arithmetic, comparison, and equality

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

Strongest to weakest: postfix call/field/index; unary `not`/`-`; multiplicative; additive; ordering; equality/`is`/`as`; `and`; `or`; assignment. Assignment is right-associative; other binary operators are left-associative.

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

`break` exits the nearest loop; `continue` begins its next iteration. Loops have type `()` unless every reachable exit is a `return` or fault, in which case they may type as `Never`.

### 7.3 Case

```lm
case value
in Some(v) then use(v)
in None
  fallback()
end
```

Arms are tested in source order. An arm may use `then` with one expression or a newline body. Cases over enums and `Bool` are checked for exhaustiveness; cases over other types require a wildcard or binding arm. Duplicate unreachable arms are compile errors.

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

Ordinary sealed definitions may be subclassed. A subclass inherits fields and methods and may add both. An override must keep parameter types and `mut` markers, may narrow the result, and may narrow but not widen the row.

Constructor signatures are not inherited. A subclass initializer handles inherited required fields and may call `super.init(...)` exactly once before reading fields initialized by it.

A call selector is fixed at compile time; the runtime class selects the sealed implementation. Computed selectors are not representable.

### 8.6 Class identity

A class value is frozen. Four identities answer four questions about one class. Each consumer names the one it needs.

- **QualifiedKey** — the nominal identity. The value is the fully qualified declaration path, for example `mathlib.geometry.Point`. Two classes are the same nominal class when their QualifiedKey values are equal. The linker uses this value. The type checker never compares it, because it works on class indices inside one module.
- **StructuralHash** — the name-free content identity. It covers the kind, the generic arity, the parent identity, the normalized field layout, the selector set, the method signatures, the implementing function identities, and the native intrinsic identifier where applicable. It never covers the class's own name. Section 3.7 states how a reference to another class enters it.
- **InterfaceHash** — the named public API identity of one export. It covers the export name, the kind, the full structural signature with class references by qualified name, the field defaults, the arm names, and the initializer signature. An import slot pins it. A rename moves it.
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

Frozenness is checked at transfer boundaries, proc send/spawn, closure transfer, digest/cache-key creation, mock installation, and relevant snapshot boundaries. Failure faults; there is no silent mutable deep copy.

`digest()` computes BLAKE3-256 over a canonical frozen graph encoding. The encoder traverses deterministic field/index/insertion order, assigns object ordinals at first encounter, uses back-references for later encounters, encodes code/classes by hash, and includes sharing and cycles. Float encoding normalizes both signed zeros to positive zero and all NaNs to the canonical NaN, matching language equality. Live resources and nondigestible descriptors cause `BoundaryViolation`.

One graph walker must define reachability and ordering for freeze, verification, copy, transfer, digest, and snapshot traversal.

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

File-not-found, end-of-input, parse failure, connection refusal, mailbox closure, and similar expected conditions belong in ordinary result types.

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
| `BoundaryViolation` | codec or descriptor rule violated |
| `UnsendableValue` | mutable/nonsendable graph crossed a boundary |
| `MalformedArtifact` | invalid artifact/bytecode |
| `MalformedSnapshot` | invalid snapshot/machine image |
| `LinkMismatch` | link binding incompatible |
| `MissingCode` | required code hash unavailable |
| `InertDescriptor` | unrebound resource was exercised |
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

- `pass`: forward to parent table/root host;
- `block`: fault the controlled guest with `PolicyDenied`;
- `mock`: use a pure handler with the exact operation signature;
- `clear`: remove the exact/group entry.

`block` and `clear` accept any `PolicyTarget`. `pass` follows the identity-preserving static rule in section 11.5. `mock` requires an exact operation descriptor known to the checker.

Lookup order is exact operation, group, then default block. Groups are flat. Insertion order has no effect.

### 13.3 Mock execution

A mock handler has verified code, empty row, and frozen captures. Installation boundary-copies it into table-owned storage. It has no table, cannot suspend, and receives a deterministic work limit. Its heap result must be frozen/sendable. A mock fault, budget exhaustion, or invalid result faults the controlled guest. The guest sees the whole perform as one instruction.

### 13.4 Pass chains and revocation

A child `pass` consults the live parent table associated at creation/launch, eventually reaching an embedding-host registry. Parent edits affect future child performs. If the parent/root binding is gone or the root implementation is ungranted, the operation fails closed with `PolicyDenied`.

Editing a table while a proc runs affects future lookups; it does not retroactively cancel a host operation already accepted unless that operation's own semantics expose cancellation.

### 13.5 Manual policy

`drive()` stops before lookup. The holder may answer/reject directly or invoke `dispatch()` to apply this table. Tables remain the only automatic policy mechanism.

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
vm = empty.from_object(program, args: ("Ada",))
```

`Vm.New` creates an `EmptyVm` with a fresh heap, frames, limits, and default-deny table. `from_object[A,R,e](self, program: Fn[A,R,e], args: A) -> Vm[R]` is an ordinary generic native declaration over the normalized structural function form: it checks the supplied tuple, transfers code/captures/arguments through the boundary codec, creates the initial frame without executing, transitions the native receiver out of the empty state, and returns `Vm[R]`. An aliased stale `EmptyVm` handle is harmless: any second load attempt is rejected by the runtime state check.

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

`WaitView` is inspection-only because automatic policy has already accepted and dispatched that operation. `Request` appears only on the manual path before policy lookup.

Events are frozen boundary views. Before terminal success is published, the value crosses transfer mode. A mutable or nonsendable result converts the controlled machine to `Fault(UnsendableValue)`.

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

`drive()` is valid from `ready`, `waiting`, and `asked`. From `asked` it returns the same semantic pending request with a freshly materialized holder token, without executing or consuming fuel; this is required after restoring an asked snapshot or losing a prior token. From `waiting`, an already dispatched wait completes before interception resumes. Otherwise it runs until the next `PERFORM` has recorded its operation, typed argument slots, reply type, destination, and continuation PC, then stops before table lookup:

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

### 14.8 Typed request matching

`Request` is an opaque holder-local token for one pending perform. Its erased inspection surface contains no `Any`:

```lm
q.op(): Operation
q.ordinal(): Int
q.args_view(): [ValueView]   # returned list and views are frozen
q.reply_type(): TypeView
```

To read arguments or answer, the holder matches the request against an exact typed operation object:

```lm
case q.as_call(Io.Print)
in Some(call)
  (text,) = call.args()       # (String,)
  captured.push(text)
  vm.answer(call, ())         # reply is statically ()
in None
  # not Io.Print
end
```

`Request.as_call(op)` has a narrow compiler-known type rule. Its argument is an exact `Operation` descriptor known to the checker, such as `Io.Print`. If the manifest signature of that descriptor is `(A...) -> R`, the result is `Option[PendingCall[(A...), R]]`. The callable `sys` member is not used here: matching is descriptor work, and the compiler supplies the typed signature from the manifest. `PendingCall[A,R]` exposes:

```text
args(self) -> A
reply_type(self) -> Type[R]
request(self) -> Request
```

This is existential elimination at an operation-identity test, not general dependent typing. The checker instantiates the token type from the static `Op` type; bytecode carries the expected dense operation/type slots; and the runtime returns `Some` only when the pending exact operation slot matches. ABI initialization has already verified that the slot owns that argument/reply signature, so the success path needs no general dynamic cast. The only other such native rule in version 0.2 is effect charging for `PolicyTable.pass`.

### 14.9 Continuation methods

While the controlled VM is `asked`:

```lm
vm.answer(call, value)       # PendingCall[A,R], value: R
vm.reject(q, fault)          # Request, Fault
vm.dispatch(q)               # Request
```

`answer` checks that the token belongs to this VM, names the current ordinal, and remains pending; boundary-encodes the statically typed reply; validates its runtime `TypeId`; installs it; clears the request; and enters `ready`. A mismatch or stale/cross-VM token faults the controlled machine with `BadOperationReply` or the caller with `InvalidRequestToken` as appropriate, never corrupting a frame.

`reject` installs the supplied frozen fault and enters `faulted`. `dispatch` applies the table and enters `ready`, `waiting`, or `faulted`. Tokens need not be linear in the source type system because the VM validates single use.

These methods are invalid in other states. Calling `step` or `run` while `asked`, or loading a nonempty VM, faults the caller with `InvalidVmState` without changing the controlled machine. Repeating `drive` while `asked` is the explicit token-recovery operation described above. Terminal execution calls return the stored terminal event idempotently.

### 14.10 Reentrancy, inspection, and ownership

A control method on a currently `running` VM, or from guest code executing inside that same VM, faults. Execution and inspection methods also fault while `proc_owned`. `table()` and edits through an already obtained table handle are the explicit synchronized exception, permitting live revocation.

`stack()` is valid only while not running or proc-owned and returns deep-frozen `FrameView` values: function identity/name, PC, source location if present, locals as `ValueView`, and a bounded operand summary. No live guest reference escapes.

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

The perform hot path is: write pending fields, load exact table action, fall back to group action, then block/pass/mock dispatch. `drive` takes the same path only until the pending fields are complete. No row lookup, string lookup, heap continuation, or public API transition occurs per guest instruction.

## 15. Nested VMs

Nesting is ordinary composition of functions that use `Vm`:

```lm
def f2(): Int with Vm
  case sys.vm.Vm().from_object(do || 21 end, args: ()).run()
  in Done(v)  then v
  in Fault(_) then 0
  end
end

def f1(e: () -> Int with Vm): Int with Vm
  expr = do || with Vm
    x = e()
    x + x
  end

  vm = sys.vm.Vm().from_object(expr, args: ())
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
3. **snapshot:** canonical machine serialization;
4. **inspection:** detached frozen views.

A control envelope is holder-owned native metadata rather than a guest collection. Every member installed into guest, link, or compiler state is independently encoded and checked. Thus `args: ("Ada",)` is legal without making mutable guest lists generally sendable.

Each host-operation parameter and result position has an ABI mode. The default `value` mode supplies an immutable/frozen boundary value. `transfer` moves a sendable value into another independently controlled heap (for example proc messages). `designator` accepts only the exact native handle kind named by the signature. `inspect` permits a transient read-only graph walk of the performing VM without making that graph sendable; the host receives a bounded inspection cursor/view and may not retain a guest pointer. `control` is reserved for holder-facing VM/compiler/link/snapshot envelopes. These modes are fixed in the operation manifest and cannot be chosen dynamically by guest code.

### 16.2 Sendable values

Transfer mode accepts:

- unit, booleans, numbers, characters, strings, bytes, digests;
- frozen graphs of sendable fields/elements;
- class/code/function values by hash plus frozen captures;
- operation/group/type descriptors;
- proc handles and snapshots;
- host value types explicitly marked sendable by the ABI.

It rejects mutable graphs, full VM and policy-table handles, live host callbacks, and live OS resources. Rejection faults with `UnsendableValue` or `BoundaryViolation`; no implicit mutable deep copy occurs.

### 16.3 Code and class transfer

Code and classes cross by semantic hash. The receiving code store must already contain verified bytes for that hash or obtain them through an embedding-host code resolver. Missing code yields `MissingCode`. Code bytes are never accepted under a mismatched hash.

A closure transfers code identity and its frozen capture graph. A capture that includes a holder-local handle or mutable graph makes the closure unsendable.

### 16.4 Handles and resources

A full `Vm`, `PolicyTable`, or raw host registry handle is holder-local. A proc `Handle` is a live sendable designator because send rights are intentionally transferable. Resource handles such as files and sockets serialize only as inert descriptors in snapshots; ordinary transfer rejects them unless an operation's ABI explicitly defines a safe transfer.

Trying to use an inert descriptor faults with `InertDescriptor` until a trusted restorer explicitly rebinds it.

### 16.5 Inspection

Everything read out of another heap—stack frames, mirrors, pending request arguments, artifact metadata—returns as an immutable native value or deep-frozen detached graph. Inspection never returns a writable guest reference.

---

## 17. Snapshots

### 17.1 Shapes

```lm
snap = vm.snapshot()
vm2 = sys.vm.Vm().restore(snap)
```

The surface spellings lower to distinct exact identities `Vm.SnapshotHeld` and `Vm.SnapshotSelf`. `vm.snapshot()` serializes a paused held `Vm[T]` and returns `Snapshot[T]`. `sys.vm.snapshot()` is receiverless and snapshots the machine performing that operation; because an independently compiled function cannot know the enclosing root machine's terminal type, it returns `SnapshotImage`, a frozen trusted image carrying a hidden result `TypeId` rather than `Any` or `DynValue`. External bytes first pass through `sys.vm.load_snapshot(bytes)`, which performs the one-time load verification and also returns `Result[SnapshotImage,SnapshotError]`.

```text
SnapshotImage.result_type(self) -> TypeView
SnapshotImage.cast_result[T](self, expected: Type[T])
  -> Result[Snapshot[T], SnapshotTypeError]
SnapshotImage.to_bytes(self) -> Bytes
Snapshot[T].to_bytes(self) -> Bytes
```

`restore(snap: Snapshot[T])` is valid only on an `EmptyVm`, installs the already trusted snapshot state, gives the receiver a fresh default-deny table, and returns `Vm[T]`. `restore_with` is the explicit form for rebinding supported inert descriptors or pending waits. `RestoreBindings` is a holder-side typed builder with ABI-generated descriptor-specific `bind` methods; it is not a `Map[String,Any]`, and a live handle can be bound only to the exact inert descriptor kind it implements. Serialization of either trusted snapshot view is deterministic.

A snapshot contains format/ABI versions, code and class manifest, type table, heap graph, roots, frame/operand arenas, current PCs, limits/fuel, state, pending request when present, inert host descriptors, and a container hash.

It excludes policy table, root grants, live host callbacks, proc scheduler ownership, host thread state, and live OS resources.

### 17.2 Paused and pending snapshots

A snapshot taken between instructions restores between those instructions. A snapshot in `asked` restores in `asked`, preserving operation, frozen arguments, reply type, destination, continuation PC, and ordinal. The holder calls `drive()` once to obtain a fresh `Request` token for that preserved ordinal; no guest instruction runs.

A snapshot in `waiting` cannot preserve a live host callback. It restores with the pending operation and an inert completion designator. The restorer must rebind the operation to a supported host continuation, reject it, or provide a validated answer.

A receiverless self-snapshot is captured while `Vm.SnapshotSelf` itself is pending. The restored copy resumes when the restorer answers that pending request with the snapshot descriptor/value it chooses. Execution then continues on the line after the call.

### 17.3 Fresh authority and multi-shot restore

The table is never serialized. Every restored VM receives a fresh default-deny table and fresh root relationship chosen by the restorer. Restored resources are inert. Snapshot bytes are immutable, so one snapshot may produce multiple diverging machines.

### 17.4 Verification only on load

An in-process snapshot created from trusted verified state may use a trusted restore path. Bytes entering from storage, network, or another process are verified once at load.

Load verification checks:

- magic, version, canonical integers, section bounds, counts, and container hash;
- resource limits before allocation;
- code hashes, code availability, and verified-artifact status;
- type/class slots and object layouts;
- object references, frozen flags, lists/maps, cycles, and roots;
- frame function slots, PCs at instruction boundaries, locals, operands, and return destinations;
- pending-request identity, argument/reply types, continuation destination, and legal state;
- descriptor encoding and inertness of resource records.

Successful loading constructs trusted `VmState`. Execution, answering, and later snapshotting do not repeat structural verification. The write barrier and normal dynamic checks remain active because they enforce runtime semantics, not snapshot trust.

### 17.5 Canonical form

The canonical snapshot representation uses deterministic section order, little-endian fixed fields where specified, canonical LEB128 counts/integers, object ordinals assigned by root traversal, and BLAKE3-256 domain-separated hashes. Debug/source-map data may be present but does not affect guest semantic identity.

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
vm = sys.vm.Vm().from_object(program, args: ("Ada",))
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

A `Handle[M,R]` supports:

```lm
h.done(): ProcResult[R] with Proc.Done
h.pause(): Result[Vm[R], ProcError] with Proc.Pause
h.resume(): Result[(), ProcError] with Proc.Resume
h.close(): SendResult with Proc.Close
```

When `M` is not `Never`, it also supports `h.send(message: M): SendResult with Proc.Send`. No `send(Any)` escape exists. `done()` blocks the holder operation until terminal and returns `Done(value)` or `Fault(fault)`. The value is transfer-checked. Pause requests synchronize at a guest instruction/operation boundary and return the underlying paused VM; resume moves it back to scheduler ownership.

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

Accepted messages are delivered FIFO by host acceptance order. `close` prevents later acceptance but preserves queued messages; `Closed` arrives after the queue drains. A send to a closed/dead peer returns a dedicated ordinary `SendResult`, unless malformed or mutable data faults the sender at its boundary.

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

Ordinary instance payloads are contiguous `Value` fields in inherited-then-source order. Native objects use an immutable shape descriptor that supplies tracing, write locations, transfer, snapshot, digest, and destruction hooks.

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
state: none | asked | waiting | inert_wait
host_completion_token      # host-only; never serialized live
```

While executing normally, arguments remain in verified operand slots. `drive` exits after the record is complete and before policy lookup. `Request.as_call` checks the exact operation slot and returns a holder token carrying VM identity, ordinal, argument tuple type, and reply type; `PendingCall.args()` boundary-encodes that tuple lazily. `answer` validates the token again before installing a reply.

The snapshot form serializes semantic fields, never a live completion token. A waiting operation restores as an inert wait requiring explicit rebinding or rejection.

### 22.9 `List`, `Map`, strings, and bytes

A `List[T]` object stores length, capacity, and a reference to a contiguous `Value` buffer. `push` is amortized O(1); indexed access is O(1); insertion/removal is O(n). Frozen lists keep the same representation and reject writes through the common frozen barrier.

A `Map[K,V]` stores entries in insertion order plus an open-addressed index from hash to entry position. Replacing a value retains the original position; removal leaves/reuses an internal tombstone while public iteration remains dense and deterministic. Lookup is expected O(1), iteration O(n).

Map keys must be frozen and digestible at insertion. The runtime uses a keyed 64-bit lookup hash cached on immutable keys; insertion order, equality, serialization, and digest do not depend on bucket order. The hash seed is VM configuration recorded in snapshots. Fuel charges use logical key/byte size rather than actual probe count.

`String` and `Bytes` are immutable objects backed by reference-counted byte slices, permitting zero-copy sharing across trusted in-process holders. String construction validates UTF-8 once. `StringBuilder` and `ByteBuffer` use growable private buffers and produce immutable outputs by transferring or copying their allocation.

### 22.10 Graph engine

One non-recursive engine drives mark, deep freeze, frozen verification, boundary transfer, structural copy, canonical digest, snapshot encoding, and detached inspection. Mode-specific visitors share shape traversal and an identity table but have separate result state; this avoids one giant branch-heavy inner loop while preserving one definition of graph reachability and field order.

- `freeze`: O(V + E), sets bits only after all reachable objects validate;
- transfer/copy: O(V + E + bytes), preserves cycles and sharing;
- digest: O(V + E + bytes), assigns deterministic traversal ordinals and domain-separates backreferences;
- snapshot write: O(reachable encoded bytes);
- external snapshot load: O(container bytes) once, producing trusted state.

Depth never consumes the Rust stack. Every mode has object, edge, byte, and work limits.

### 22.11 Policy representation

A policy table contains one dense exact-action vector indexed by operation slot and one dense group-action vector indexed by group slot. An action is a compact tagged record for block, pass, or mock. Mock records hold verified code, frozen captures, and a work budget.

The default block action requires no allocation. Live edits replace one action under the table's synchronization primitive. A running VM reads an immutable action snapshot for the current perform; revocation affects the next lookup.

### 22.12 Procs and asynchronous host work

A proc owns one `VmState` on one scheduler task. The baseline scheduler uses Rust threads for isolation clarity, with a bounded mailbox and a completion channel for host operations. An implementation may multiplex many proc tasks, but one VM is never executed concurrently.

Asynchronous host operations receive a single-use completion sink containing only controlled-VM ID, pending ordinal, and typed reply encoder. Completion queues never hold Rust references into the guest heap. Pause/resume transfers scheduler ownership at an interpreter safepoint.

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

A `FileHandle` is live only in the host binding that created it. Snapshot form is inert.

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

`NetHandle` is the sealed native resource parent of `TcpHandle` and `ListenerHandle`; it is unrelated to the `Any` top type. TLS, DNS policy, proxies, and certificates are library/host extensions, not ambient behavior.

### 23.5 VM operations

Generic signatures below are manifest-level schemas instantiated by the compiler. `A` is an argument-tuple type, `T` is the machine's terminal result, `R` is one pending operation's reply type, and `Fn[A,T,e]` is manifest metanotation for a callable with argument tuple `A`, result `T`, and row `e`.

```text
Vm.New                   () -> EmptyVm
Vm.FromObject[A,T,e]     (EmptyVm, Fn[A,T,e], control A) -> Vm[T]
Vm.FromArtifact[A,T]     (EmptyVm, LinkedEntry[A,T], control A) -> Vm[T]
Vm.Step[T]               (Vm[T]) -> StepEvent[T]
Vm.Run[T]                (Vm[T]) -> RunResult[T]
Vm.Drive[T]              (Vm[T]) -> DriveEvent[T]
Vm.Answer[T,A,R]         (Vm[T], PendingCall[A,R], R) -> ()
Vm.Reject[T]             (Vm[T], Request, Fault) -> ()
Vm.Dispatch[T]           (Vm[T], Request) -> ()
Vm.Stack[T]              (Vm[T]) -> [FrameView]
Vm.Table[T]              (Vm[T]) -> PolicyTable
Vm.SetLimits[T]          (Vm[T], Limits) -> ()
Vm.AddFuel[T]            (Vm[T], Int) -> ()
Vm.SnapshotHeld[T]       (Vm[T]) -> Snapshot[T]
Vm.SnapshotSelf          () -> SnapshotImage
Vm.LoadSnapshot          (Bytes) -> Result[SnapshotImage, SnapshotError]
Vm.Restore[T]            (EmptyVm, Snapshot[T]) -> Vm[T]
Vm.RestoreWith[T]        (EmptyVm, Snapshot[T], RestoreBindings) -> Vm[T]
```

The held and receiverless forms use separate exact operation identities because their honest result types differ, while sharing one serializer/host implementation family. `SnapshotImage.cast_result(type_descriptor[T]())` checks the hidden result `TypeId` and returns `Result[Snapshot[T],SnapshotTypeError]`; typed restore accepts only the checked view.

### 23.6 Proc operations

A proc handle carries both mailbox and terminal result types:

```text
Proc.Run[M,R]       (Vm[R], Type[M]) -> Handle[M,R]
Proc.Spawn[M,R,A]   (Class[Proc[M]], control A) -> Handle[M,R]
Proc.Send[M,R]      (Handle[M,R], M) -> SendResult
Proc.Close[M,R]     (Handle[M,R]) -> SendResult
Proc.Recv[M]        (proc self) -> Recv[M]
Proc.Done[M,R]      (Handle[M,R]) -> ProcResult[R]
Proc.Pause[M,R]     (Handle[M,R]) -> Result[Vm[R], ProcError]
Proc.Resume[M,R]    (Handle[M,R]) -> Result[(), ProcError]
```

A proc with no mailbox uses `Never` as `M`; such a handle has no callable `send` method.

### 23.7 Compiler and reflection

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

It also contains `StepEvent`, `RunResult`, `DriveEvent`, `Recv`, `ProcResult`, `SendResult`, `PendingCall`, `SnapshotImage`, portable operation error enums, `OpenOptions`, `SeekFrom`, `FileInfo`, `SocketAddress`, `Duration`, `Instant`, `CompileOptions`, and related ABI records.

`List`, `Map`, `String`, `Bytes`, builders, type descriptors, faults, empty/typed VM, snapshot, proc, and resource handles are native core classes declared in the same pinned image. Their complete method tables are sealed there; some bodies are intrinsics and some are ordinary verified bytecode attached during the core build.

### 24.2 Prelude

The prelude introduces only names used in nearly every module:

```text
(), Never, Bool, Int, Float, Byte, Char, String, Bytes
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

`std/set` defines `Set[T]` as an ordinary sealed class over `Map[T,()]`, with `add`, `remove`, `has`, `union`, `intersection`, `difference`, `is_subset`, and ordered `values`. A deque is not core; `std/deque` may be added as a package without affecting language semantics.

### 24.6 Strings, bytes, builders, and formatting

`String` is immutable valid UTF-8:

```text
byte_len / char_count / is_empty
concat(other: String) -> String
starts_with / ends_with / contains
find(needle: String) -> Option[Int]          # byte offset
slice_bytes(start,length) -> Result[String,Utf8Error]
slice_chars(start,length) -> Result[String,IndexError]
chars() -> List[Char]
bytes() -> Bytes
split(separator: String) -> List[String]
lines() -> List[String]
trim / trim_start / trim_end
replace(needle,replacement) -> String
to_lower_ascii / to_upper_ascii
parse_int(radix: Int) -> Result[Int,ParseIntError]
parse_float() -> Result[Float,ParseFloatError]
```

`Bytes` supports `len`, `get`, `at`, `slice`, `concat`, `starts_with`, `find`, `hex`, and `utf8`. `StringBuilder` supports `push_char`, `push_string`, `clear`, `len`, and `finish`; `ByteBuffer` supports `push`, `extend`, `reserve`, `clear`, and `finish`.

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

`std/fs` defines explicit `File` and directory helpers. `File` offers `read`, `read_exact`, `read_all`, `read_text`, `write`, `write_all`, `flush`, `seek`, and `close`, each retaining the exact underlying `Fs.*` row. Top-level helpers include `open`, `read`, `read_text`, `write`, `write_text`, `stat`, `read_dir`, `create_dir`, `remove`, and `rename`.

There are no finalizers. Code closes explicitly; the host reclaims leaked resources only when the VM dies, without guest callbacks.

### 24.10 Time, randomness, networking, and process inputs

`std/time` defines frozen `Duration` and `Instant`, checked conversion helpers, `now`, `monotonic`, and `sleep`. `std/random` provides `bytes`, half-open integer ranges, Boolean selection, list `choose`, and Fisher-Yates `shuffle`, with exact `Rand` rows.

`std/net` wraps resolve/connect/listen/accept/read/write/shutdown/close for TCP. TLS, HTTP, and DNS policy are separate packages because they introduce substantial policy and dependency choices.

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
  case request.as_call(Io.Print)
  in Some(call)
    (text,) = call.args()
    captured.push(text)
    vm.answer(call, ())
    true
  in None
    false
  end
end
```

A policy can define one such function per operation whose behavior it owns. This remains fully type-checked by the ordinary `Request.as_call` rule and does not add variadic generics, tuple spreading, or a third dependent native rule. `std/vm` instead provides fuel/limit builders, terminal-result mapping, snapshot-image file helpers, and bounded request logging through `ValueView`.

### 24.13 Procs

`std/proc` supplies explicit supervision, bounded send loops, close/drain, cancellation-message conventions, and result aggregation. It does not add shared memory or hide proc effects. `Handle[M,R]` preserves message and result types through `send`, `done`, `pause`, and `resume`.

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

Native `List`, `Map`, `String`, `Bytes`, builder, graph, numeric, and type-test operations are intrinsics or kernel instructions, not host operations. Their faults are deterministic language faults.

### 25.5 Native classes and graph shapes

Every native heap class registers one immutable shape descriptor describing traced references, frozen-write locations, canonical field order, snapshot encoding, boundary policy, and digestibility. A native class that cannot participate consistently must be holder-local or inert and cannot masquerade as an ordinary sendable object.

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

A host admits code by one or more explicit rules: known semantic hash, successful bytecode verification plus accepted imports/row bound, signature policy, or an application-specific audit record. Verification proves structural/type/effect claims; it does not decide whether requested operations should be granted.

### 27.3 Limits

Before allocating from untrusted artifact/snapshot/boundary input, implementations check byte counts, object counts, nesting/work limits, frame/operand maxima, string/collection sizes, and checked arithmetic. Runtime limits cover fuel, heap, stack, pending boundary bytes, snapshot bytes, mailbox bytes/messages, mock work, and host-specific quotas.

A malformed external input returns a load/verify error or faults the controlled boundary; it must not crash the host, overflow arithmetic, allocate beyond declared limits, or create unchecked code/state.

### 27.4 Revocation and fail-closed behavior

Policy edits apply to future performs. Parent/root disappearance, missing code, inert resources, invalid state, and host registry mismatches fail closed. Snapshot restore creates no authority. A blocked operation is a machine fault, not a value visible to code inside that machine.

### 27.5 No ambient recovery hooks

There are no finalizers, signal handlers, exception hooks, destructor callbacks, dynamic loader callbacks, or implicit module initializers that execute guest code outside normal verified calls/operations. Host cleanup cannot reenter a dead guest.

### 27.6 Side channels and host policy

This specification defines logical authority and isolation, not constant-time execution or denial of timing/memory-pressure side channels between VMs sharing a process. Hosts requiring stronger separation run VMs in separate processes or hardware isolation while retaining the same artifact/operation protocol.

---

## 28. Conformance suite

A conforming implementation passes tests for at least:

1. identical semantic hashes for canonical equivalent compiler output;
2. rejection of malformed, truncated, overlong, and noncanonical artifact encodings;
3. verifier detection of stack, local, type, call, field, intrinsic, perform, and row inconsistencies;
4. class sealing, initialization safety, override row narrowing, and enum exhaustiveness;
5. exact/group/default table precedence, pure mocks, pass-chain authority, and live revocation;
6. `run`, one-instruction `step`, `drive`, `answer`, `reject`, `dispatch`, waiting, and illegal-state transitions;
7. no host-stack growth proportional to guest call depth;
8. nested-VM default denial and transitive grant charging;
9. deep freeze, cycles, sharing, map order, digest stability, and frozen write barriers;
10. boundary rejection of mutable/holder-local values and inert descriptor behavior;
11. snapshot round trips at every instruction boundary, in `asked`, and with supported waiting rebinding;
12. one-time snapshot load verification followed by trusted resume without repeated whole-image checks;
13. proc isolation, FIFO acceptance, close/drain, pause/resume, dead-peer results, and terminal transfer checks;
14. reflection and stack views containing no writable guest references;
15. deterministic diagnostics, compile environments, interface/build keys, and byte-for-byte reproducible artifacts;
16. fuel, heap, frame, operand, boundary, mailbox, mock, and snapshot limits;
17. fuzzing of scanners, parsers, artifact/snapshot readers, verifier, boundary codec, graph walker, and interpreter state transitions;
18. cross-platform ABI vectors for hashes, numbers, UTF-8, floats, manifests, artifacts, snapshots, and value digests.

---

# Appendix A: Surface grammar

This EBNF-like grammar is normative with the clarifications below. `NL` denotes one or more valid statement separators.

```ebnf
module          = opt_separators, { definition, separators },
                  [ expression, opt_separators ], EOF ;

definition      = class_decl | enum_decl | function_decl ;

class_decl      = "class", IDENT, [ type_params ], [ "<", type ], separators,
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
                  { generic_apply_suffix | call_suffix | field_suffix | index_suffix } ;
generic_apply_suffix = "[", type, { ",", type }, "]" ;
call_suffix     = "(", [ arguments ], ")" ;
field_suffix    = ".", IDENT ;
index_suffix    = "[", expression, "]" ;

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

list_literal   = "[", [ expression, { ",", expression } ], "]" ;
map_literal     = "{", [ map_entry, { ",", map_entry } ], "}" ;
map_entry       = expression, ":", expression ;

closure         = "do", "|", [ parameters ], "|", [ ":", type ],
                  [ effect_clause ],
                  ( separators, block | expression ), "end" ;

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
                | qualified_name, "(", [ pattern, { ",", pattern } ], ")" ;

return_expr     = "return", [ expression ] ;

literal         = INT | FLOAT | CHAR | STRING | BYTES
                | "true" | "false" | "()" ;
```

### A.1 Clarifications

- `method_parameters` always starts with untyped source `self` or `mut self`; its containing class supplies the type. There are no source static methods.
- Classes and enums declare only type parameters. Top-level functions and methods may additionally declare `effect` parameters.
- `()` is unit. `(T,)` and `(T,U)` are tuple types; the same parenthesized list followed by `->` is a function parameter list. A one-element tuple requires the trailing comma.
- `do || ... end` is an empty-parameter closure. A closure may put exactly one body expression on the header line; a multi-expression body starts after a separator.
- A bracket suffix is generic application only where static resolution permits it and normally precedes a call; otherwise it is indexing. Ambiguous source is rejected.
- A postfix assignment target must be a writable field or index, not an arbitrary call result.
- Enum arms must precede enum methods. A zero-field constructor such as `None` is recognized from expected/scrutinee context; another bare name is a binding pattern.
- Class fields and methods may be interleaved. Field layout follows inherited fields then local field source order.
- The built-in static typing rule for `PolicyTable.pass` is specified in section 11.5 and is not expressible fully in the ordinary grammar/type language.

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
  vm = sys.vm.Vm().from_object(program, args: ())
  captured: [String] = []

  loop do
    case vm.drive()
    in Asked(q)
      case q.as_call(Io.Print)
      in Some(call)
        (text,) = call.args()
        captured.push(text)
        vm.answer(call, ())
      in None
        case q.as_call(Clock.Now)
        in Some(call)
          vm.answer(call, 1_700_000_000)
        in None
          vm.reject(q, policy_denied_fault(q.op()))
        end
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
- shared-memory guest threads;
- stateful policy-table callbacks;
- ambient imports, mutable globals, or effectful module initialization;
- automatic resource closing or serialization of live OS resources;
- record/replay layers, reply channels, attenuated handles, or remote scheduling;
- JIT/tiered execution, although verified interpreter state is designed to permit later deoptimization;
- guarantees against microarchitectural or process-wide timing side channels.
