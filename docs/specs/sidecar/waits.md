# Typed Waits and Selection

Status: accepted design. Week 10 implements the typed waits and `select`.

## 1. Purpose

A wait is a typed request for one later result.

Selection waits for one result from several sources. It does not add another continuation to a proc.

The first public sources are VM drive boundaries and proc mailbox receives.

## 2. Wait values

`Wait[T]` is an opaque native type. `T` is the result type.

A wait value is holder-local. It cannot cross a VM boundary or enter a mailbox.

Each wait is one-shot. Reuse after choice, cancellation, or completion faults with `InvalidVmState`.

The runtime checks ownership and token state. The compiler needs no affine type rule.

Source creation carries the source effect. `drive_wait()` carries `Vm.DriveWait`.

`receive_wait()` carries `Proc.RecvWait`. Choice, waiting, and cancellation carry exact `Wait.*` effects.

A wait type stores no effect row. Source creation delegates an authorized action inside one holder.

## 3. Core operations

The first surface has these operations:

```text
Vm[T].drive_wait()             -> Wait[DriveEvent[T]]
Proc[M].receive_wait()         -> Wait[Recv[M]]
Wait[T].wait()                 -> T
Wait[A].choose(Wait[B])        -> Wait[Choice[A, B]]
Wait[T].cancel()               -> Bool
```

`Choice[A, B]` is an ordinary closed core enum:

```text
enum Choice[A, B]
  First(value: A)
  Second(value: B)
end
```

`choose()` consumes both input roots. It creates one root over both source trees.

`wait()` arms the complete tree. It parks the calling proc until one source commits.

`cancel()` removes one unarmed tree. It returns `true` after successful removal.

Direct control keeps its current result types:

```text
vm.drive()     = vm.drive_wait().wait()
self.receive() = self.receive_wait().wait()
```

These equations define visible behavior. Direct calls can use an internal fast path.

## 4. Select syntax

`select` is typed syntax over `choose()` and `wait()`.

```lm
select
in child.drive_wait() -> event
  handle_drive(event)
in self.receive_wait() -> command
  handle_command(command)
end
```

A select has at least two arms. Each arm expression must have type `Wait[T]`.

The arm name has type `T` inside its body. `_` discards the result.

All arm bodies need one compatible result type. The enclosing callable lists all source and body effects.

The compiler lowers arms to a left-associated `Choice` tree. The runtime registers every leaf in one wait set.

The compiler recognizes no source kind inside `select`. It checks only `Wait[T]`.

## 5. Readiness and commitment

Readiness does not expose a result. Selection commits one winner and withdraws every loser.

The runtime tests sources in arm order when it arms the wait.

A later source change makes the parked proc runnable. The runtime tests all sources when that proc resumes.

Arm order selects the first ready source. A wake event does not reserve a source result.

A receive winner removes one mailbox message. A losing receive leaves every message queued.

A drive winner returns one `DriveEvent`. A losing drive stops child progress at an interpreter boundary.

Selection does not undo completed work. Instructions that retired before withdrawal stay retired.

## 6. Scheduler model

The scheduler stores one wait set for each parked task. One wait set can register several scheduler sources.

Current internal sources include mailbox wake keys and host completion keys. Drive waits can depend on either kind.

One readiness event refreshes its wait set. Completion removes all losing registrations.

The scheduler stays single-threaded. One VM never executes concurrently.

## 7. Drive leases

A drive wait gives the scheduler a temporary execution lease for one holder-owned child.

The scheduler runs that child with the normal instruction quantum. Other procs still run between quanta.

An effect request, terminal result, or fault completes the drive wait.

When another source wins, the scheduler withdraws the lease. It restores stable child control before the arm runs.

No other control operation can use the child during an active lease.

`ReplySink` still validates each reply after a drive event exposes a request.

## 8. Host operations

`Host::start` must not block the scheduler thread.

A host can return `Completed` only when the work finishes without an external wait.

Potentially blocking file and stream work enters a bounded I/O service. Submission returns one completion token.

File tokens route to stable workers. Operations on one file keep FIFO order.

The scheduler polls completions between slices. `Host::wait` runs only when no proc can run.

An in-memory host can complete an operation immediately. It must keep the same reply contract.

## 9. Snapshots

Wait descriptions contain only serializable identifiers and source data. Restore rebuilds scheduler indexes from machine state.

Scheduler queues, channels, mutexes, and worker tasks never enter snapshot bytes.

A live host operation remains an attachment. Immediate capture returns `ResourceActive`.

`Handle[M, R].snapshot_wait(fuel)` parks the caller. The normal scheduler continues the target proc world.

Fuel counts retired target-world instructions. Host completion time does not consume fuel.

The call retries capture after target progress. It returns the last blocker when fuel reaches zero.

## 10. Limits and cleanup

Each machine can hold at most 1,024 live wait records.

Wait creation fails before partial registration when this table is full.

Machine termination removes all wait records. Cleanup invokes no guest callback.

A cleanup failure cannot replace the machine's original fault.
