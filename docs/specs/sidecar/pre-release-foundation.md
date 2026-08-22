# Pre-release Language and Host Foundation

Status: stages 1 through 7 implemented.

This sidecar defines the first public release foundation.

It covers language reliability, host operations, and program inputs.

Self-hosting is outside this work.

Interpreter and compiler optimization are outside this work.

Each stage records its build, test, and execution costs.

## 1. Release contract

The first public release makes four user-visible promises.

1. Guest code and malformed external data never crash the host process.
2. A program can consume host inputs and perform normal application work.
3. Every operation remains visible to effect checking and runtime policy.
4. Core protocols use interfaces where user types can participate.

The release can omit later convenience features.

It cannot turn ordinary platform failures into host faults.

It cannot accept program arguments and then discard them.

## 2. Terms

| Term | Meaning |
|---|---|
| standard bundle | The pinned groups and operations distributed with Loom |
| ABI bundle | The immutable operation definitions used by one program |
| bundle digest | The canonical identity of one ABI bundle |
| host failure | An ordinary platform failure returned as a typed value |
| host defect | A host implementation violates its declared operation contract |
| program arguments | The strings after the command-line `--` separator |

## 3. Error boundary

### 3.1 Ordinary errors

An expected platform failure returns the operation's declared error value.

Examples include a closed pipe, a missing file, denied environment access, and invalid UTF-8.

The host adapter must encode these failures as `Err` values.

The adapter must not report them through `HostStart::Failed`.

### 3.2 Host defects

`HostStart::Failed` reports a host implementation defect.

Examples include a wrong argument shape and an impossible completion token.

The VM converts a host defect into `Fault(HostFault)`.

The fault records the operation and source call chain.

### 3.3 Output failure

Byte output returns `Result[Int, IoError]`.

The integer gives the accepted byte count.

A closed pipe returns `Err(IoError.BrokenPipe)`.

Text helpers use byte output and retain its error.

The CLI must not panic while it reports a closed diagnostic stream.

The CLI treats a closed report stream as a completed report.

### 3.4 Snapshot errors

`SnapshotError` remains a visible enum.

Its constructor is its stable programmatic category.

It implements the common display interface.

`RunSnapshot[T].to_bytes()` returns `Result[Bytes, SnapshotError]`.

`VmSnapshot.to_bytes()` returns `Result[Bytes, SnapshotError]`.

The encoded bytes use the canonical snapshot container.

The encoding includes the bundle digest.

## 4. Deliberate machine failure

Core adds these functions.

```lm
panic(message: String): Never
assert(condition: Bool)
assert_message(condition: Bool, message: String)
```

`panic` creates `Fault(UserPanic)`.

The fault carries the supplied message and current source call chain.

`assert` and `assert_message` create `Fault(AssertionFailed)`.

`Option.expect(message)` returns its payload or calls `panic`.

`Result.expect(message)` returns its payload or calls `panic`.

The first release does not add `unwrap`.

## 5. Short case arms

A short case arm accepts any single statement.

This includes `return`, `break`, and `continue`.

The parser stores the statement in the normal arm body.

No control statement becomes a general expression.

Normal loop-placement checks still apply.

## 6. Diagnostic repairs

An interface name in a value-type position produces a specific diagnostic.

The diagnostic states that interfaces can appear as bounds.

A bare associated type inside an interface produces a specific diagnostic.

The diagnostic suggests `Self.Name`.

Diagnostics must use the source name for each effect variable.

The CLI accepts `--help` and `-h` with a successful status.

The CLI accepts `--version` and `-V` with a successful status.

An unknown command remains an error.

## 7. ABI identity

### 7.1 Purpose

The current standard operation manifest becomes the standard ABI bundle.

The compiler, verifier, VM, and host receive the same immutable value.

The standard command uses the standard bundle.

### 7.2 Stable definitions

Each operation definition contains these fields.

```text
qualified name
operation version
parameter schemas
reply schema
parameter boundary modes
reply boundary mode
operation kind
snapshot class
group memberships
```

Each group contains exact operation names or other group names.

### 7.3 Dense runtime slots

Standard operations retain their current slots.

The resulting slots remain dense.

Portable identity uses operation identities, not dense slots.

### 7.4 Bundle identity

The bundle digest covers every operation field.

It also covers all group membership edges.

Artifact verification binds the exact bundle digest.

Snapshot encoding binds the exact bundle digest.

A loader rejects a different bundle before execution.

The rejection names both bundle digests.

### 7.5 Compiler use

The checker resolves row names through the active bundle.

It creates `sys` operation members from the active bundle.

Every active operation uses the same call syntax.

Every active group uses the same row syntax.

The compiler emits the operation's dense slot.

### 7.6 Verifier use

The verifier validates operation slots against the active bundle.

It validates each operation type against the declared signature.

It validates each `PERFORM` argument and reply type.

It validates every row name against the active bundle.

Verification never trusts the host implementation.

### 7.7 VM use

Each loaded program retains its ABI bundle.

Every machine in one world uses the same bundle.

The VM reads operation names, kinds, signatures, and groups from that bundle.

A world rejects installed code from another bundle.

### 7.8 Public status

The ABI bundle machinery remains an internal runtime boundary.

This release does not expose a stable embedding crate.

Host extensibility needs a separate design and implementation unit.

That unit must use the same checker, verifier, policy, and snapshot contracts.

## 8. Program host inputs

### 8.1 Program arguments

The program entry remains an ordinary zero-parameter entry function.

The CLI does not call a terminal closure by a special rule.

The standard bundle defines this operation.

```text
Args.Get () -> [String]
```

Loom exposes the operation as `sys.args()`.

The call charges the `Args` effect row.

Runtime policy must grant `Args` or `Args.Get`.

Each call returns a guest-owned list in the original argument order.

The CLI rejects a native argument that is not valid UTF-8.

### 8.2 Argument parsing

`--` ends `lm run` option parsing.

Every later token belongs to the Loom program.

A token before `--` that is not the program path remains an error.

The CLI never ignores an extra positional token.

### 8.3 Exit status

Normal completion exits with status zero.

A machine fault exits with status one.

The first release defers an explicit status operation.

A terminal value does not control process status by its nominal type.

### 8.4 Standard input and output

The standard bundle adds these operations.

```text
Io.ReadBytes   (Int)   -> Result[Bytes, IoError]
Io.Write       (Bytes) -> Result[Int, IoError]
Io.WriteError  (Bytes) -> Result[Int, IoError]
```

`ReadBytes` returns empty bytes at the end of input.

A negative count returns `IoError.InvalidInput`.

A count above the host limit returns `IoError.LimitExceeded`.

Writes may accept fewer bytes than requested.

`std/io` supplies `write_all`, text printing, and buffered line reading.

Core does not model the process streams as zero-field resource classes.

The `std.io` helpers call the `sys.io` operations directly.

`ConsoleLineReader` is a class because it owns pending bytes and end-of-input state.

The existing text operations remain during migration.

They use the same underlying host streams.

### 8.5 Stream interfaces

Core defines effect-polymorphic byte stream interfaces.

```lm
interface ByteReader[effect e]
  type Error
  def read(self, count: Int): Result[Bytes, Self.Error] with e
end

interface ByteWriter[effect e]
  type Error
  def write(self, bytes: Bytes): Result[Int, Self.Error] with e
end
```

Concrete resource types retain their exact operation rows.

Generic helpers use the interface effect argument.

`ByteWriter.write` returns a positive count for each nonempty successful write.

The count cannot exceed the supplied byte length.

`std.io.write_all_to` faults when a writer breaks this contract.

The interfaces do not merge filesystem, network, and process authority.

### 8.6 Environment, arguments, and current directory

The standard bundle adds these operations.

```text
Args.Get       ()       -> [String]
Env.Get        (String) -> Result[Option[String], EnvError]
Fs.CurrentDir  ()       -> Result[String, FsError]
```

`Env.Get` reads one named variable.

The first release does not expose environment enumeration.

An absent variable returns `Ok(None)`.

Invalid platform text returns an encoding error.

`Fs.CurrentDir` uses the filesystem effect and error family.

### 8.7 Secure entropy

The standard bundle adds this operation.

```text
Entropy.Bytes (Int) -> Result[Bytes, EntropyError]
```

`Entropy.Bytes` returns cryptographically secure bytes.

It never falls back to deterministic data.

Deterministic `Rand` remains a separate effect for simulations and tests.

A host without secure entropy denies or fails the entropy operation.

### 8.8 Snapshot classification

Completed argument, environment, directory, and entropy values are machine state.

A pending console operation is a host attachment.

Environment, directory, and entropy queries complete inside the host call.

They leave no pending host attachment.

No host worker object enters snapshot bytes.

## 9. Standard error values

`IoError` gains stable cases for common conditions.

```lm
enum IoError
  BrokenPipe
  InvalidInput(message: String)
  LimitExceeded(message: String)
  Failed(message: String)
end
```

`EnvError` distinguishes invalid names, invalid encoding, denial, and platform failure.

`EntropyError` distinguishes invalid counts, limits, unavailability, and platform failure.

Every portable error type implements `Display`.

## 10. Stage 1 implementation

Stage 1 removes known reliability and diagnostic defects.

It includes these changes.

- Add focused probes for every reported failure.
- Repair broken-pipe handling.
- Reserve host faults for host defects.
- Add snapshot error text and snapshot byte encoding.
- Add deliberate panic and assertion functions.
- Add `Option.expect` and `Result.expect`.
- Accept control statements in short case arms.
- Repair interface diagnostics.
- Repair help and version handling.
- Replace misleading error sentinels in examples.

Stage 1 passes when failed platform I/O cannot panic the host process.

## 11. Stage 2 implementation

Stage 2 gives the host boundary one verified ABI identity.

It includes these changes.

- Add immutable ABI bundle construction.
- Convert standard manifest queries into standard-bundle queries.
- Thread bundles through checking, verification, loading, and execution.
- Bind artifacts and snapshots to the bundle digest.
- Keep the dynamic bundle builder as internal runtime machinery.
- Defer the public host extension API.

Stage 2 passes when each artifact and snapshot binds one exact ABI bundle.

## 12. Stage 3 implementation

Stage 3 makes Loom useful as an application language.

It includes these changes.

- Add `Args.Get` and the `sys.args()` surface.
- Pass arguments after `--` through the `Args` effect.
- Add byte console operations.
- Add result-bearing output.
- Add environment and filesystem current-directory operations.
- Add secure entropy.
- Add stream interfaces and standard helpers.

Stage 3 passes when one binary filter uses every command boundary safely.

## 13. Required tests

Each new operation needs checker, verifier, VM, host, and policy tests.

Each new resource needs cleanup, cancellation, and snapshot tests.

Each new error needs rendering and pattern tests.

The ABI bundle suite includes these cases.

- A different bundle rejects the artifact.
- A different bundle rejects the snapshot.
- The verifier checks operation slots against the bound bundle.

The command suite includes these cases.

- Binary standard input preserves invalid UTF-8.
- A closed output pipe returns `BrokenPipe`.
- CLI fault reporting on a closed pipe does not panic.
- Arguments preserve empty strings and Unicode.
- `sys.args()` preserves empty strings and Unicode.
- `sys.args()` needs the `Args` row and a policy grant.
- Environment absence returns `None`.
- Secure entropy never uses deterministic `Rand` state.

## 14. Release checks

Run formatting and workspace linting before every stage commit.

Run the full workspace suite before each stage closes.

Keep the existing test duration as a regression reference.

Do not add workers to hide slower tests.

Record any accepted compile-time or runtime change explicitly.

The branch closes Stage 3 only after all three stage gates pass.

## 15. Stage 3 implementation record

The Stage 3 gate passed on 2026-08-21.

Workspace linting and testing completed without failures.

The warm full workspace suite completed in about 29 seconds.

The release benchmark compared Stage 3 with commit `ed3bb7a`.

| Measurement | Stage 2 | Stage 3 | Change |
|---|---:|---:|---:|
| core classes | 172 | 185 | +7.6% |
| core functions | 513 | 528 | +2.9% |
| core artifact | 112,328 bytes | 117,003 bytes | +4.2% |
| core compilation | 1.885 ms | 1.973 ms | +4.7% |
| core loading | 0.802 ms | 0.830 ms | +3.5% |

Stage 3 adds core contracts, program input operations, and typed errors.

The selected execution benchmarks found no runtime regression.

| Benchmark | Stage 2 | Stage 3 |
|---|---:|---:|
| `int_loop` | 34.8 ns | 33.5 ns |
| `direct_call` | 32.7 ns | 32.9 ns |
| `world_int_loop` | 35.5 ns | 36.5 ns |

## 16. Interface doctrine

The first release adds an interface only when language syntax or generic core code consumes it.

The required core interface set contains these interfaces.

```lm
interface Display
  def append_to(self, mut builder: StringBuilder)
end

interface PartialEq
  def __eq__(self, other: Self): Bool
end

interface Hashable: PartialEq
  def __hash__(self): Int
end
```

`Iterable`, `Iterator`, `Counted`, `ByteReader`, and `ByteWriter` remain part of this set.

The first release does not add unused `Clone`, `Default`, `Ord`, or marker interfaces.

A later generic consumer can justify each additional interface.

## 17. Stage 4: `Self` and interface composition

Stage 4 provides the type foundation required by the new core interfaces.

Inside an interface contract, bare `Self` names the conforming type.

`Self` can appear in parameters, results, and nested type applications.

Inside a class, `Self` names the current nominal type application.

Interface inheritance uses the existing colon form.

A comma separates several parent interfaces.

An inherited interface contributes its methods and associated type requirements.

Two bounds can contribute one identical method contract without ambiguity.

Different contracts with one method name remain ambiguous.

Enums can declare and implement interfaces.

A normal class that conforms to a `Self`-dependent interface must be final.

An enum family can conform because its family is closed.

The closed native `Text` family can conform for the same reason.

Class `Self` names the declared class application. It does not promise a dynamic subclass type.

Artifacts store direct parent applications.

Generic bounds and conformances also store the complete parent closure.

The verifier rejects cycles, missing parent conformances, and inheritance beyond 128 levels.

Interface contracts and inherited contracts enter interface identity hashes.

An interface name remains invalid as a value type.

The diagnostic states that the name is an interface bound.

The first release does not add existential interface values.

A future release can add an explicit form such as `dyn Display`.

Stage 4 passes when `clone(): Self` and `same(other: Self)` enforce the conforming type.

## 18. Stage 5: `Display`

String interpolation accepts any value that conforms to `Display`.

`append_to` writes into the interpolation builder without an intermediate `String` allocation.

The core scalar and text implementations lower to existing builder intrinsics.

Portable error values implement `Display` and remove their repeated `message()` methods.

A pure core helper builds a standalone `String` from any `Display` value.

The checker removes the closed interpolation type list.

The verifier checks each selected display call.

Stage 5 passes when a user class interpolates through its declared conformance.

`string_interp` and `string_builder` must remain within normal benchmark noise.

The Stage 5 gate passed on 2026-08-22.

Workspace linting and testing completed without failures.

The warm full workspace suite completed in about 30 seconds.

The selected execution benchmarks found no regression.

| Benchmark | Stage 4 | Stage 5 |
|---|---:|---:|
| `string_interp` | 258.1 ns | 199.2 ns |
| `string_builder` | 43.3 ns | 41.4 ns |

## 19. Stage 6: `PartialEq`

An explicit `PartialEq` conformance activates a user `__eq__` method.

The `==` operator calls that method for a conforming left type.

The `!=` operator negates the same result.

Core removes `__ne__` as a surface method.

Built-in structural and identity equality remain language rules.

These rules do not imply a `PartialEq` conformance.

Int, Bool, Text, Char, and Bytes implement the interface.

A method named `__eq__` without conformance does not enable the operator.

Stage 6 passes when generic equality uses one verified interface dispatch.

Primitive equality benchmarks must remain within normal benchmark noise.

The Stage 6 gate passed on 2026-08-22.

Workspace testing completed without failures.

The warm full workspace suite completed in about 30 seconds.

Core size and startup costs did not increase.

| Measurement | Stage 5 | Stage 6 |
|---|---:|---:|
| core classes | 185 | 185 |
| core functions | 540 | 535 |
| core artifact | 119,968 bytes | 119,518 bytes |
| core compilation | 1.948 ms | 1.943 ms |
| core loading | 0.838 ms | 0.835 ms |

Native equality retained its existing instructions.

| Benchmark | Stage 5 | Stage 6 |
|---|---:|---:|
| `int_eq` | 33.4 ns | 32.0 ns |
| `text_eq` | 40.8 ns | 40.5 ns |

Generic `PartialEq` dispatch measured 93.8 ns per comparison.

## 20. Stage 7: `Hashable`, `Map`, and `Set`

`Hashable` extends `PartialEq`.

Its equality must be reflexive, symmetric, and transitive.

Equal values must return equal semantic hashes.

A hash must remain stable while its value is frozen.

The VM mixes each semantic hash with a private process key.

`Map[K, V]` requires `K` to conform to `Hashable`.

Bytecode selects a native path or an interface-backed path for each map operation.

Built-in keys retain the current native instruction path.

Generic and user keys call verified `__hash__` and `__eq__` implementations.

Compiler-private probe instructions separate these calls from synchronous VM instructions.

The VM never invokes guest code inside one collection instruction.

Each stored entry caches its semantic hash.

A lookup computes the query hash once.

It calls equality only for matching hash candidates.

The VM mixes semantic hashes with a private process key before bucket access.

The map contract rejects an effectful hash or equality method.

User heap keys must be frozen before insertion.

A mutable user heap key faults with `MutableMapKey`.

Snapshots store semantic hashes beside entries.

Snapshots omit the private derived index.

Restoration rebuilds that index with the active process key.

Core defines ordered `Set[T]` as an ordinary class over `Map[T, ()]`.

`Set` provides insertion, removal, traversal, copying, filtering, and standard set algebra.

Stage 7 passes when a frozen user class works as a map key and set element.

Existing Int, Text, and Bytes map benchmarks must not regress.

The Stage 7 gate passed on 2026-08-22.

Workspace testing completed without failures.

Strict workspace linting completed without warnings.

The warm full workspace suite completed in 35.7 seconds.

Set added two classes and thirty functions.

Those methods account for this stage's measured core growth.

| Measurement | Stage 6 | Stage 7 |
|---|---:|---:|
| core classes | 185 | 187 |
| core functions | 535 | 565 |
| core artifact | 119,518 bytes | 131,407 bytes |
| core compilation | 1.943 ms | 2.145 ms |
| core loading | 0.835 ms | 0.983 ms |

Native map operations retained their existing instruction path.

| Benchmark | Stage 6 | Stage 7 |
|---|---:|---:|
| `map_insert` | 128.0 ns | 122.3 ns |
| `map_lookup` | 71.6 ns | 68.0 ns |
| `map_str_lookup` | 60.5 ns | 60.4 ns |
| `map_bytes_lookup` | 57.4 ns | 52.7 ns |

The generic user-key lookup measured 207.2 ns.

## 21. Interface release gates

Each stage records core compilation, artifact size, loading time, and full suite time.

Each stage measures its affected runtime benchmarks.

The full suite retains the current duration as its reference.

No stage can hide slower tests by adding workers.
