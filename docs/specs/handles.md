# Resource Handles and Snapshots

Status: accepted design. Week 10 implements the file-handle slice.

## 1. Purpose

A resource handle is a typed guest value. It names one resource entry
outside every guest heap.

The first resource kind is `FileHandle`. Later resource kinds use the
same lifecycle and snapshot rules.

This design supports three service locations:

- the root host can own the backing resource;
- a driver can wrap another handle;
- a driver can supply its own backing state.

## 2. Terms

The **handle value** is the value that guest code stores and passes.

The **resource entry** records the handle kind, state, owner, and
service binding. Guest code cannot forge this entry.

The **resource control** is the holder-local view of one resource
entry. A holder uses it for inspection and closure.

The **backing resource** is the file, memory object, or other state
that services operations.

## 3. Core rules

Each open handle value names one live resource entry.

Aliases name the same resource entry. Closing one alias closes the
entry for every alias.

A closed handle value remains a valid typed value. An operation on it
returns the ordinary closed-resource error.

Closing a handle removes its authority. Closing does not rewrite guest
fields, locals, collections, or suspended frames.

An unreachable closed handle needs no special cleanup. Normal heap
collection removes it.

A resource identifier grants no authority by itself. Only a native
handle value or resource control grants access.

## 4. File operations

The first slice adds these operations:

```text
Fs.Open  (String, OpenOptions) -> Result[FileHandle, FsError]
Fs.Read  (FileHandle, Int) -> Result[Bytes, FsError]
Fs.Write (FileHandle, Bytes) -> Result[Int, FsError]
Fs.Seek  (FileHandle, SeekFrom) -> Result[Int, FsError]
Fs.Flush (FileHandle) -> Result[(), FsError]
Fs.Close (FileHandle) -> Result[(), FsError]
```

The methods on `FileHandle` perform these exact operations. The
methods do not hide or widen their effect rows.

The root host maps one resource entry to one platform file. A custom
host can map the same entry to an in-memory file.

## 5. Handles through `drive`

A driver can answer `Fs.Open` with a file handle that it already
holds. The reply creates another handle value for the same resource
entry.

The child cannot observe the service location. It uses normal `Fs`
operations on the received handle.

```lm
case request.as_call(Fs.Open)
in Some(call)
  vm.answer(call, Ok(parent_file))
in None
  vm.dispatch(request)
end
```

A driver can also mint a new file resource for an `Fs.Open` request.
The runtime binds that resource to the driver.

```lm
case request.as_call(Fs.Open)
in Some(call)
  control = vm.mint_file(call)
  files.push(MemoryFile(control, Bytes(""), 0))
in None
  vm.dispatch(request)
end
```

Later file requests expose the same resource through a holder-local
control. The driver can select its backing state without host access.

`mint_file` consumes only a current `Fs.Open` call. It installs the
successful open reply in the performing machine.

`ReplySink` validates the surface, route, target, ordinal, operation,
and policy cursor before minting starts.

Minting fails atomically. A failed allocation installs no handle and
leaves no resource entry.

## 6. Holder management

The holder-facing surface is:

```text
Vm[T].handles() -> List[ResourceHandle]
Vm[T].resource(FileHandle) -> ResourceHandle
ResourceHandle.is_open() -> Bool
ResourceHandle.close() -> Bool
ResourceHandle.kind() -> String
ResourceHandle.same_resource(ResourceHandle) -> Bool
```

This first slice implements these names.

`handles()` returns every live resource reachable from the controlled
machine world. It includes resources owned by descendants and procs.

The returned controls are holder-local. They are not child
`FileHandle` values and cannot cross a machine boundary.

`close()` is idempotent. It returns `true` only when this call closes
the resource.

`same_resource()` returns `true` only when both controls name the same
live entry. Closed controls never match.

A driver reads a `FileHandle` from `PendingCall.args()`. It converts
the handle with `vm.resource(file)`.

The driver uses `same_resource()` to select the correct backing state.
The comparison grants no resource authority.

Closing a root-host file also releases its host file. Closing a
driver-backed file removes only its resource entry.

The driver owns ordinary backing data. It can retain or discard that
data after closure.

Partial management needs no separate mechanism. A holder can close
selected controls and leave other resources alone.

## 7. Snapshots

The snapshot invariant is:

> Snapshot bytes contain no live resource attachment. They can
> contain closed handle values.

A live resource blocks the snapshot with `ResourceActive`. The error
contains the machine path and resource kind.

The snapshot encoder writes a closed marker for each closed handle.
It never writes a host token, file descriptor, driver binding, or live
resource identifier.

Restore creates no resource entry for a closed marker. Every restored
alias remains closed.

An operation on a restored closed file returns `FsError.Closed`. It
never reopens a path or contacts a previous driver.

A holder can force closure before an immediate snapshot:

```lm
for control in vm.handles() do
  control.close()
end
snapshot = vm.snapshot()
```

Child code can later observe a closed-handle error. The child must
recover if continued execution matters.

## 8. Bounded waiting

`snapshot_wait(fuel)` runs the controlled world at safe boundaries. It
captures the first state without a live resource.

The fuel limit counts retired guest instructions. It does not measure
host time.

The call returns the current `ResourceActive` error when the fuel ends.
It leaves the controlled world valid.

The call also returns when no machine can make progress. It does not
spin while an external completion remains unavailable.

A host application can apply a time limit around repeated fuel-limited
calls. The VM does not depend on a clock.

## 9. Cleanup

Normal `Fs.Close` closes the resource after the service reports
success.

A successful driver answer to `Fs.Close` closes the same resource
entry.

Machine termination closes every resource that the machine owns or
services. It does not invoke guest code.

Cleanup never replaces the machine's original fault.

Pending host operations remain separate attachments. Cancellation and
scoped leases land in later Week 10 slices.

## 10. First implementation slice

This slice includes:

- immutable `Bytes` values;
- the six file operations;
- root-host and in-memory host bindings;
- file-handle transfer through typed replies;
- driver-backed file minting;
- holder enumeration and forced closure;
- live-resource snapshot rejection;
- closed-handle snapshot and restore;
- fuel-bounded snapshot waiting.

This slice excludes `FileLease`, path profiles, directories, TCP,
blocking cancellation, and platform error expansion.
