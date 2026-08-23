# Network Effects, Resources, and Protocol Layers

Status: accepted design. The network-effects branch implements this foundation.

## 1. Purpose

This document defines Loom network effects and their host resource model.

The release implementation adds DNS, TCP, UDP, TLS, and direct HTTP/1.1 support.

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
- `std.tls` and `std.http` are literal modules.
- Source imports select standard modules and their dependency closure.
- A standard-module import grants no effect.
- Effect sets give higher layers short public rows.
- Effect sets do not create new runtime operations.
- Direct HTTP uses lower DNS and TCP operations.
- HTTP request bodies and response bodies have explicit limits.
- TLS uses a separate stream resource above TCP.
- A TLS handshake consumes its TCP stream after host submission.
- The TLS host supports client and server handshakes.
- TLS policy, roots, names, versions, ALPN, and buffers stay explicit.
- Secure HTTP uses the same verified parser as cleartext HTTP.
- Live network resources block snapshots.
- Closed network handles remain typed machine state.
- UDP receives preserve complete datagrams across wait cancellation.

## 3. Layer boundaries

### 3.1 Pinned core Loom code

Pinned core Loom code defines these items:

- portable address and error values;
- handle classes and their ordinary methods;
- bounded read and write loops;
- the checked `SocketAddress` factory;
- direct DNS and TCP entry helpers;
- effect-set names in public signatures.

The pinned core contains values that operation signatures require.

Core Loom code has no direct access to operating-system sockets.

### 3.2 Standard Loom modules

`std.tls` defines TLS configuration values and client helpers.

`std.http` defines bounded HTTP values, codecs, and client helpers.

Each standard module has one source path, interface, artifact, and module identity.

A `use` path under `std.tls` selects `std.tls`.

A `use` path under `std.http` selects `std.tls` and `std.http` in link order.

The linker includes only modules reachable from the program imports.

The bootstrap catalog compiles each selected module once per process.

A release bundle can replace source compilation with verified decoded artifacts.

The public catalog can bind selected interfaces into an explicit `CompileEnv`.

A runtime compiler must receive that catalog through its compile environment.

The runtime never searches a filesystem or loads a module by an ambient name.

An import changes name resolution and linking only. It grants no effect.

### 3.3 Exact host operations

Exact operations define the boundary between a machine and its service.

The VM validates each argument before it calls the host.

The host returns plain boundary values or one pending completion token.

### 3.4 Generic VM resource kernel

The VM tracks resource identity, ownership, state, and service location.

The resource kernel contains no socket implementation.

`lm-vm` must not depend on an operating-system network crate.

### 3.5 Root host

`lm-host` owns DNS workers and the socket reactor.

The reactor owns every root-host socket.

The scheduler thread submits commands and consumes completions.

### 3.6 Pure intrinsics

A pure intrinsic can accelerate byte scanning or cryptography.

A pure intrinsic cannot open a connection or inspect host policy.

Every intrinsic keeps the observable Loom result.

## 4. Exact operations

The first network manifest contains these operations:

```text
Dns.Resolve       (String, Int) -> Result[List[SocketAddress], NetError]

Tcp.Connect       (SocketAddress) -> Result[TcpStream, NetError]
Tcp.Listen        (SocketAddress, Int) -> Result[TcpListener, NetError]
Tcp.Accept        (TcpListener) -> Result[(TcpStream, SocketAddress), NetError]
Tcp.Read          (TcpStream, Int) -> Result[TcpRead, NetError]
Tcp.Write         (TcpStream, Bytes) -> Result[Int, NetError]
Tcp.Shutdown      (TcpStream, Shutdown) -> Result[(), NetError]
Tcp.LocalAddress  (TcpResource) -> Result[SocketAddress, NetError]
Tcp.PeerAddress   (TcpStream) -> Result[SocketAddress, NetError]
Tcp.Close         (TcpResource) -> Result[(), NetError]

Tls.Handshake     (TcpStream, String, Int, List[Bytes], List[Bytes], Int, Int)
                  -> Result[TlsStream, TlsError]
Tls.ServerHandshake
                  (TcpStream, List[Bytes], Bytes, List[Bytes], Int, Int)
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

Only exact operations execute `PERFORM`.

`Dns` and `Tcp` are separate namespaces and separate policy groups.

DNS accepts a numeric port. Service-name lookup is not part of this operation.

The first DNS name limit is 253 visible ASCII bytes.

The host preserves the bounded address order that its resolver returns.

`Tls.Handshake` uses flattened boundary values from `TlsClientConfig`.

The root mode is zero for WebPKI roots and one for custom roots.

The minimum version is 12 for TLS 1.2 and 13 for TLS 1.3.

The final integer limits retained TLS plaintext and ciphertext buffers.

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

Tls.Stream = {
  Tls.Read,
  Tls.Write,
  Tls.Shutdown,
  Tls.LocalAddress,
  Tls.PeerAddress,
  Tls.Close
}

Tls.Client = {
  Tls.Handshake,
  Tls.Stream
}

Tls.Server = {
  Tls.ServerHandshake,
  Tls.Stream
}

Udp.Socket = {
  Udp.SendTo,
  Udp.RecvFrom,
  Udp.LocalAddress,
  Udp.Close
}

Udp = {
  Udp.Bind,
  Udp.Socket
}

Http.Client = {
  Dns.Resolve,
  Tcp.Client,
  Tls.Client
}
```

`Tls.Client` does not include DNS or TCP connection creation.

It accepts an existing `TcpStream` and adds TLS authority.

`Http.Client` combines all lower client operations for secure HTTP.

`Http.CleartextClient` keeps its smaller operation closure.

The names do not hide runtime requests from a driver.

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

`ResourceKind` includes `File`, `TcpStream`, `TcpListener`, and `TlsStream`.

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
serve_tls_stream(current Tls.Handshake call)
```

The TLS helper replaces the consumed TCP resource with one TLS resource.

All helpers use the generic internal resource creation path.

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

The first host read limit is 16 MiB.

The host copies received bytes into immutable `Bytes` storage once.

## 13. Write semantics

`Tcp.Write` can write fewer bytes than its input contains.

An empty input completes with zero.

A nonempty successful write reports positive progress.

The first host write limit is 16 MiB.

The core helpers submit at most 65535 bytes in one read or write.

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

A later guest close returns `NetError.Closed`.

Forced holder cleanup is idempotent.

## 15. Concurrency and ordering

One stream can have read and write requests pending at the same time.

Each stream has one FIFO read queue and one FIFO write queue.

Read progress does not wait for write progress.

Write progress does not wait for read progress.

Each listener has one FIFO accept queue.

The root host passes the requested backlog to the operating system.

The reactor preserves completion order within each queue.

The scheduler still decides the order of guest continuation execution.

A second pending request in one direction remains queued.

Queue limits return `NetError.LimitExceeded`.

## 16. Host reactor

The command-line host uses one event-driven socket reactor.

The command-line host starts the reactor and DNS workers on the first network operation.

The reactor owns all nonblocking TCP sockets.

The fixed file worker pool does not execute socket reads or writes.

DNS uses a separate bounded worker pool because platform resolution can block.

The reactor accepts operation requests through a bounded channel.

A separate control channel carries only tokens from bounded operations and resources.

The reactor sends completions through the common host completion channel.

The scheduler thread never waits on a platform socket.

The reactor enforces global limits for sockets, queued requests, and retained bytes.

The first global retained network-data limit is 64 MiB.

The limit covers pending values, unread completions, and live TLS configuration state.

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

A live TCP stream, TCP listener, or TLS stream is a host attachment.

A live network resource blocks snapshot creation with `ResourceActive`.

The blocker reports its machine path and resource kind.

Snapshot bytes never contain a socket, host token, or driver binding.

A closed handle serializes as a closed marker.

Restore creates no resource entry for that marker.

A restored closed TCP handle returns `NetError.Closed`.

A restored closed TLS handle returns `TlsError.Closed`.

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

It defines these message values:

```text
HttpHeader(name: String, value: Bytes)
HttpRequest(method: String, target: String, headers: [HttpHeader], body: Bytes)
HttpResponse(status: Int, headers: [HttpHeader], body: Bytes)

HttpLimits(
  max_header_bytes: Int,
  max_headers: Int,
  max_body_bytes: Int,
  max_chunks: Int,
  max_wire_bytes: Int,
  read_chunk_bytes: Int
)
```

It returns this closed error family:

```lm
enum HttpError
  InvalidRequest(message: String)
  InvalidResponse(message: String)
  LimitExceeded(message: String)
  Network(error: NetError)
  Tls(error: TlsError)
end
```

Pure methods serialize and parse complete message bytes.

Stream methods find framing before they wait for more bytes.

The response reader accepts an effect-polymorphic read function.

TCP and TLS wrappers map transport errors into `HttpError`.

`Http.send` performs operations from `Http.CleartextClient`.

`Http.send_secure` performs operations from `Http.Client`.

It does not perform an exact `Http.Request` operation.

The initial client uses one connection for one request.

It sends `Connection: close` and rejects a caller-supplied connection field.

The client supports content length, chunked transfer coding, and end-of-stream bodies.

The client rejects conflicting body framing.

The client rejects an invalid status line or header field.

A status code is from 100 through 599. The first parser rejects codes below 200.

Headers use an ordered `List[HttpHeader]`.

A map cannot preserve duplicate fields or field order.

Header-name comparison uses ASCII case folding.

Header values remain bytes until the caller requests validated text.

Each parser, serializer, and stream helper receives explicit limits.

The header count includes fields that serializers add.

The first parser rejects informational responses.

The client does not follow redirects automatically.

The client does not manage cookies automatically.

The client does not read proxy or environment settings.

The client does not decompress a body automatically.

The secure client receives one explicit `TlsClientConfig`.

The first layer does not implement HTTP/2 or HTTP/3.

## 21. HTTP server foundation

TCP listening remains an exact lower operation.

`std.http` parses one bounded request from an accepted stream.

`std.http` writes one bounded response to that stream.

A server loop accepts connections and starts explicit worker procs.

The caller selects concurrency, limits, shutdown behavior, and handler effects.

The library adds no hidden global server state.

## 22. HTTP effect sets

`Http.CleartextClient` is a transparent effect set.

It gives the direct client one concise public row.

`Http.Client` is the corresponding secure client set.

Passing either set grants its lower exact operations.

A manual driver observes DNS, TCP, and TLS requests separately.

The set cannot support one HTTP-level mock or transcript entry.

A later exact `Http.Request` operation remains possible.

Such an operation requires a host service or an effect provider.

The normal client path does not use an isolated machine.

An isolated machine remains available for an untrusted service boundary.

## 23. TLS client

### 23.1 Public values

TLS is a separate transport layer above TCP.

`std.tls` defines this explicit client configuration:

```text
TlsRoots = WebPki | Custom(List[Bytes])
TlsVersion = Tls12 | Tls13

TlsClientConfig {
  server_name: String,
  roots: TlsRoots,
  alpn: List[Bytes],
  minimum_version: TlsVersion,
  max_buffer_bytes: Int
}
```

`WebPki` uses the root set compiled into `lm-host`.

`Custom` accepts DER certificate values and replaces the WebPKI roots.

No configuration disables certificate or server-name verification.

The minimum version allows TLS 1.2 or TLS 1.3.

The maximum version is TLS 1.3 in this ABI.

The ALPN list preserves caller order.

The buffer limit controls retained plaintext and TLS records.

`std.tls` validation applies these limits:

- A server name contains from 1 through 253 printable ASCII bytes.
- A custom root list contains from 1 through 128 certificates.
- One certificate contains at most 1 MiB.
- All custom certificates contain at most 4 MiB.
- The ALPN list contains at most 32 values.
- One ALPN value contains from 1 through 255 bytes.
- All ALPN values contain at most 4096 bytes.
- The TLS buffer limit is from 1 through 1 MiB.

The host repeats all validation before certificate parsing or allocation.

`std.tls` defines `Tls.handshake` and `Tls.connect_host`.

The pinned core defines the `TlsStream` methods.

These entry points have the following rows:

```text
Tls.handshake(stream, config) with Tls.Handshake
Tls.connect_host(host, port, config) with Dns.Resolve, Tcp.Client, Tls.Client

TlsStream.read(max_bytes) with Tls.Read
TlsStream.write(bytes) with Tls.Write
TlsStream.shutdown() with Tls.Shutdown
TlsStream.local_address() with Tls.LocalAddress
TlsStream.peer_address() with Tls.PeerAddress
TlsStream.close() with Tls.Close
TlsStream.write_all(bytes) with Tls.Stream
TlsStream.read_exact(count) with Tls.Stream
TlsStream.read_to_end(max_total) with Tls.Stream
```

### 23.2 Stream ownership

`TlsStream` is a final native resource class.

A successful handshake replaces one `TcpStream` resource with one `TlsStream` resource.

An accepted host handshake consumes the TCP resource on success or failure.

A pure configuration rejection occurs before host submission and leaves the TCP stream open.

Every alias of the consumed TCP stream becomes closed.

The VM performs resource replacement as one reply transition.

`serve_tls_stream` performs the same transition for a driver-backed handshake.

A live TLS stream blocks snapshots as a host attachment.

A closed TLS value remains typed snapshot state.

### 23.3 Host implementation

`lm-host` uses pinned rustls with its ring provider.

The root host owns each rustls connection and its nonblocking socket.

`lm-vm` sees only the TLS resource kind, a host token, and plain boundary values.

The existing socket reactor drives TLS reads and writes.

TLS does not use the blocking file worker pool.

Each TLS stream has separate FIFO read and write queues.

Read progress does not wait for application write progress.

The handshake returns only after all required client handshake records enter the socket.

`Tls.Write` can accept a partial input when the configured buffer is full.

`Tls.Read` returns the same `TcpRead.Data` and `TcpRead.End` values as TCP.

`Data` never contains zero bytes.

`End` requires a valid TLS `close_notify` after all plaintext data.

A transport end without `close_notify` returns `TlsError.Protocol`.

`Tls.Shutdown` sends `close_notify`, flushes it, and closes the socket write direction.

`Tls.Close` releases the resource without an additional protocol exchange.

### 23.4 Errors

TLS returns this closed error family:

```lm
enum TlsError
  InvalidConfig(message: String)
  Handshake(message: String)
  Certificate(message: String)
  Protocol(message: String)
  Network(error: NetError)
  Closed
  LimitExceeded(message: String)
end
```

Certificate validation failures use `Certificate`.

Other handshake failures use `Handshake`.

TLS record and closure failures use `Protocol`.

Socket failures retain their stable `NetError` category inside `Network`.

Diagnostic text is bounded to 512 Unicode scalar values.

The host never exposes a raw platform error number.

### 23.5 ALPN rule

The generic TLS layer accepts an ordered ALPN candidate list.

This ABI does not expose the selected ALPN value.

A protocol adapter with one wire protocol passes zero or one ALPN value.

The HTTP/1.1 client accepts no ALPN value or the single value `http/1.1`.

It rejects a configuration that can negotiate another protocol.

A later metadata operation can expose selected ALPN without changing stream I/O.

### 23.6 Secure HTTP

`Http.send_secure` performs operations from `Http.Client`.

It uses `Tls.connect_host` and the same bounded HTTP/1.1 codec.

The cleartext and TLS readers call one effect-polymorphic response reader.

The method sends one request on one new connection.

It sends TLS shutdown after the response and always closes the stream.

An HTTP error keeps its HTTP category.

A TLS error appears as `HttpError.Tls`.

The initial client does not pool connections, follow redirects, or negotiate HTTP/2.

### 23.7 Deferred TLS features

This slice uses TCP listeners and adds one TLS server handshake.

It does not add client certificates, certificate pin sets, or session-cache policy.

It does not add certificate reload policy.

These features require new explicit values and operation identities.

They do not require a new VM resource kernel.

## 24. Deterministic testing

`RecordingHost` implements bounded virtual DNS, TCP, and UDP services.

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

Pending data and live TLS configurations share one retained-byte budget.

HTTP header bytes, header count, body bytes, and chunk counts have explicit limits.

TLS roots, ALPN data, and internal buffers have explicit limits.

The host verifies TLS values before it builds a rustls configuration.

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

The streaming chunk scanner resumes at its last complete chunk boundary.

Resource cleanup remains linear in the live resource count.

Network benchmarks run separately from interpreter dispatch benchmarks.

A source that imports no standard module does not compile or link a standard module.

The bootstrap compiler caches each selected standard module for the process lifetime.

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

### Stage 7: Selective standard modules

- Keep operation boundary types and native resources in the pinned core.
- Move TLS configuration and client helpers into `std.tls`.
- Move HTTP values, codecs, and clients into `std.http`.
- Add a bundled module catalog with explicit compile-environment binding.
- Select the transitive standard closure from source imports.
- Cache each selected standard compilation once per process.
- Link only the standard modules reachable from the program.
- Keep TCP-only tests on the core-only compile path.

### Stage 8: TLS server handshake

- Add explicit server certificate and key values.
- Reuse the TLS stream resource and reactor.
- Consume the submitted TCP stream on every result.

### Stage 9: UDP

- Add one UDP socket resource.
- Preserve complete datagrams during selection.
- Add local virtual-host and real-host exchanges.

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
- A submitted TLS handshake consumes its TCP resource on every result.
- TLS configuration limits are enforced in Loom and at the host boundary.
- TLS end-of-stream requires `close_notify`.
- Local certificate tests use no external network access.
- `Http.Client` expands to DNS, TCP client, and TLS client operations.
- A TLS server handshake consumes its TCP resource on every result.
- UDP selection never loses or truncates a datagram.
- Live UDP sockets block snapshots.
- The full workspace tests, formatting, and lint checks pass.
