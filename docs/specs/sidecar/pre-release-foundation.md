# Pre-release Host Effects

Status: complete. Stages 0 through 8 and the integration follow-up are complete.

This sidecar replaces the completed pre-release foundation plan.

The language specification records the completed language work.

This plan defines the remaining host effects for the first public release.

## 1. Purpose

The release must support normal command tools, servers, terminal programs, and durable checkpoints.

Every host action must keep an exact effect identity.

Every live host attachment must use the common resource model.

Every ordinary platform failure must return a typed error.

The VM must contain no operating-system implementation.

Loom code must provide composition and convenience where possible.

## 2. Existing foundation

This work extends these implemented parts:

- exact operations and transparent effect groups;
- policy tables, mocks, drivers, and pass-through routing;
- typed `Result` errors that implement `Error`;
- a generic host resource registry;
- typed `Wait[T]` values and `select`;
- byte input and output;
- file handles;
- DNS, TCP, TLS clients, and HTTP/1.1;
- arguments, environment access, clocks, and entropy;
- snapshot checks for live host attachments.

The new work must reuse these parts.

It must not create a second scheduler, handle model, or policy path.

## 3. Main decisions

This plan makes these changes to the initial proposal:

1. One generic `Op.wait(...)` rule creates host-operation wait sources.
2. The operation manifest marks the operations that support this rule.
3. Wait selection uses readiness and commit as separate steps.
4. Raw terminal mode uses an explicit `RawMode` resource.
5. Signal delivery uses an explicit `SignalStream` resource.
6. Terminal and signal support land together.
7. File and directory removal use different operations.
8. Files and directories have separate durability operations.
9. Pipes use typed read and write ends.
10. UDP receives one complete datagram and reports its local address.

These changes remove hidden lifetime state.

They also remove duplicated wait operation identities.

## 4. General host-operation waits

### 4.1 Surface

An exact operation value can create a typed wait source.

```lm
select
in sys.io.read_bytes.wait(64) -> input
  handle_input(input)
in sys.clock.sleep.wait(16_000_000) -> tick
  handle_tick(tick)
end
```

For an operation with this type:

```text
Op[op, (A...) -> R]
```

the checker gives this special method type:

```text
wait(A...) -> Wait[R] with op
```

The operation manifest must mark `op` as a wait-source operation.

A direct call keeps its current behavior.

```lm
result = sys.io.read_bytes(64)
source = sys.io.read_bytes.wait(64)
```

The first expression performs the operation immediately.

The second expression prepares one wait source.

The API has three consumption tiers.

`sys.io.read_bytes(64)` performs one operation directly.

`sys.io.read_bytes.wait(64).wait()` prepares and consumes one source.

`select` prepares several sources and consumes one selected source.

### 4.2 Identity and authority

`Op.wait` creates no new exact operation.

The source charges the original operation in the enclosing effect row.

The source also uses the original policy action.

A policy table can block, pass, mock, or drive the operation as before.

A later policy edit does not change an existing prepared source.

That edit applies to later source preparation.

`Wait.Choose`, `Wait.Wait`, and `Wait.Cancel` keep their current identities.

The wait-source flag forms part of the operation manifest identity.

### 4.3 Checker, bytecode, and verifier

The checker reads the argument tuple and result from the operation type.

It rejects `.wait` on an operation without the manifest flag.

The bytecode adds one typed wait preparation instruction.

That instruction stores the exact operation and its checked arguments.

Selectable operation slots and argument counts must not exceed 65,535.

The container keeps 32-bit fields. The decoder rejects values that exceed the compact execution form.

The verifier repeats the manifest, argument, result, and effect checks.

Malformed bytecode cannot prepare a wait for another operation.

Normal `PERFORM` lowering does not change.

### 4.4 Lifecycle

A host-operation wait has these states:

| State | Meaning |
|---|---|
| prepared | The VM validated authority, arguments, limits, and resource use. |
| armed | The scheduler registered the source with its service. |
| ready | The service can produce a result without another external wait. |
| committed | Selection chose this source and consumed its result. |
| cancelled | Selection withdrew this source without a result. |

Preparation does not consume input data.

Arming starts all leaves before the scheduler selects one ready leaf.

Arm order resolves simultaneous readiness.

The selected source commits once.

Every losing source cancels once.

### 4.5 Cancellation contract

The host must separate readiness from guest-visible commitment.

A ready source must remain ready until commit or cancellation.

A read source can fill a bounded host buffer before commit.

Cancellation keeps those bytes on the same logical stream.

An accept source can hold a connection in a bounded host queue.

Cancellation returns that connection to the same logical listener.

A signal source keeps its event in the signal queue until commit.

The input service can read ahead into a bounded host buffer.

TCP, TLS, pipe, and UDP services can retain input before commit.

A connect source starts its connection attempt during arming.

Cancellation closes an unfinished or uncommitted connection.

The remote peer can observe that short connection attempt.

A DNS worker can finish before commit.

The host retains its bounded result until commit or cancellation.

The host protocol reports one of these cancellation outcomes:

- cancellation won and no result escaped;
- commitment won and the result belongs to the selected source.

The current Boolean host cancellation result cannot express this rule.

Stage 1 replaces it with an explicit outcome.

Guest drivers can perform visible work while they prepare a reply.

Selection does not undo completed guest work.

A driver for a wait-source operation must preserve consumable input on cancellation.

### 4.6 Initial wait-source operations

The final manifest marks these operations:

| Operation | Cancellation rule |
|---|---|
| `Clock.Sleep` | Remove the timer. |
| `Io.ReadBytes` | Keep unread or read-ahead bytes. |
| `Dns.Resolve` | Discard a retained result. |
| `Tcp.Connect` | Close the uncommitted socket. |
| `Tcp.Accept` | Leave the connection queued until commit. |
| `Tcp.Read` | Leave bytes in the socket or host buffer. |
| `Tls.Read` | Leave plaintext in the TLS buffer. |
| `Signal.Next` | Leave the signal in its bounded queue. |
| `Pipe.Read` | Leave bytes in the pipe or host buffer. |
| `Exec.Wait` | Keep the reaped child status. |
| `Udp.RecvFrom` | Leave the datagram queued until commit. |

Writes do not become wait sources in this release.

A cancelled write can hide partial external progress.

File operations do not become wait sources in this release.

The bounded file worker service remains the file suspension path.

TLS handshakes do not become wait sources in this release.

A handshake consumes its TCP stream when host submission starts.

### 4.7 Resources and snapshots

A prepared root-host source records a pending host attachment.

That attachment blocks snapshot creation before and during arming.

Commit or cancellation removes the attachment.

The snapshot blocker names the exact pending operation.

Receive and drive waits keep their current snapshot behavior.

Host-operation sources use the existing per-machine wait limit.

Their retained values use the existing host retained-byte budgets.

Machine cleanup cancels every live source.

Cleanup never replaces the machine's first fault.

## 5. Terminal effects

### 5.1 Exact operations

The `Tty` group contains these operations:

```text
Tty.IsTerminal (StdStream) -> Bool
Tty.Size       (StdStream) -> Result[TtySize, TtyError]
Tty.EnterRaw   () -> Result[RawMode, TtyError]
Tty.ExitRaw    (RawMode) -> Result[(), TtyError]
```

`StdStream` has `Input`, `Output`, and `Error` cases.

`TtySize` contains positive `columns` and `rows` values.

`Tty.Size` rejects a stream that is not a terminal.

`Tty.EnterRaw` changes standard input only.

`TtyError` is this closed family:

```lm
enum TtyError implements Error
  Closed
  NotTerminal
  Busy
  PermissionDenied(message: String)
  Unsupported(message: String)
  Failed(message: String)
end
```

### 5.2 Why raw mode uses a resource

`SetRaw(Bool)` hides ownership and nesting.

One proc could disable raw mode that another proc owns.

A `RawMode` value gives the state one explicit lifetime.

`RawMode` is a final native resource class.

Every alias names the same host attachment.

Closing one alias restores the saved terminal state.

A later exit request returns `TtyError.Closed`.

Only one raw resource can exist for one root host.

A second enter request returns `TtyError.Busy`.

The host stores the exact original terminal state before the change.

The host restores that state on these events:

- `Tty.ExitRaw`;
- owner completion;
- owner fault;
- normal host exit;
- handled interrupt or termination.

No process can restore terminal state after `SIGKILL` or a host abort.

A live `RawMode` blocks snapshot creation.

### 5.3 Raw-mode scope

The first raw mode disables canonical input and local echo.

It also exposes terminal control bytes through `Io.ReadBytes`.

The platform adapter documents any unavoidable platform difference.

Later mode flags can use a new configuration value.

The first release exposes no arbitrary terminal control bits.

### 5.4 Standard terminal code

`std/term` contains pure escape-sequence helpers and bounded key decoding.

It performs byte input and output through `Io`.

It polls `Tty.Size` after a timer tick.

The first release adds no resize signal or resize operation.

## 6. Signal effects

### 6.1 Exact operations

The `Signal` group contains these operations:

```text
Signal.Open  (List[SignalKind]) -> Result[SignalStream, SignalError]
Signal.Next  (SignalStream) -> Result[SignalKind, SignalError]
Signal.Close (SignalStream) -> Result[(), SignalError]
```

`SignalKind` contains `Interrupt` and `Terminate`.

The requested list must contain at least one kind.

The host removes duplicate kinds before it opens the stream.

`Signal.Next` is a wait-source operation.

`SignalStream.next_wait()` uses `sys.signal.next.wait(self)`.

### 6.2 Why signals use a stream

A stream gives guest signal delivery an explicit lifetime.

Without a live stream, the host keeps normal signal behavior unless raw mode is active.

Raw mode installs cleanup handlers for both supported signals.

Opening a stream reuses or installs safe host notification handlers.

Closing one alias closes the stream and removes guest signal delivery.

The host terminates normally after it observes an unrequested signal.

It restores every raw terminal resource before that termination.

Only one signal stream can exist for one root host.

A second open request returns `SignalError.Busy`.

The command host permits one active platform signal service in one process.

Another root host receives `Busy` while that service remains active.

The current signal adapter supports Linux.

Other platforms return `SignalError.Unsupported`.

Raw mode also returns `TtyError.Unsupported` when signal cleanup is unavailable.

`SignalError` is this closed family:

```lm
enum SignalError implements Error
  Closed
  InvalidInput(message: String)
  Busy
  Unsupported(message: String)
  LimitExceeded(message: String)
  Failed(message: String)
end
```

The host handles no signal by running guest code asynchronously.

The platform handler only records a bounded notification.

The scheduler observes that notification at a normal host boundary.

### 6.3 Delivery rules

The host queues requested signals while the stream remains open.

The queue preserves observed order within its bounded capacity.

The platform can coalesce equal signals before host observation.

The host coalesces duplicate queued notifications of one kind.

`Signal.Next` removes one matching signal only after selection commits.

The first observed `Interrupt` starts one escalation state.

A second `Interrupt` before stream closure forces host termination.

The host restores every raw terminal resource before that termination.

Stream closure resets the interrupt escalation state.

Uncatchable platform termination remains outside this guarantee.

### 6.4 Snapshot use

A live `SignalStream` is a host attachment.

It blocks snapshot creation.

A shutdown handler closes the stream before it captures a snapshot.

This flow supports checkpoint-on-termination:

1. Open one signal stream.
2. Select between work and `Signal.Next`.
3. Quiesce all live resources after `Terminate`.
4. Close the signal stream.
5. Capture and persist the snapshot.
6. Exit the program.

## 7. File-system completion

### 7.1 Exact operations

The `Fs` group gains these operations:

```text
Fs.Stat        (String) -> Result[FileInfo, FsError]
Fs.ReadDir     (String, Int) -> Result[List[Result[DirEntry, FsError]], FsError]
Fs.CreateDir   (String) -> Result[(), FsError]
Fs.RemoveFile  (String) -> Result[(), FsError]
Fs.RemoveDir   (String) -> Result[(), FsError]
Fs.Rename      (String, String, RenameMode) -> Result[(), FsError]
Fs.Sync        (FileHandle) -> Result[(), FsError]
Fs.SyncDir     (String) -> Result[(), FsError]
```

`OpenOptions` gains `CreateNew`.

`CreateNew` creates one file and rejects an existing path.

`Fs.CreateDir` creates exactly one directory.

Its parent directory must exist.

### 7.2 Portable values

`FileKind` contains `File`, `Directory`, `Symlink`, and `Other`.

`FileInfo` contains these fields:

```text
kind: FileKind
byte_length: Int
modified_ns: Option[Int]
read_only: Bool
```

`modified_ns` uses nanoseconds from the Unix epoch.

An unavailable modification time produces `None`.

`Fs.Stat` follows the final symbolic link.

`byte_length` reports the platform metadata length for every file kind.

`DirEntry` contains a UTF-8 `name` and a `FileKind`.

Its `FileKind` does not follow the final symbolic link.

`RenameMode` contains `NoReplace` and `Replace`.

### 7.3 Directory rules

`Fs.ReadDir` accepts an explicit maximum entry count.

The host also enforces a fixed maximum.

An excessive directory returns `FsError.LimitExceeded`.

The outer error reports failure to open or continue the directory.

Each inner error reports one bad entry.

A non-UTF-8 entry name produces `FsError.InvalidEncoding` in that entry.

The operation never drops such an entry silently.

Directory order has no semantic guarantee.

`std/fs` provides a helper that sorts valid names.

String paths remain the first release path model.

Byte paths remain in the release ledger.

### 7.4 Removal and rename

`Fs.RemoveFile` removes a file or a symbolic link.

It never follows the final symbolic link.

`Fs.RemoveDir` removes one empty directory.

The standard library can build recursive removal with explicit policy.

`NoReplace` returns `AlreadyExists` when the target exists.

`Replace` requests an atomic replacement of a compatible target.

It can replace a non-directory target with a non-directory source.

A directory source requires an absent target.

Rename never follows the final symbolic link at either path.

The host returns `Unsupported` when it cannot provide that operation safely.

The host never emulates atomic rename with a remove-then-rename sequence.

The host never copies across file systems during rename.

A cross-file-system request returns `CrossDevice`.

### 7.5 Durability

`Fs.Flush` flushes language and host stream buffers.

It makes no storage durability promise.

`Fs.Sync` requests durable file contents and file metadata.

It maps to the platform equivalent of `sync_all`.

`Fs.SyncDir` requests durability for directory-entry changes.

A durable replacement uses this order:

1. Write a temporary file.
2. Flush the temporary file.
3. Sync the temporary file.
4. Rename it with `Replace`.
5. Sync its parent directory.

### 7.6 Errors

`FsError` becomes this closed family:

```lm
enum FsError implements Error
  Closed
  InvalidInput(message: String)
  InvalidEncoding(message: String)
  LimitExceeded(message: String)
  NotFound(message: String)
  AlreadyExists(message: String)
  PermissionDenied(message: String)
  NotDirectory(message: String)
  IsDirectory(message: String)
  DirectoryNotEmpty(message: String)
  CrossDevice(message: String)
  Unsupported(message: String)
  Failed(message: String)
end
```

The host maps stable platform categories before it uses `Failed`.

Messages have a fixed scalar limit.

The host exposes no raw platform error number.

## 8. Byte-only console I/O

### 8.1 Exact operations

The `Io` group keeps only these operations:

```text
Io.ReadBytes  (Int) -> Result[Bytes, IoError]
Io.Write      (Bytes) -> Result[Int, IoError]
Io.WriteError (Bytes) -> Result[Int, IoError]
```

The ABI removes the three legacy text operations.

This change removes two parallel console models.

### 8.2 Core helpers

The pinned core provides these zero-import helpers:

```text
print[T: Display](value: T) -> Result[(), IoError] with Io.Write
println[T: Display](value: T) -> Result[(), IoError] with Io.Write
print_error[T: Display](value: T) -> Result[(), IoError] with Io.WriteError
read_line(Int) -> Result[Option[String], IoError] with Io.ReadBytes
```

`print` and `println` use the `Display` contract.

`read_line` reads small prompt input without hidden text effects.

`std/io.ConsoleLineReader` keeps its bounded buffered path for bulk input.

Invalid UTF-8 returns `IoError.InvalidInput`.

### 8.3 Migration

This stage migrates every effect row, policy rule, mock, example, and test.

It also updates compiler examples that inspect exact operations.

The old operation names receive no compatibility aliases.

Pre-release artifacts and snapshots can break across this ABI change.

### 8.4 Byte stream interfaces

Core defines effect-polymorphic `ByteReader` and `ByteWriter` interfaces.

```lm
interface ByteReader[effect e]
  type Error: Error
  def read(self, count: Int): Result[Bytes, Self.Error] with e
end

interface ByteWriter[effect e]
  type Error: Error
  def write(self, bytes: Bytes): Result[Int, Self.Error] with e
end
```

These resource classes implement the interfaces with their exact rows:

| Resource | Reader row | Writer row |
|---|---|---|
| `FileHandle` | `Fs.Read` | `Fs.Write` |
| `PipeReader` | `Pipe.Read` | none |
| `PipeWriter` | none | `Pipe.Write` |
| `TcpStream` | `Tcp.Read` | `Tcp.Write` |
| `TlsStream` | `Tls.Read` | `Tls.Write` |

The public TCP and TLS read methods return `Bytes`.

Empty bytes report orderly end of input for a positive read count.

The exact TCP and TLS operations retain `TcpRead` for driver and wait protocols.

`std/io.write_all_to` accepts every conforming writer.

## 9. Pipes and operating-system children

### 9.1 Effect split

`Pipe` controls anonymous byte pipes.

`Exec` controls operating-system child programs.

The split lets policy grant redirection without process creation.

`Proc` remains the Loom process group.

The new API never calls an operating-system child a proc.

### 9.2 Pipe operations

```text
Pipe.Open  () -> Result[(PipeReader, PipeWriter), PipeError]
Pipe.Read  (PipeReader, Int) -> Result[Bytes, PipeError]
Pipe.Write (PipeWriter, Bytes) -> Result[Int, PipeError]
Pipe.Close (PipeEnd) -> Result[(), PipeError]
```

`PipeEnd` is the sealed native parent of both end types.

`Pipe.Read` is a wait-source operation.

An empty successful read reports end of input.

`Pipe.Write` can report partial progress.

Closing the final writer lets readers observe end of input.

A live pipe end blocks snapshot creation.

`PipeError` is this closed family:

```lm
enum PipeError implements Error
  Closed
  BrokenPipe
  InvalidInput(message: String)
  LimitExceeded(message: String)
  Unsupported(message: String)
  Failed(message: String)
end
```

### 9.3 Child specification

The core defines these boundary values:

```text
ChildInput  = Inherit | Null | Pipe(PipeReader)
ChildOutput = Inherit | Null | Pipe(PipeWriter)
ChildEnv    = Inherit | Exact(Map[String, String]) | Overlay(Map[String, String])

ExecSpec {
  program: String,
  arguments: List[String],
  directory: Option[String],
  environment: ChildEnv,
  input: ChildInput,
  output: ChildOutput,
  error: ChildOutput
}
```

Separate input and output types reject a pipe direction error statically.

The exact environment contains no inherited value unless the caller supplies it.

The overlay environment inherits host values.

Overlay entries add or replace values with the same name.

An overlay cannot remove an inherited value.

Use an exact environment when inherited values must be absent.

Program lookup uses the selected environment and platform rules.

One child specification contains at most 4,096 arguments.

It contains at most 4,096 explicit environment entries.

One text item contains at most 65,536 UTF-8 bytes.

All text items contain at most 1,048,576 UTF-8 bytes together.

One pipe read or write contains at most 16,777,216 bytes.

### 9.4 Exec operations

```text
Exec.Spawn     (ExecSpec) -> Result[Child, ExecError]
Exec.Wait      (Child) -> Result[ChildStatus, ExecError]
Exec.Terminate (Child) -> Result[(), ExecError]
Exec.Kill      (Child) -> Result[(), ExecError]
Exec.Close     (Child) -> Result[(), ExecError]
```

`ChildStatus` contains `Exited(code: Int)` and `Terminated`.

`Exec.Wait` is a wait-source operation.

`ExecError` is this closed family:

```lm
enum ExecError implements Error
  Closed
  InvalidInput(message: String)
  LimitExceeded(message: String)
  NotFound(message: String)
  PermissionDenied(message: String)
  Unsupported(message: String)
  Failed(message: String)
end
```

A successful spawn consumes every pipe end that the child receives.

Every alias of a consumed pipe end becomes closed.

A failed spawn leaves those pipe ends open.

`Exec.Wait` reaps the child and consumes its handle after commit.

`Terminate` requests the platform's normal termination path.

`Kill` requests forced termination.

The API exposes no raw signal number.

`Exec.Close` detaches a running child and arranges later reaping.

It does not terminate the child.

A live child handle blocks snapshot creation.

The API never invokes a shell implicitly.

`std/exec` can provide builders and explicit shell helpers later.

## 10. TLS server handshake

The `Tls` namespace gains one server operation:

```text
Tls.ServerHandshake
  (TcpStream, List[Bytes], Bytes, List[Bytes], Int, Int)
  -> Result[TlsStream, TlsError]
```

The surface accepts a `TlsServerConfig` value.

That value contains these fields:

```text
certificate_chain: List[Bytes]
private_key: Bytes
alpn: List[Bytes]
minimum_version: TlsVersion
max_buffer_bytes: Int
```

Each certificate uses DER encoding.

The private key uses PKCS#8 DER encoding.

The host repeats all count and byte checks before parsing.

The certificate chain contains from 1 through 128 certificates.

Each certificate contains from 1 through 1,048,576 bytes.

The full certificate chain contains at most 4,194,304 bytes.

The private key contains from 1 through 1,048,576 bytes.

The ALPN list contains at most 32 values.

Each ALPN value contains from 1 through 255 bytes.

All ALPN values contain at most 4,096 bytes.

The buffer limit is from 1 through 1,048,576 bytes.

The minimum version value is 12 or 13.

Submission consumes the TCP stream on every result.

A successful call returns the existing `TlsStream` type.

The new `Tls.Server` effect group includes the handshake and `Tls.Stream`.

Client authentication and certificate reload policy remain deferred.

## 11. UDP

### 11.1 Exact operations

```text
Udp.Bind         (SocketAddress) -> Result[UdpSocket, NetError]
Udp.SendTo       (UdpSocket, SocketAddress, Bytes) -> Result[(), NetError]
Udp.RecvFrom     (UdpSocket) -> Result[UdpDatagram, NetError]
Udp.LocalAddress (UdpSocket) -> Result[SocketAddress, NetError]
Udp.Close        (UdpSocket) -> Result[(), NetError]
```

`Udp.RecvFrom` is a wait-source operation.

`Udp.LocalAddress` makes port-zero binding useful.

`Udp.Socket` contains send, receive, address, and close operations.

The `Udp` group contains bind and `Udp.Socket`.

### 11.2 Datagram rules

`UdpDatagram` contains immutable `data` and its peer address.

The host reads one complete datagram into a bounded buffer.

The first datagram byte limit is 65,535.

The operation never truncates a datagram silently.

A zero-length datagram remains a valid datagram.

`Udp.SendTo` sends the complete datagram or returns an error.

It never reports partial progress.

The existing network retained-byte budget includes queued datagrams.

A live UDP socket blocks snapshot creation.

Connected UDP, multicast, broadcast, and ancillary data remain deferred.

## 12. Pinned existing semantics

### 12.1 Console writes

Each `Io.Write` call makes one platform write attempt.

The host flushes accepted bytes before it returns `Ok`.

The returned integer reports accepted bytes.

A closed output pipe returns `IoError.BrokenPipe`.

Diagnostic reporting treats its own closed pipe as a completed report.

### 12.2 DNS scope

`Dns.Resolve` uses the operating-system resolver.

It can inspect host files, resolver configuration, and configured name services.

It can cause network traffic.

The exact `Dns.Resolve` effect covers that complete authority.

### 12.3 TCP delay policy

Every connected or accepted TCP stream enables `TCP_NODELAY`.

The host closes the socket if it cannot establish that invariant.

Setter operations remain in the release ledger.

## 13. Effect and resource summary

| Group | Long-lived resource | Snapshot rule | Wait-source operation |
|---|---|---|---|
| `Io` | none | pending read blocks | `Io.ReadBytes` |
| `Tty` | `RawMode` | live mode blocks | none |
| `Signal` | `SignalStream` | live stream blocks | `Signal.Next` |
| `Fs` | `FileHandle` | live handle blocks | none |
| `Pipe` | pipe ends | live end blocks | `Pipe.Read` |
| `Exec` | `Child` | live child blocks | `Exec.Wait` |
| `Dns` | none | pending resolve blocks | `Dns.Resolve` |
| `Tcp` | stream or listener | live resource blocks | connect, accept, read |
| `Tls` | `TlsStream` | live stream blocks | `Tls.Read` |
| `Udp` | `UdpSocket` | live socket blocks | `Udp.RecvFrom` |

Closed resource values remain typed machine state.

Restore never recreates an operating-system attachment.

## 14. Layer placement

### 14.1 Canonical manifest

The manifest owns operation names, signatures, groups, snapshot classes, and wait-source flags.

Each field contributes to the ABI identity.

### 14.2 Pinned core

The core owns boundary enums, errors, native resource classes, and direct methods.

It also owns zero-import display output helpers.

### 14.3 Standard Loom modules

Standard modules own buffering, scoped cleanup, sorting, protocol parsing, and ergonomic builders.

They perform only the exact operations in their declared rows.

### 14.4 VM

The VM owns generic wait, resource, policy, verification, and snapshot machinery.

It knows resource kinds but no operating-system API.

### 14.5 Root host

The root host owns terminal state, signal hooks, files, children, pipes, and sockets.

It uses bounded workers or reactors outside the scheduler thread.

The host starts each new service only after its first operation.

The root host serves every operation in its installed ABI table.

A missing platform capability returns the operation's typed `Unsupported` error.

The host never reports a table operation as unimplemented.

The implementation reuses the current socket reactor and TLS library.

Terminal and signal support can add one reviewed platform dependency.

## 15. Implementation stages

### Stage 0: Baseline and document reconciliation

- Record core compile, load, artifact, suite, and focused runtime measurements.
- Record every planned difference from the active operation tables.
- Record the current ABI and snapshot format versions.

Gate: Each later stage reconciles its active specifications and generated tables.

### Stage 1: General host wait sources (complete)

- Add the manifest wait-source flag.
- Add the checker and bytecode preparation rule.
- Add independent verifier checks.
- Add prepared, armed, ready, committed, and cancelled VM states.
- Replace Boolean host cancellation with explicit outcomes.
- Redesign standard input around a bounded cancellation-safe buffer.
- Add waits for existing Clock, Io, DNS, TCP, and TLS read operations.

Gate: A read-versus-sleep loop loses no bytes under every race order.

Gate: One select safely combines host, mailbox, and drive sources.

Gate: Machine death cancels mixed armed sources and releases every attachment.

Gate: Direct operation benchmarks remain within normal noise.

### Stage 2: Terminal and signals (complete)

- Add terminal and signal manifest entries.
- Add `RawMode` and `SignalStream` resource kinds.
- Implement platform adapters and restoration paths.
- Add `std/term` byte helpers.
- Add a TUI event-loop example.
- Add a checkpoint-on-termination example.

Gate: Every normal exit and Loom fault restores the saved terminal state.

Gate: A losing signal wait preserves its signal.

Gate: Closing a signal stream completes every armed signal wait.

Gate: Active raw mode keeps cleanup handlers for both supported signals.

### Stage 3: File-system completion (complete)

- Add metadata and directory boundary values.
- Expand `FsError` and host mappings.
- Add creation, removal, rename, and durability operations.
- Add scoped standard-library helpers.
- Add invalid-name and crash-durability tests.

Gate: A durable replacement follows the specified sync order.

Gate: A non-UTF-8 directory entry remains visible as an error entry.

### Stage 4: Byte-only console I/O (complete)

- Remove the three text operation identities.
- Add generic core display helpers.
- Keep prompt and buffered line readers over bytes.
- Migrate all examples, tests, rows, policies, and documentation.

Gate: The repository contains no old console operation name.

Gate: Closed output pipes never panic the CLI.

### Stage 5: Pipes and Exec (complete)

- Add typed pipe ends and one child resource.
- Add strict spawn boundary values and limits.
- Add pipe-read and child-wait sources.
- Add cleanup, detachment, and reaping paths.
- Add one checked pipeline example.

Gate: A pipeline handles partial writes, end of input, and child failure.

Gate: Live children and pipe ends report precise snapshot blockers.

### Stage 6: TLS server handshake (complete)

- Add the server configuration and operation.
- Reuse the existing TLS stream resource.
- Add certificate, key, ALPN, and limit checks.
- Add local client-server integration tests.

Gate: Success and failure both consume the submitted TCP stream.

### Stage 7: UDP (complete)

- Add the UDP resource and exact operations.
- Add reactor readiness and cancellation.
- Add complete-datagram boundary values.
- Add a local exchange example.

Gate: Selection never loses or truncates a datagram.

### Stage 8: Release closure (complete)

- Pin console, DNS, and TCP delay semantics.
- Update every normative operation table.
- Run malformed-module and snapshot mutation suites.
- Run supported-platform host tests.
- Record final performance and size measurements.
- Review all new diagnostics for stable terms.

Gate: The full workspace suite passes with its baseline worker count.

Gate: No stage hides cost through test caching or reduced coverage.

## 16. Landing standard

Each new group needs all items in this list:

- one `Error` implementation for each new ordinary error family;
- checker tests for exact rows and denied authority;
- verifier tests for forged argument and result types;
- VM tests for resources, aliases, cleanup, and policy routing;
- host tests for success, failure, limits, and cancellation;
- snapshot tests before, during, and after resource closure;
- one run-pass example with checked output;
- one local integration test with no external internet access;
- focused performance and core-size measurements.

The host test suite must enumerate deterministic race scenarios with scripted hosts.

The scenarios must cover each relevant completion and cancellation order.

It must not depend on timing for correctness.

A program that uses no new group must start no new host worker.

Each stage records core compilation, loading, artifact size, and suite duration.

The measurements use the same worker count and build mode.

## 17. Release ledger

The first release defers these features:

- arbitrary terminal mode flags;
- a terminal resize wait source;
- signals other than interrupt and terminate;
- raw signal numbers and guest signal handlers;
- byte-valued file paths;
- file watching and directory streaming;
- recursive removal as a host primitive;
- shells, pseudoterminals, and job control;
- Unix-domain sockets;
- connected UDP, multicast, and broadcast;
- TLS client certificates and server client authentication;
- TCP_NODELAY setters and other socket options;
- wait-source writes;
- checkpointable external resources.

These additions can use new operations or standard Loom modules later.

They do not require another resource or policy model.

## 18. Integration follow-up

The checkpoint-on-termination example uses `write_all_to` through `FileHandle` conformance.

It flushes and syncs the file before it closes the handle.

`Bytes.from_hex` decodes hexadecimal text with `HexError` results.

`std/base64` provides strict RFC 4648 encoding and decoding.

`std/json` provides bounded JSON parsing and deterministic stringification.

These standard modules link only when source code imports them.
