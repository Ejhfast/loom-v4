# Effect Pass-Through and the Driver

Status: accepted and implemented.

This document records the old defect and its solution. The normative
rules are in `language-spec.md` sections 13.4, 13.5, and 14.7 through
14.10.

## 1. Purpose

A driver intercepts direct requests from its machine. It also
intercepts descendant requests that pass through that machine.

The old implementation intercepted direct requests only. This
document records the defect and the implemented replacement.

## 2. The four table actions

A policy table holds one action for each operation. Read the four
actions as the holder's decision, made in advance:

| Action | Meaning |
|---|---|
| `block` | reject, decided in advance |
| `mock` | answer, decided in advance |
| `clear` | no decision, so the default denies |
| `pass` | ask my holder |

`drive` is the same decision, made live. The two mechanisms then agree,
and the model is one model.

The old `pass` implementation broke the agreement. It always moved to
the parent table and ignored a live driver.

> Apply the current table action first. A pass then asks the holder.

A live driver was therefore invisible below its direct child.

## 3. Demonstration

Three machines. P is the top level. P drives A. A creates B and runs
B. Both A and B perform `Io.Print`.

Save the program below as `nested-drive.lm` to repeat the runs.

```lm
def drive_loop(vm: Vm[Int], mut seen: [String]): ([String], Int) with Vm
  loop do
    case vm.drive()
    in Asked(q)
      case q.as_call(Io.Print)
      in Some(call)
        args = call.args()
        seen.push(args[0])
        vm.answer(call, ())
      in None
        vm.dispatch(q)
      end
    in Done(value)
      return (seen.freeze(), value)
    in Fault(_)
      return (seen.freeze(), 0 - 1)
    end
  end
  (seen.freeze(), 0 - 1)
end

inner = do || : Int with Vm, Io.Print
  sys.io.print("from A\n")
  b = sys.vm.Vm().from_object(do || : Int with Io.Print
    sys.io.print("from B\n")
    7
  end, args: ())
  b.table().pass(Io.Print)
  case b.run()
  in Done(v)  then v
  in Fault(_) then 0 - 1
  end
end

a = sys.vm.Vm().from_object(inner, args: ())
a.table().pass(Vm)
a.table().pass(Io.Print)

seen: [String] = []
out = drive_loop(a, seen)
sys.io.print("intercepted={out[0].len()} result={out[1]}\n")
```

### 3.1 Behavior before the fix

```text
$ lm run --show-result nested-drive.lm --allow Vm,Io.Print
from B
intercepted=1 result=7
Done(())
```

P captured the print of A. The print of B reached the host and wrote to
standard output. P held a live drive loop and never saw the request.

### 3.2 Behavior after the fix

```text
$ lm run --show-result nested-drive.lm --allow Vm,Io.Print
intercepted=2 result=7
Done(())
```

P captures both prints. Neither print reaches the host. A asked its
holder, because A passes `Io.Print`, and P is the holder.

## 4. Authority is not affected

Authority already works at every level. Remove one line from the
program above:

```lm
# a.table().pass(Io.Print)
```

```text
$ lm run --show-result nested-drive-noio.lm --allow Vm,Io.Print
intercepted=1 result=-1
Done(())
```

B faulted with `PolicyDenied`, and `b.run()` returned `Fault`. A passed
`Io.Print` on the table of B, but P declined on the table of A.

Specification 15 therefore holds as written: a top-level row bounds the
operations the whole descendant tower can cause. The defect is in
service, not in authorization.

| Question | Scope today |
|---|---|
| May B perform this operation? | every level decides |
| Who answers B? | the nearest live driver, `mock`, or host |

## 5. Solution

### 5.1 Policy routing

Each `pass` computes the next policy location. This location is the
parent table or the root host.

The policy walk tests the table owner for an active driver. It performs
this test only after that table passes.

A block or mock therefore wins before driver routing. A world without
a live driver keeps its old behavior.

### 5.2 Nested control edges

Each nested `run`, `step`, or `drive` records one explicit control
edge. The edge links the pending parent operation to its direct child.

When a descendant request reaches a driver, the loop parks the
activation chain. It keeps the nested edges from the driven surface to
the performing descendant.

The current `drive` call has completed with `Asked`. The loop therefore
clears the edge from the holder to the driven surface.

The next control call rebuilds the activation chain from the stored
edges. Guest or Rust call depth does not hold this state.

### 5.3 Request routing and `ReplySink`

The public request token names the performing machine. The continuation
receiver names the driven surface.

The runtime creates one internal `ReplySink` for a valid continuation.
It checks these facts once:

- the caller controls the surface;
- the route connects the surface to the target;
- the target holds the current asked request;
- the ordinal matches the request;
- a typed call also matches the operation.

`answer` and `reject` then update the performing descendant. Direct
requests use the same path with equal surface and target identifiers.

`ReplySink` is a small Rust stack value. It allocates no heap object and
adds no check to ordinary execution.

### 5.4 Dispatch continuation

A routed request stores the policy location after the surface pass.
`dispatch` resumes at that location and never reapplies the surface
table.

Another active driver can receive the request during this resumed
walk. A block, mock, or root host can also resolve it.

Nested VM control needs one extra rule. `dispatch` records that control
edge and returns to the driving loop.

The next `drive` rebuilds the driven surface before it runs the child.
This order keeps descendant requests visible to the driver.

### 5.5 Snapshots

Snapshot format 3 stores nested edges, routed targets, and saved policy
cursors. Admission checks the receiver edge and the descendant chain.

A cursor outside the captured world becomes the restoring holder
binding. It restores no old policy table or grant.

### 5.6 Cost

An ordinary instruction takes no new branch. A passed table adds one
Boolean driver test to the existing policy walk.

A routed continuation performs fixed identity and state checks.
`ReplySink` adds no allocation and no graph walk.

A surfaced request still adds one guest round trip. That cost belongs
to the explicit manual policy decision.

## 6. Scope

`mock` stays. It is the advance answer, and it belongs beside the
advance rejection. `mock` also serves a machine that runs under `run`,
which no driver can do. The confusion came from `pass`, not from
`mock`.

Week 15 models every VM and machine-world transition. This change
alters those transitions, so it belongs before that week.

## 7. Why this matters beyond the defect

A separate proposal lets a driver mint a handle for an operation such
as `Fs.Open`. The driver then serves a child from its own file, or from
a filesystem it holds in memory. The child receives a type-conforming
handle and cannot tell the difference.

That proposal is out of scope here. This solution supplies its routing
foundation.

A driver now receives a passed request from every descendant depth.
Minting can use that request without a nested-VM special case.

Reply construction and minted-handle lifetime remain separate design
questions. They do not change pass-through routing.
