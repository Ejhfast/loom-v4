# Week 4 Status

This note records what landed in week 4, the known simplifications,
the changed tests, one incident, and the deferred work.

## Landed

- The `lm-abi` crate: the canonical operation and group manifest.
  Groups: `Io`, `Fs`, `Clock`, `Rand`, `Net`, `Proc`, `Vm`,
  `Compiler`, `Reflect`. Exact operations: `Io.Print`, `Io.Error`,
  `Io.ReadLine`, `Clock.Now`, `Clock.Monotonic`, `Clock.Sleep`,
  `Rand.Int`, and the `Vm` control family (`Vm.New`, `Vm.FromObject`,
  `Vm.Run`, `Vm.Step`, `Vm.Drive`, `Vm.Answer`, `Vm.Reject`,
  `Vm.Dispatch`, `Vm.Table`). Each operation has a stable dense slot
  and a domain-separated SHA-256 identity over its name and full
  signature; `manifest_digest()` pins the whole table. The checker,
  the verifier, the VM, and the host all read this one table.
- The `sys` surface: `sys.<group>.<Member>(args)` is one `PERFORM`.
  `sys.io.Print` as a value is `Op[Io.Print, (String) -> ()]`, a
  16-byte immediate (`Value::Op`) whose identity lives in the type.
  Calling it through a variable emits `PERFORM_VALUE` and charges the
  identity from the static type. Calling an operation object is the
  only guest-to-host boundary.
- Rows validate against the manifest (closes the week-3 observation):
  an unknown operation or group name in a row is `E1050` in the
  checker, and the verifier rejects the same names in claimed rows,
  function-type rows, and application rows.
- Row checking charges real performs: direct performs, statically
  selected callees, higher-order effect variables, and the two
  dependent rules — `PolicyTable.pass(target)` charges the passed
  operation set to the granter's row, and first-class operation calls
  charge the identity-indexed type.
- Effect-variable binding from explicitly rowed function arguments
  (closes the week-3 observation): the inference pass now also
  synthesizes an argument whose declared parameter still holds an
  unresolved effect variable, so
  `apply(x, do |n: Int|: Int with Io.Print ... end)` binds
  `e := {Io.Print}` and the caller is charged the bound row.
- The entry block collects its row instead of rejecting performs: the
  inferred entry row is the program row, and the root policy decides
  at run time. `lm run file.lm --allow Io.Print,Clock` grants exact
  operations or whole groups at the root; the default is deny.
- The independent verifier reconstructs rows: every reachable
  `PERFORM` and `PERFORM_VALUE` must name an operation inside the
  claimed row; `pass` edits must sit inside the claimed row; a mock
  handler must carry the exact operation signature with the empty
  row; first-class `Op` types must equal the manifest signature.
- Bytecode format version 4: `Perform`, `PerformValue`, `OpConst`,
  `TableEdit`, `AsCall`, `CallArgs`, `FaultCode`, and `Unreachable`
  instructions; `Fault`, `Request`, `PolicyTable`, `EmptyVm`,
  `Vm[T]`, `PendingCall[A,R]`, and `Op` type entries. Every new byte
  surface has corruption tests.
- The `case` backstop (closes the week-3 observation): the last arm
  keeps its tests and jumps to an `Unreachable` instruction, which
  the verifier treats as a diverging terminator and the VM turns into
  the stable implementation subcode `UnreachableCode`. The oracle
  models the same backstop.
- The world engine in `lm-vm`: one `World` owns every machine record
  (heap, frames, arenas, fuel, state, one pending-perform record, one
  native policy table, parent link). One driver loop executes an
  explicit activation stack; `run`, `step`, and `drive` are stop
  modes of that loop. Nested machines are activations, so nested VM
  depth never grows the Rust stack (gate: a 60-level tower runs on a
  512 KiB thread). `run` uses the same loop to the terminal and
  allocates no per-instruction event (gate: a counting-allocator test
  bounds a 700,000-instruction run below 100 Rust allocations).
- Machine states `empty`, `ready`, `running`, `asked`, `waiting`,
  `done`, and `faulted` per specification 14.3 (`proc_owned` is week
  8). Illegal transitions fault the caller with `InvalidVmState`
  without mutating the controlled machine. Terminal execution calls
  return the stored event idempotently. Faults surface to holders as
  frozen `Fault` values; `fault.code()` returns the stable code text.
- Policy tables: dense exact and group action vectors, default block,
  `pass`, `block`, pure bounded `mock`, `clear`, and live editing.
  Lookup is exact, then group, then default block. Pass chains walk
  the parent links to the root grants and fail closed with
  `PolicyDenied`. A mock runs in an ephemeral machine on the same
  driver loop under a fixed work budget; a mock fault or budget
  exhaustion faults the controlled guest with `HostFault`.
- Typed manual driving: `drive()` stops before policy lookup and
  returns `Asked(request)`. `request.as_call(op)` returns
  `Option[PendingCall[A,R]]` through the compiler-known identity
  rule; `call.args()` returns the boundary-copied typed argument
  view; `vm.answer(call, value)` is statically typed to the reply.
  Stale, consumed, forged, and cross-VM tokens fault with
  `InvalidRequestToken` without corrupting the controlled machine.
  Repeating `drive` while `asked` mints a fresh token. There is no
  `Answer(Any)` path anywhere.
- The boundary-transfer subset: scalars, `Op` values, and deeply
  frozen graphs of strings, tuples, lists, maps, instances, closures,
  and fault values, with cycles and sharing kept. Builders and native
  handles reject with `UnsendableValue`. `from_object` arguments
  cross through the `args:` tuple envelope; terminal results cross in
  transfer mode, and an unsendable result converts the controlled
  machine to `Fault(UnsendableValue)`.
- The core image gained `core/errors.lm` (`IoError`) and `core/vm.lm`
  (`RunResult`, `StepEvent`, `DriveEvent`) as ordinary source. The
  pin in `core/pinned-hash.txt` moved to
  `abcbc06e8ed7208b523d9444392b583518c32311f8cca69de98c59a5c0069beb`
  (expected churn) and the determinism gate stays green.
- Core linking for the runtime: `lm_bytecode::corelink` resolves the
  pinned core enums inside one module by name and validates their
  exact shape. The verifier and the VM share this one module, so the
  class indices behind operation replies and VM events always agree.
  This is the isolated positional coupling until week-5 hash linking.
- The `lm-host` crate: `CliHost` implements `Io.Print`/`Io.Error`
  over stdout/stderr, `Io.ReadLine` over stdin with the pinned
  `Result[Option[String], IoError]` reply, `Clock.Now`/`Monotonic`,
  `Clock.Sleep` through the asynchronous completion channel
  (synchronous-in-the-CLI for now), and a seeded deterministic
  `Rand.Int` (`--rand-seed`). `lm-vm` never depends on `lm-host`;
  completions carry plain data only. `RecordingHost` in `lm-vm` is
  the deterministic test host and exercises the waiting state.
- `loop [do] ... end` as sugar for `while true`, and the scanner now
  tracks block keywords, so a multi-statement block body inside
  parentheses ends its statements at newlines. Labeled call
  arguments parse; the checker accepts only `args:` on
  `from_object`.
- Class constructor patterns: `Pair(a, b)` and user classes
  destructure the named scrutinee class in declaration order, with
  nested patterns.
- Examples with checked output: `examples/04-effects/hello.lm`
  (prints `Hello Ada!`), `blocked.lm` (`Done("PolicyDenied")`),
  `mock-clock.lm` (`Done(123)`), and `manual-drive.lm`
  (`Done((["tick\n"], 123))`).
- Negative UI examples: `perform-without-row.lm` (`E1046`),
  `unknown-op-in-row.lm` (`E1050`), `answer-type-mismatch.lm`
  (`E1004`), plus the kept `row-widening-override.lm` with real
  operations.
- The seeded-mutation no-panic harness (`lm-testkit/tests/fuzz.rs`):
  400 seeded mutation rounds per input over all eleven examples, for
  module bytes (decode, verify, and bounded run) and for source text
  (scan, parse, check, lower). The PRNG seed is fixed
  (`0x00c0ffee12345678`), so a failure reproduces exactly. The
  permanent corpus in `tests/fuzz-regressions/` replays five crafted
  rejection modules — the two week-3 verifier findings, the two
  week-4 forgery classes, and the local-count bomb below — plus two
  source seeds.
- Perform benchmark smoke checks: exact pass, group pass, block,
  mock, drive interception, nested run, and async wait.

## Incident: unbounded decoder-driven allocation

The first fuzz run took the host down. A mutated `local_count` field
sized two allocations before any bound applied: the verifier dataflow
state (`vec![None; local_count]` per function) and the initial frame
arena in the new `load_frame`. A forged count near `2^32` demanded
tens of gigabytes. The fix: the verifier rejects `local_count` above
65,536 and bounds the dataflow footprint (blocks times locals) before
it sizes any state, and `load_frame` checks the arena limit before it
resizes. The case is the checked-in seed
`tests/fuzz-regressions/local-count-bomb.lmbc`. The build shell now
also caps process address space at 4 GiB (commit 46076fa), so a
future escape fails with an allocation error instead of host memory
exhaustion. The guides record the rule: never size an allocation from
an untrusted length field before a bounds check.

## Review fixes

An independent review confirmed one defect and one documentation gap.

- A continuation method with a consumed token faulted the caller
  with `InvalidVmState`, because the asked-state check ran before
  the token logic. A machine without a pending request means the
  token is consumed or stale, so the fault is now
  `InvalidRequestToken` (specification 12.3). Run and step on an
  asked machine, and a second load of an aliased `EmptyVm`, stay
  `InvalidVmState`.
- The stable code `BadOperationReply` is not produced in this
  slice, and that gap was undocumented. The checker and the
  verifier tie every `answer` value to the typed `PendingCall`
  reply type, and the runtime binds the token to the live pending
  operation, so a wrong-typed reply is unreachable. The code
  arrives when a dynamic reply path exists.

## Simplifications inside the slice

- `Rand.Bytes` is cut: its reply needs the `Bytes` type, which is not
  in the slice. `Rand.Int` invalid ranges and other host failures
  outside a declared ordinary reply fault with `HostFault`;
  `Clock.Sleep` replies `()` instead of `Result[(), ClockError]`, and
  `Rand.Int` replies `Int`. The manifest identity hashes cover the
  simplified signatures, so widening them later is an explicit ABI
  version change.
- Rows keep the canonical-name text encoding inside artifacts,
  validated against the manifest on both sides. `PERFORM` carries the
  dense manifest slot. Hash-based row encoding arrives with week-5
  linking.
- Policy targets are static descriptors (`Io`, `Clock.Now`) resolved
  at check time. A first-class identity-erased `PolicyTarget` value
  does not exist yet, so `block`/`clear` on a computed target is
  deferred with it.
- Groups are not first-class values, `sys.vm.Vm` is not a value, and
  `Op` values exist for fixed host operations only. `Request`
  inspection is `as_call` only; `q.op()`, `q.ordinal()`,
  `q.args_view()`, `q.reply_type()`, `call.reply_type()`, `stack()`,
  `WaitView` payloads, `SetLimits`, and `AddFuel` are deferred.
- The `Fault` surface is minimal: a frozen value with a stable code
  observable through `fault.code()` as text and through display.
  Message, operation, `data`, and `trace` fields exist in the record
  but have no guest accessors yet.
- A child machine receives the default `VmConfig` budget instead of a
  reservation from the parent budget (specification 14.11); machine
  records live until the world drops. Both are revisited with the
  week-6 resource work.
- The mock handler closure is stored in the table owner's heap and
  rooted by the table, not in a separate table-owned heap. The
  observable rules hold: installation boundary-copies the handler,
  the handler runs in a fresh machine with a deterministic budget,
  and its frozen result crosses back by copy.
- Reentrancy is structurally unreachable from guest code this week:
  handles are holder-local and unsendable, and the holder is
  suspended while its child runs. The runtime still counts activation
  references and faults `InvalidVmState` as defense in depth; Rust
  unit tests cover it directly.
- `Io.ReadLine` in the CLI blocks synchronously; only `Clock.Sleep`
  uses the waiting state end to end. The completion channel already
  has the week-9 asynchronous shape (`start`/`poll`/`wait` with
  single-use tokens).
- The differential oracle models the pure subset only. Programs with
  performs are outside the oracle and report a harness limit, not a
  guest outcome. The oracle does not model VMs.
- The verifier accepts a type test between classes with a common
  ancestor (sibling enum cases included), because the exhaustiveness
  backstop emits sibling tests on flow-narrowed values. Tests between
  classes without a common ancestor stay rejected, and the week-3
  generic-argument equality rule is unchanged.
- The one `unsafe` block in the repository is the test-only counting
  allocator behind the `run` allocation gate; it forwards to the
  system allocator and counts.

## Changed tests

Existing expectations changed only where a construct moved from
rejected to supported, plus one stack-size precedent:

- `lm-source`: `loop` moved from reserved to supported;
  `rejects_reserved_keyword` became `parses_loop_as_while_true`.
- `lm-testkit/tests/checker.rs`: the `E1002` family case now uses a
  nested `class`, because no reserved keyword remains.
- `lm-testkit/tests/week3.rs`: the entry now collects its row, so
  `def go() with Io.Print end go()` and the closure and `init` cases
  run to `Done`; the pure-function forms keep `E1046`. Class
  constructor patterns moved from `E1041` to supported.
- `lm-testkit/tests/complexity.rs`: the gates run on the supported
  8 MiB stack (the week-3 note set this precedent); the wall-clock
  bounds are unchanged.
- The core pin moved with the new core files; the determinism and
  prelude-independence gates pass unchanged.

## Deferred work

- Snapshots, `restore`, `from_artifact`, `LinkedEntry`, and the full
  graph codec (weeks 5-7). The transfer subset above is the week-4
  stand-in for the codec.
- Proc ownership, `proc_owned`, and scheduler work (week 8).
- The remaining host operations (`Fs.*`, `Net.*`, `Io.ReadBytes`,
  `Rand.Bytes`) and true asynchronous completions (week 9).
- CI workflow files, Miri, `cargo-fuzz` targets (nightly), and
  committed benchmark distributions; the smoke checks and the seeded
  harness stand in.
- Fuel accounting for intrinsic work beyond one unit per instruction,
  and parent-budget reservation for child machines.
