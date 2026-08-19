# Network Effects, Resources, and Protocol Layers

Status: accepted design. Week 10 implements the DNS and TCP foundation.

## 1. Purpose

This document defines Loom network effects and their host resource model.

The first implementation adds DNS resolution, TCP streams, TCP listeners, and direct HTTP/1.1 support.

The design keeps operating-system network state outside `lm-vm`.

The design also keeps protocol rules in verified Loom code when practical.

## 2. Main decisions

The network foundation uses these decisions:

- DNS and TCP operations have exact manifest identities.
- TCP streams and listeners use the common resource registry.
- The command-line host owns one event-driven socket service.
- Each stream permits concurrent read and write progress.
- TCP reads report data and end-of-stream as different values.
- TCP writes can report partial progress.
- Loom code implements bounded loops and protocol state machines.
- Effect sets give higher layers short public rows.
- Effect sets do not create new runtime operations.
- Direct HTTP uses lower DNS and TCP operations.
- HTTP request bodies and response bodies have explicit limits.
- TLS remains a separate transport layer.
- Live network resources block snapshots.
- Closed network handles remain typed machine state.

## 3. Layer boundaries

### 3.1 Core Loom code

Core Loom code defines these items:

- portable address and error values;
- handle classes and their ordinary methods;
- bounded read and write loops;
- address parsing and formatting;
- HTTP message values;
- HTTP/1.1 serialization and parsing;
- explicit connection and server helpers;
- effect-set names in public signatures.

Core Loom code has no direct access to operating-system sockets.

### 3.2 Exact host operations

Exact operations define the boundary between a machine and its service.

The VM validates each argument before it calls the host.

The host returns plain boundary values or one pending completion token.

### 3.3 Generic VM resource kernel

The VM tracks resource identity, ownership, state, and service location.

The resource kernel contains no socket implementation.

`lm-vm` must not depend on an operating-system network crate.

### 3.4 Root host

`lm-host` owns DNS workers and the socket reactor.

The reactor owns every root-host socket.

The scheduler thread submits commands and consumes completions.

### 3.5 Pure intrinsics

A pure intrinsic can accelerate byte scanning or cryptography.

A pure intrinsic cannot open a connection or inspect host policy.

Every intrinsic keeps the observable Loom result.

## 4. Exact operations

The first network manifest contains these operations:

```text
Dns.Resolve       (String, Int) -> Result[List[SocketAddress], NetError]

Tcp.Connect       (SocketAddress) -> Result[TcpStream, NetError]
Tcp.Listen        (SocketAddress, Int) -> Result[TcpListener, NetError]
Tcp.Accept        (TcpListener) -> Result[Pair[TcpStream, SocketAddress], NetError]
Tcp.Read          (TcpStream, Int) -> Result[TcpRead, NetError]
Tcp.Write         (TcpStream, Bytes) -> Result[Int, NetError]
Tcp.Shutdown      (TcpStream, Shutdown) -> Result[(), NetError]
Tcp.LocalAddress  (TcpResource) -> Result[SocketAddress, NetError]
Tcp.PeerAddress   (TcpStream) -> Result[SocketAddress, NetError]
Tcp.Close         (TcpResource) -> Result[(), NetError]
```

Only exact operations execute `PERFORM`.

`Dns` and `Tcp` are separate namespaces and separate policy groups.

DNS accepts a numeric port. Service-name lookup is not part of this operation.

The host preserves the bounded address order that its resolver returns.

## 5. Effect sets

### 5.1 Purpose

An effect set is a named finite union of exact operations and other effect sets.

An effect set performs no operation.

An effect set gives a public API one stable and concise effect row.

The first manifest defines these sets:

```text
Tcp.Stream = {
  Tcp.Read,
  Tcp.Write,
  Tcp.Shutdown,
  Tcp.LocalAddress,
  Tcp.PeerAddress,
  Tcp.Close
}

Tcp.Listener = {
  Tcp.Listen,
  Tcp.Accept,
  Tcp.LocalAddress,
  Tcp.Close
}

Tcp.Client = {
  Tcp.Connect,
  Tcp.Stream
}

Tcp.Server = {
  Tcp.Listener,
  Tcp.Stream
}

Http.CleartextClient = {
  Dns.Resolve,
  Tcp.Client
}
```

The manifest can add `Tls.Client` and `Http.Client` with the TLS layer.

### 5.2 Transparency

Effect sets are transparent.

The checker expands their transitive exact-operation closure.

These rows are semantically equal:

```text
with Http.CleartextClient

with Dns.Resolve, Tcp.Connect, Tcp.Read, Tcp.Write,
     Tcp.Shutdown, Tcp.LocalAddress, Tcp.PeerAddress, Tcp.Close
```

Code with an effect set can perform every operation in that set.

The set does not restrict a member operation to one protocol implementation.

The policy cannot distinguish an HTTP TCP write from another TCP write.

### 5.3 Namespaces and membership

Each operation has one namespace.

Each operation can belong to multiple effect sets.

The manifest rejects unknown members, cycles, duplicate names, and operation-set name collisions.

A dotted descriptor can name an exact operation or an effect set.

Descriptor resolution uses the manifest. It does not infer the descriptor kind from punctuation.

### 5.4 Rows, interfaces, and hashes

Row inclusion compares normalized exact-operation closures.

An effect variable remains a symbolic row member.

The verifier expands effect sets with the pinned ABI manifest.

Every reachable `PERFORM` must belong to the expanded declared row.

Structural function hashes cover the normalized exact-operation closure.

An interface can retain the written effect-set name as presentation data.

The interface also pins the effect-set manifest digest.

A set membership change moves the ABI digest and each affected interface hash.

### 5.5 Policy tables

Policy tables store exact entries and effect-set entries separately.

An exact entry has precedence over all set entries.

When several set entries match, an explicit block has precedence over every pass.

A pass applies when at least one set passes and no matching set blocks.

The default action blocks when no entry applies.

`mock` accepts only an exact operation.

`clear` removes only the named entry. It does not erase entries from overlapping sets.

`PolicyTable.pass(set)` charges the complete expanded set to the granter's row.

This lookup remains independent of insertion order.

## 6. ABI type expressions

The operation manifest must not add one enum variant for every composite type.

The manifest uses normalized type expressions:

```text
Primitive(Unit)
Primitive(Bool)
Primitive(Int)
Core(SocketAddress)
Core(NetError)
Native(TcpStream)
Native(TcpListener)
List(type)
Tuple(types)
Apply(Option, [type])
Apply(Result, [success, error])
```

Operation identity hashes the complete normalized expression.

The checker, verifier, host boundary, and interface writer use one shared type grammar.

The decoder checks every length before it allocates storage.

## 7. Portable address values

`IpAddress` is a closed core enum:

```lm
enum IpAddress
  V4(bytes: Bytes)
  V6(bytes: Bytes)
end
```

An `IpAddress.V4` payload has exactly four bytes.

An `IpAddress.V6` payload has exactly sixteen bytes.

`SocketAddress` is a final frozen core class:

```lm
final class SocketAddress
  ip: IpAddress
  port: Int
  flow_info: Int
  scope_id: Int
end
```

An IPv4 address has zero flow information and zero scope identity.

A port is in the inclusive range from zero through 65535.

The public constructor validates all fields.

DNS and TCP replies always return validated values.

Address parsing and formatting use ordinary Loom code.

Formatting uses a canonical dotted form for IPv4.

Formatting uses a canonical compressed form for IPv6.

## 8. Portable network errors

`NetError` is a closed core enum:

```lm
enum NetError
  InvalidInput(message: String)
  NameNotFound(message: String)
  Unavailable(message: String)
  PermissionDenied(message: String)
  AddressInUse(message: String)
  ConnectionRefused(message: String)
  ConnectionReset(message: String)
  NotConnected(message: String)
  TimedOut(message: String)
  Closed
  LimitExceeded(message: String)
  Unsupported(message: String)
  Failed(message: String)
end
```

The host maps platform errors into these stable categories.

The host does not expose a portable raw operating-system error number.

`Failed` preserves a bounded diagnostic message for an unmapped failure.

The same platform condition maps to the same category across operations.

## 9. TCP handle types

The core image defines these native classes:

```lm
sealed native class TcpResource
final native class TcpStream < TcpResource
final native class TcpListener < TcpResource
```

Guest code cannot construct these classes directly.

Each open value names one resource entry.

Every alias names the same entry.

Closing one alias closes every alias.

A closed value remains typed machine state.

An operation on a closed value returns `NetError.Closed`.

The VM uses the native tag-to-class bridge for method dispatch.

The checker must not synthesize a permanent special method table for each network handle.

## 10. Generic resource kernel

The world stores one resource table for all handle kinds.

Each entry contains these fields:

```text
ResourceId
ResourceKind
ResourceOwner
ResourceState
ServiceBinding
creation operation
```

`ResourceKind` initially includes `File`, `TcpStream`, and `TcpListener`.

`ServiceBinding` has these forms:

```text
Host(HostResourceToken)
Driver(MachineId)
```

The generic table replaces the file-specific world map.

Typed public helpers can use one generic internal creation path.

Resource creation and the successful operation answer form one atomic transition.

A failed allocation creates no resource and installs no answer.

The creating machine owns cleanup responsibility.

Sending a handle transfers use. It does not transfer cleanup responsibility.

Machine termination closes every resource that the machine owns or services.

Cleanup invokes no guest code.

Cleanup never replaces the original machine fault.

## 11. Driver-backed network resources

A driver can answer a handle-producing operation with an existing compatible handle.

A driver can also create a driver-backed stream or listener for a current typed request.

The VM validates the request identity before it creates the resource.

Later operations on that resource return to the same driver.

The child cannot observe whether the root host or a driver owns the backing state.

The public helpers remain typed:

```text
serve_tcp_stream(current Tcp.Connect or Tcp.Accept call)
serve_tcp_listener(current Tcp.Listen call)
```

Both helpers use the generic internal resource creation path.

## 12. Read semantics

`Tcp.Read` returns one `TcpRead` value:

```lm
enum TcpRead
  Data(bytes: Bytes)
  End
end
```

`Data` always contains at least one byte.

`End` reports an orderly peer shutdown after all queued bytes have been read.

A reset returns `NetError.ConnectionReset`.

A nonpositive maximum returns `NetError.InvalidInput`.

A maximum above the host limit returns `NetError.LimitExceeded`.

One read returns no more than the requested maximum.

The host copies received bytes into immutable `Bytes` storage once.

## 13. Write semantics

`Tcp.Write` can write fewer bytes than its input contains.

An empty input completes with zero.

A nonempty successful write reports positive progress.

The core `write_all` helper repeats partial writes.

`write_all` treats zero progress on nonempty data as `NetError.Failed`.

The helper keeps a bounded view or compact copy of the unsent suffix.

The host never retains a small view of an unbounded byte root.

## 14. Shutdown and close

`Shutdown` is a closed core enum:

```lm
enum Shutdown
  Read
  Write
  Both
end
```

Read shutdown rejects later reads with `NetError.Closed`.

Write shutdown rejects later writes with `NetError.Closed`.

`Tcp.Close` closes both directions and releases the backing socket.

Close is idempotent.

Forced holder cleanup uses the same host close path.

## 15. Concurrency and ordering

One stream can have read and write requests pending at the same time.

Each stream has one FIFO read queue and one FIFO write queue.

Read progress does not wait for write progress.

Write progress does not wait for read progress.

Each listener has one FIFO accept queue.

The reactor preserves completion order within each queue.

The scheduler still decides the order of guest continuation execution.

A second pending request in one direction remains queued.

Queue limits return `NetError.LimitExceeded`.

## 16. Host reactor

The command-line host uses one event-driven socket reactor.

The reactor owns all nonblocking TCP sockets.

The fixed file worker pool does not execute socket reads or writes.

DNS uses a separate bounded worker pool because platform resolution can block.

The reactor accepts commands through a bounded channel.

The reactor sends completions through the common host completion channel.

The scheduler thread never waits on a platform socket.

The reactor enforces global limits for sockets, queued requests, and retained bytes.

## 17. Cancellation and races

The host interface adds cancellation for one pending completion token.

The host interface also adds generic resource closure by kind and host token.

Both operations are idempotent.

Cancellation removes a queued read, write, accept, resolve, or connect request.

Cancellation cannot remove a completion that the host already published.

The scheduler accepts only the first matching completion or cancellation result.

The scheduler ignores every late completion.

Closing a stream cancels its pending read and write requests.

Closing a listener cancels its pending accept requests.

Machine death cancels every pending host operation owned by that machine.

Raw read and accept operations are cancellation-safe.

The reactor buffers bytes accepted before a read cancellation becomes final.

Higher loops can report partial progress when cancellation interrupts them.

## 18. Snapshots and transfer

A live stream or listener is a host attachment.

A live network resource blocks snapshot creation with `ResourceActive`.

The blocker reports its machine path and resource kind.

Snapshot bytes never contain a socket, host token, or driver binding.

A closed handle serializes as a closed marker.

Restore creates no resource entry for that marker.

A restored closed handle returns `NetError.Closed`.

The runtime never reconnects or relistens during restore.

## 19. Core TCP helpers

The core image defines direct handle methods with exact rows:

```text
TcpStream.read(max_bytes) with Tcp.Read
TcpStream.write(bytes) with Tcp.Write
TcpStream.shutdown(direction) with Tcp.Shutdown
TcpStream.local_address() with Tcp.LocalAddress
TcpStream.peer_address() with Tcp.PeerAddress
TcpStream.close() with Tcp.Close

TcpListener.accept() with Tcp.Accept
TcpListener.local_address() with Tcp.LocalAddress
TcpListener.close() with Tcp.Close
```

The core image also defines these ordinary helpers:

```text
write_all(stream, bytes) with Tcp.Stream
read_exact(stream, count) with Tcp.Stream
read_to_end(stream, max_total) with Tcp.Stream
connect_host(host, port) with Dns.Resolve, Tcp.Client
with_connection(address, body) with Tcp.Connect, Tcp.Close, e
```

Every helper has an explicit byte or iteration limit.

No helper reconnects automatically.

`connect_host` tries a bounded resolver result in host order.

It returns the last connection error when every address fails.

## 20. HTTP/1.1 client

The first HTTP layer is direct verified Loom code.

It performs operations from `Http.CleartextClient`.

It does not perform an exact `Http.Request` operation.

The initial client uses one connection for one request.

It sends `Connection: close` unless the caller supplies a stricter valid value.

The client supports content length, chunked transfer coding, and end-of-stream bodies.

The client rejects conflicting body framing.

The client rejects an invalid status line or header field.

Headers use an ordered `List[HttpHeader]`.

A map cannot preserve duplicate fields or field order.

Header-name comparison uses ASCII case folding.

Header values remain bytes until the caller requests validated text.

The request and response types carry explicit limits.

The client does not follow redirects automatically.

The client does not manage cookies automatically.

The client does not read proxy or environment settings.

The client does not decompress a body automatically.

The client does not select certificate policy automatically.

The first layer does not implement HTTP/2 or HTTP/3.

## 21. HTTP server foundation

TCP listening remains an exact lower operation.

Core Loom code parses one bounded request from an accepted stream.

Core Loom code writes one bounded response to that stream.

A server loop accepts connections and starts explicit worker procs.

The caller selects concurrency, limits, shutdown behavior, and handler effects.

The library adds no hidden global server state.

## 22. HTTP effect sets

`Http.CleartextClient` is a transparent effect set.

It gives the direct client one concise public row.

Passing this set grants its lower DNS and TCP operations.

A manual driver observes those lower operations.

The set cannot support one HTTP-level mock or transcript entry.

A later exact `Http.Request` operation remains possible.

Such an operation requires a host service or an effect provider.

The normal client path does not use an isolated machine.

An isolated machine remains available for an untrusted service boundary.

## 23. TLS extension

TLS is a separate transport layer above TCP.

It must not change TCP read or write semantics.

The TLS layer can use a reviewed external Rust library inside `lm-host`.

`lm-vm` still sees only plain boundary values and resource tokens.

Certificate roots, server names, protocol versions, and verification policy remain explicit inputs.

The TLS layer defines its own stream resource and exact operations.

`Tls.Client` includes only those exact TLS operations and required lower effects.

`Http.Client` can unite `Dns.Resolve`, `Tcp.Client`, and `Tls.Client`.

Adding TLS membership changes the effect-set and ABI digests.

The cleartext client remains available with its narrower row.

## 24. Deterministic testing

`RecordingHost` implements a bounded virtual DNS and TCP service.

Tests can register addresses, listeners, incoming bytes, and expected writes.

The virtual service preserves the same partial read and write rules.

Tests can force cancellation and close races at named boundaries.

Driver-backed streams test service replacement without real network access.

Loopback integration tests exercise the real reactor.

External internet access is not part of the test suite.

HTTP tests use local scripted peers and fixed bytes.

## 25. Security and limits

Root policy blocks every network operation by default.

A client effect set excludes `Tcp.Listen` and `Tcp.Accept`.

An operation validates every count before allocation or host submission.

DNS result counts have a fixed upper bound.

Pending request queues have fixed upper bounds.

HTTP header bytes, header count, body bytes, and chunk counts have explicit limits.

The host charges retained immutable byte roots, not only visible views.

The host compacts a small retained view of a large root.

Diagnostics never include unbounded peer data.

## 26. Performance gates

An idle network feature adds no branch to the ordinary interpreter dispatch path.

Effect-set inclusion uses precomputed operation bitsets.

Policy lookup uses dense exact and set slots.

One ready TCP read uses one host completion and one guest byte allocation.

One ready TCP write retains no bytes after completion.

Full-duplex traffic progresses without head-of-line blocking between directions.

Resource cleanup remains linear in the live resource count.

Network benchmarks run separately from interpreter dispatch benchmarks.

## 27. Implementation stages

### Stage 1: Specification and manifests

- Add this sidecar.
- Replace hard-coded composite ABI types with normalized type expressions.
- Add nested effect sets and their hashes.
- Add policy and verifier tests for overlapping sets.

### Stage 2: Generic resources

- Generalize file-specific service bindings.
- Add TCP resource kinds and native class roles.
- Add generic host cancellation and resource closure.
- Preserve all file behavior and snapshot vectors.

### Stage 3: DNS and TCP host path

- Add portable address and error values.
- Add exact DNS and TCP operations.
- Add the DNS worker pool and socket reactor.
- Add deterministic host and loopback tests.

### Stage 4: Core TCP surface

- Add handle methods and bounded helpers.
- Add driver-backed stream and listener construction.
- Add client, server, cancellation, transfer, and snapshot examples.

### Stage 5: HTTP foundation

- Add bounded HTTP values, serialization, and parsing.
- Add the cleartext client and server connection helpers.
- Add effect-set rows, local integration tests, and examples.

### Stage 6: TLS client

- Select and pin one reviewed TLS dependency.
- Add explicit TLS configuration and resource operations.
- Add `Http.Client` without changing `Http.CleartextClient`.
- Add local certificate tests with no external network access.

## 28. Conformance gates

- Every network operation has checker, verifier, host, and policy tests.
- Hand-built bytecode cannot perform an operation outside an expanded row.
- Every effect-set membership change moves the required digests.
- Policy overlap follows exact precedence and set-block precedence.
- TCP read never returns empty `Data`.
- TCP write reports partial progress correctly.
- A blocked read does not delay a write on the same stream.
- Close wakes every pending operation on that resource.
- Cancellation consumes one completion path only.
- Late completions do not resume a machine.
- Live streams and listeners block snapshots.
- Restored closed handles stay closed.
- Client policy never grants listen or accept.
- HTTP parsing enforces every configured bound.
- The full workspace tests, formatting, and lint checks pass.
