# Effect Pass-Through and the Driver

Status: problem record. The implementation does not follow it yet.

This document describes a defect in `language-spec.md` sections 13.4
and 13.5, and in the implementation of both. It gives a demonstration
program, the behavior today, and the behavior the language needs. It
proposes a direction. It does not specify the fix.

## 1. Purpose

A driver intercepts every operation of the machine it drives. A driver
does not intercept an operation of a machine one level below that. The
operation passes the driver and reaches the host.

This document records why that is wrong and what must replace it.

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

`pass` breaks the agreement. Specification 13.4 says a child `pass`
"consults the live parent table". The implementation does the same:
`resolve_policy` in `crates/lm-vm/src/world.rs` moves to `m.vm.parent`
and reads that machine's table.

> `pass` reads the holder's advance decision. It never asks the holder.

A live driver is therefore invisible to every machine below its direct
child.

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

### 3.1 Behavior today

```text
$ lm run --show-result nested-drive.lm --allow Vm,Io.Print
from B
intercepted=1 result=7
Done(())
```

P captured the print of A. The print of B reached the host and wrote to
standard output. P held a live drive loop and never saw the request.

### 3.2 Behavior the language needs

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
| Who answers B? | the nearest `mock`, or the host |

## 5. Direction

The rule is one sentence:

> `pass` asks the holder. If the holder drives, the holder receives the
> request. If no holder drives, the walk reads the table and climbs, as
> it does today.

The second branch is the behavior today. A world with no driver
therefore keeps its exact behavior, and an unblocked operation still
reaches the host. The change adds behavior only where a live driver
exists.

The activation stack already holds the chain. When P drives A and A
runs B, the stack records B, then A with its drive mode and its holder,
then P. `deliver_asked` already parks one machine and installs an
`Asked` event into another machine's heap. It builds a token that names
its own target machine.

A fix must answer four questions. This document does not answer them.

1. `resolve_policy` reads no activation state. It needs the chain.
2. `answer` and `reject` take a `Vm[T]` receiver. P holds no handle to
   B. The token names B already, so answering through the token is one
   option. The ownership check must then change from "B is my child" to
   "this request came to me".
3. `dispatch` returns `()` today, and it runs the child. In the program
   above, B runs and finishes inside one `dispatch` call of P. A
   surfaced request from B must reach P, so `dispatch` must return an
   event, or the driving loop must change shape.
4. Specification 14.10 covers reentrancy for a machine and its holder.
   It must also cover a parked machine whose holder is suspended inside
   its own call.

Two costs need measurement. A deep tower walks more levels for each
operation. A surfaced request costs one round trip into guest code.
`docs/notes/benchmarks.md` records the dispatch floor.

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

That proposal is out of scope here, and it needs no rule of its own.
It needs this fix. A driver can only mint for a request it receives,
and today it receives nothing from below its direct child. A parent
that wraps a child gains nothing as soon as the child creates a machine
of its own.

Fix `pass` first. The minting proposal then composes with no special
case, which is the correct test of the fix.
