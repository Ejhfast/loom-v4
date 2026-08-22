# Pre-release Language and Host Foundation

Status: stages 1 through 3 implemented.

This sidecar defines the first public release foundation.

It covers language reliability, extensible host operations, and the command application world.

Self-hosting is outside this work.

Interpreter and compiler optimization are outside this work.

Each stage records its build, test, and execution costs.

## 1. Release contract

The first public release makes four user-visible promises.

1. Guest code and malformed external data never crash the host process.
2. A command program can consume inputs and perform normal application work.
3. An embedder can add typed host operations without changing Loom's core.
4. Every operation remains visible to effect checking and runtime policy.

The release can omit later convenience features.

It cannot omit a safe extension boundary.

It cannot turn ordinary platform failures into host faults.

It cannot accept command arguments and then discard them.

## 2. Terms

| Term | Meaning |
|---|---|
| standard bundle | The pinned groups and operations distributed with Loom |
| extension bundle | Immutable operation definitions supplied by one embedder |
| ABI bundle | The standard bundle plus zero or more extension bundles |
| bundle digest | The canonical identity of one ABI bundle |
| host failure | An ordinary platform failure returned as a typed value |
| host defect | A host implementation violates its declared operation contract |
| command entry | The program value selected by `lm run` |
| command arguments | The strings after the command-line `--` separator |

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

It gains `message(): String`.

It later conforms to the common display interface.

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

## 7. ABI bundles

### 7.1 Purpose

The current standard operation manifest becomes the standard ABI bundle.

An embedder can add operations without editing `lm-abi`.

The compiler, verifier, VM, and host receive the same immutable bundle.

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

A bundle builder rejects duplicate names.

It rejects missing group members.

It rejects recursive group membership.

It rejects unsupported type schemas.

It rejects a redefinition of a standard name.

### 7.3 Dense runtime slots

Standard operations retain their current slots.

Extension operations follow the standard operations.

The builder sorts extension operations by stable identity.

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

An extension operation uses the same call syntax as a standard operation.

An extension group uses the same row syntax as a standard group.

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

### 7.8 Host registration

The embedding API registers one implementation for each served operation.

The API supplies checked owned arguments.

The API supplies a typed reply builder.

The host receives no writable guest pointer.

An asynchronous implementation receives a single-use completion handle.

The host can cancel a pending completion.

The VM rejects a reply with the wrong declared type.

### 7.9 Extension resources

An extension bundle can declare an opaque resource kind.

Each resource kind has a stable qualified name.

Each resource value carries a kind, token, and generation.

The VM resource registry owns its lifecycle record.

Each kind declares cleanup and cancellation callbacks.

The first release classifies extension resources as host attachments.

Any live extension resource blocks snapshot creation.

### 7.10 Initial embedding scope

The first Rust API supports data operations and opaque resources.

It supports synchronous and asynchronous completion.

It supports policy configuration and manual driving.

It does not bypass artifact verification.

It does not expose raw heap storage.

The initial release does not require a C interface.

## 8. Command application world

### 8.1 Entry forms

`lm run` accepts these entry forms.

```text
frozen non-callable value
() -> T with e
([String]) -> T with e
```

A frozen non-callable value accepts no command arguments.

The zero-argument function receives the empty tuple.

The argument function receives one frozen list.

The list contains strings after `--` in their original order.

Any other callable signature produces a command-entry diagnostic.

### 8.2 Argument parsing

`--` ends `lm run` option parsing.

Every later token belongs to the Loom program.

A token before `--` that is not the program path remains an error.

The CLI never ignores an extra positional token.

### 8.3 Exit status

Core defines `ExitStatus`.

```lm
enum ExitStatus
  Success
  Failure
  Code(value: Int)
end
```

An entry returning another type exits with status zero after normal completion.

`Success` exits with status zero.

`Failure` exits with status one.

`Code(value)` accepts values from zero through 255.

An invalid code produces a command-entry diagnostic and status one.

A machine fault exits with status one.

An embedder remains free to interpret terminal values differently.

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

### 8.6 Environment and current directory

The standard bundle adds these operations.

```text
Env.Get              (String) -> Result[Option[String], EnvError]
Process.CurrentDir   ()       -> Result[String, ProcessError]
```

`Env.Get` reads one named variable.

The first release does not expose environment enumeration.

An absent variable returns `Ok(None)`.

Invalid platform text returns an encoding error.

The current directory is a host query.

It does not grant filesystem access.

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

`ProcessError` distinguishes invalid input, denial, missing paths, and platform failure.

`EntropyError` distinguishes invalid counts, limits, unavailability, and platform failure.

Every error type provides `message(): String`.

## 10. Stage 1 implementation

Stage 1 removes known reliability and diagnostic defects.

It includes these changes.

- Add focused probes for every reported failure.
- Repair broken-pipe handling.
- Reserve host faults for host defects.
- Add snapshot error messages and snapshot byte encoding.
- Add deliberate panic and assertion functions.
- Add `Option.expect` and `Result.expect`.
- Accept control statements in short case arms.
- Repair interface diagnostics.
- Repair help and version handling.
- Replace misleading error sentinels in examples.

Stage 1 passes when failed platform I/O cannot panic the host process.

## 11. Stage 2 implementation

Stage 2 makes the host boundary extensible.

It includes these changes.

- Add immutable ABI bundle construction.
- Convert standard manifest queries into standard-bundle queries.
- Thread bundles through checking, verification, loading, and execution.
- Bind artifacts and snapshots to the bundle digest.
- Add extension operations and groups.
- Add extension resource descriptors.
- Add the `lm-embed` Rust crate.
- Add a custom telemetry operation example.

Stage 2 passes when custom typed operations need no core source edit.

## 12. Stage 3 implementation

Stage 3 makes Loom useful as a command application.

It includes these changes.

- Activate callable command entries.
- Pass command arguments after `--`.
- Add byte console operations.
- Add result-bearing output.
- Add environment and current-directory operations.
- Add explicit command exit status.
- Add secure entropy.
- Add stream interfaces and standard helpers.

Stage 3 passes when one binary filter uses every command boundary safely.

## 13. Required tests

Each new operation needs checker, verifier, VM, host, and policy tests.

Each new resource needs cleanup, cancellation, and snapshot tests.

Each new error needs rendering and pattern tests.

The extension suite includes these cases.

- A custom operation compiles and runs.
- A missing implementation faults only its machine.
- A wrong reply type faults only its machine.
- A different bundle rejects the artifact.
- A custom group expands to its exact operations.
- A live custom resource blocks a snapshot.

The command suite includes these cases.

- Binary standard input preserves invalid UTF-8.
- A closed output pipe returns `BrokenPipe`.
- CLI fault reporting on a closed pipe does not panic.
- Arguments preserve empty strings and Unicode.
- A non-callable entry rejects command arguments.
- Environment absence returns `None`.
- Secure entropy never uses deterministic `Rand` state.
- Every exit status maps correctly.

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
| core classes | 172 | 194 | +12.8% |
| core functions | 513 | 538 | +4.9% |
| core artifact | 112,328 bytes | 119,594 bytes | +6.5% |
| core compilation | 1.885 ms | 1.937 ms | +2.8% |
| core loading | 0.802 ms | 0.837 ms | +4.4% |

Stage 3 adds core contracts, command status values, and typed errors.

The selected execution benchmarks found no runtime regression.

| Benchmark | Stage 2 | Stage 3 |
|---|---:|---:|
| `int_loop` | 34.8 ns | 31.8 ns |
| `direct_call` | 32.7 ns | 31.3 ns |
| `world_int_loop` | 35.5 ns | 34.2 ns |
