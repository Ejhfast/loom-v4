# Rust Vertical-Slice Build Order

Status: implementation sequence for language version 0.2  
Cadence: one merged, releasable mainline increment per week  
Reference implementation: stable Rust; bootstrap compiler in Rust; final compiler in the language

This plan does **not** build a scanner, then a checker, then a VM as isolated feature projects. The first week lands a narrow but complete source-to-execution system. Every later week widens or hardens that same path across syntax, static semantics, bytecode, verification, runtime, tools, examples, tests, and performance.

The order follows several practices used by production compiler/runtime projects:

- keep an end-to-end executable “walking system” from the first merge;
- make intermediate forms immutable, printable, and independently verifiable;
- use source-level UI tests, run tests, corruption tests, and benchmarks as first-class products;
- verify untrusted structure at boundaries and cache verified identities;
- fuzz decoders, verifiers, and execution transitions continuously, with differential oracles where possible;
- bootstrap in explicit stages and compare stage outputs;
- profile before introducing clever representations, JITs, or unsafe fast paths.

A week is not complete because a crate compiles. It is complete when a user can run the stated programs, invalid inputs fail precisely, and the new path survives the stated gates.

---

## 1. Repository and dependency shape

```text
Cargo.toml
crates/
  lm-abi/          canonical core/operation/intrinsic/fault manifests
  lm-source/       UTF-8 source, spans, diagnostics, scanner, parser
  lm-hir/          resolved and typed HIR, CFG, pretty printers
  lm-types/        interned types, subtyping, inference, rows
  lm-bytecode/     serialized and decoded instruction formats
  lm-verify/       artifact and bytecode verifier
  lm-value/        Value, TypeId, ObjRef, scalar semantics
  lm-heap/         object table, pages, GC, native shapes
  lm-graph/        freeze, transfer, digest, snapshot traversal
  lm-vm/           frames, interpreter, requests, policy, limits
  lm-host/         root host operations and async completion adapters
  lm-proc/         scheduler, typed mailboxes, pause/resume
  lm-compiler/     source-to-artifact pipeline and bootstrap frontend
  lm-testkit/      UI/run/corruption/conformance harnesses
  lm-cli/          lm check/build/run/test/inspect/disasm/snapshot
core/
  option.lm result.lm ordering.lm pair.lm range.lm
  vm.lm proc.lm errors.lm native.lmi
std/
  list.lm map.lm set.lm text.lm fmt.lm math.lm value.lm path.lm
  io.lm fs.lm time.lm random.lm net.lm json.lm
  vm.lm proc.lm compiler.lm reflect.lm test.lm
compiler/
  source/          final self-hosted compiler
examples/
  01-basics/ ... 20-sandbox-service/
tests/
  ui/ run-pass/ run-fail/ artifacts/ verifier/ snapshots/
  vm-model/ boundary/ proc/ std/ bootstrap/ conformance/ fuzz-regressions/
benches/
  compile/ dispatch/ calls/ allocation/ collections/ perform/
  drive/ nested-vm/ graph/ snapshot/ proc/
fuzz/
  scanner/ parser/ artifact/ verifier/ snapshot/ graph/ vm-transitions/
```

Dependency direction is enforced by Cargo features and a repository check:

```text
core image / std / self-hosted compiler
                 |
          verified artifacts
                 |
CLI + host + proc scheduler + bootstrap compiler
                 |
ABI + source/HIR/types + bytecode/verifier + value/heap/graph/VM
```

`lm-vm` has no filesystem, clock, network, process, or compiler-frontend dependency. `lm-host` never receives a general writable guest pointer. Unsafe Rust is confined to allocation pages, raw-copy helpers, and the optional C shim.

---

## 2. Weekly completion contract

Every week lands all of the following where applicable:

1. a complete vertical production path, not a dormant subsystem;
2. at least two runnable language examples with checked output;
3. minimal source-level negative tests for each new rule;
4. a human-readable dump for every new intermediate or external format;
5. corruption tests for every newly accepted byte format;
6. a focused benchmark for every changed hot path;
7. fuzz seeds or properties for every new parser/verifier/state-machine surface;
8. documentation beside the implementation, including invariants in unsafe modules.

Unsupported syntax or semantics reject explicitly. No success path silently falls back to an AST interpreter, unchecked bytecode, ambient host access, or a placeholder `Any` API.

---

# Part I — A real language system in four weeks

## Week 1 — Narrow end-to-end compiler and VM

### Land

- Rust workspace, CI on Linux/macOS/Windows, formatting/lints, Miri job for low-level crates, and a small `xtask` for generated ABI tables.
- UTF-8 source reader, spans, scanner, recursive-descent/Pratt parser, deterministic diagnostics, and a printable AST.
- A deliberately narrow language slice: integers, booleans, strings, locals, arithmetic/comparison, `if`, `while`, top-level functions, direct calls, `return`, and a trailing entry expression.
- Interned primitive/function types, bidirectional local checking, typed HIR, basic blocks, and a printable CFG.
- Compact serialized bytecode, fixed decoded instructions, verifier, explicit frames/operand arena, one interpreter loop, instruction fuel, and terminal `Done`/`Fault`.
- `lm check`, `lm run --show-result`, `lm disasm`, and a UI/run test harness from day one.
- Baseline 16-byte `Value`, full-width `Int`/`Float`, immutable strings, code slots, and a temporary non-collecting page arena with a hard heap cap. The arena is an intentional one-week implementation behind the final heap API, not a second runtime.

### Runnable outputs

```lm
# examples/01-basics/factorial.lm
def factorial(n: Int): Int
  if n <= 1
    1
  else
    n * factorial(n - 1)
  end
end

factorial(10)
```

```text
$ lm run --show-result examples/01-basics/factorial.lm
Done(3628800)
```

```lm
# examples/01-basics/control.lm
x = 0
sum = 0
while x < 100
  sum = sum + x
  x = x + 1
end
sum
```

```text
$ lm run --show-result examples/01-basics/control.lm
Done(4950)
```

```text
$ lm check tests/ui/type-mismatch.lm
error[E1004]: expected Int, found String
  --> tests/ui/type-mismatch.lm:2:5
```

`lm disasm` prints function signatures, block boundaries, stack effects, and resolved jump targets for the same files.

### Gates

- Every executed function has passed the independent verifier.
- 100,000 guest recursive calls either hit the configured frame limit or complete without Rust-stack growth.
- Parser, bytecode decoder, and verifier have initial fuzz targets.
- Benchmarks record parse/check/emit time, dispatch instructions per second, direct-call cost, and operand/frame growth.
- No later week is allowed to replace this pipeline with a separate “real” compiler or VM.

---

## Week 2 — Objects, collections, closures, and a real heap

### Land

- Per-VM object table, segmented bump pages, stop-the-VM mark/sweep, scoped host roots, generation-checked `ObjRef`, and native shape descriptors.
- Classes, fields, defaults, `init`, `self`/`mut self`, direct and virtual method calls, single inheritance skeleton, sealed selector tables, and closures with frozen capture descriptors.
- First-order native generic applications plus an initial `List[T]`/`Map[K,V]` surface (`len`, `at`, `push`, `has`, `put`) that does not yet depend on `Option`; also `StringBuilder`, `ByteBuffer`, interpolation, literals, indexing, and the frozen-write barrier. User-declared generics and the final pinned collection interfaces arrive in Week 3 on the same representation.
- Runtime class/type/selector/field slots resolved at load; no textual method lookup in the loop.
- HIR/bytecode/verifier support across the whole slice, plus heap/object/frame dumps in `lm inspect --live` test mode.

### Runnable outputs

```lm
class Counter
  value: Int = 0

  def add(mut self, n: Int): Int
    self.value = self.value + n
    self.value
  end
end

c = Counter()
c.add(2)
c.add(3)
```

```text
$ lm run --show-result examples/02-objects/counter.lm
Done(5)
```

```lm
words = ["red", "blue", "red", "green", "blue", "red"]
counts: {String: Int} = {}

i = 0
while i < words.len()
  word = words.at(i)
  if counts.has(word)
    counts.put(word, counts.at(word) + 1)
  else
    counts.put(word, 1)
  end
  i = i + 1
end

counts
```


```text
$ lm run --show-result examples/02-objects/counts.lm
Done({"red": 3, "blue": 2, "green": 1})
```

A third example stores closures in a list, selects one, and calls it to prove closure conversion and `CALL_VALUE` are on the production path.

### Gates

- Allocation/collection stress runs cyclic graphs under a small heap limit without leaks or recursive tracing.
- Frozen field/list/map writes fault uniformly.
- Virtual dispatch is class-slot plus selector-slot indexed lookup.
- `List.push`, field load/store, virtual call, allocation, and full GC have committed benchmark distributions.
- Miri covers slot generations, page lifetimes, and scoped roots.

---

## Week 3 — Complete static core and pinned core image

### Land

- Generic classes/functions/methods, tuples, enums, constructor patterns, exhaustive `case`, nominal inheritance, override checks, definite field initialization, flow refinement with `is`, and `Never` joins.
- Effect-row representation and syntax throughout signatures/HIR/artifacts, initially exercising empty rows plus higher-order row variables; Week 4 adds actual performs.
- Final interned type DAG, memoized subtyping, first-order generic argument inference, recursive SCC checking, and typed CFG joins.
- Pinned source-defined core image: `Option`, `Result`, `Ordering`, `Pair`, `Range`, VM/proc event declarations, portable errors, and complete sealed native-class interfaces, and correct baseline bodies for every core method. Complete the `List`/`Map` methods whose signatures use those core types without changing the Week 2 storage representation.
- Prelude resolution as a name-import layer distinct from core identity.
- A small test-only typed-HIR evaluator for the pure subset. It is not shipped and exists solely as a differential oracle for compiler/VM testing.

### Runnable outputs

```lm
enum Expr
  Number(value: Int)
  Add(left: Expr, right: Expr)
  Neg(value: Expr)
end

def eval(expr: Expr): Int
  case expr
  in Number(n) then n
  in Add(a, b) then eval(a) + eval(b)
  in Neg(v)    then 0 - eval(v)
  end
end

eval(Add(Number(40), Neg(Number(-2))))
```

```text
$ lm run --show-result examples/03-types/expr.lm
Done(42)
```

```lm
def choose[T](value: Option[T], fallback: T): T
  value.value_or(fallback)
end

(choose(Some("yes"), "no"), choose(None, "no"))
```

```text
$ lm run --show-result examples/03-types/generics.lm
Done(("yes", "no"))
```

Negative UI examples show non-exhaustive enums, escaping uninitialized `self`, invariant `List` mismatch, ambiguous generic calls, and an override that widens a row.

### Gates

- Core image recompiles byte-for-byte identically on all CI hosts.
- Host ABI generation refers to pinned core definition hashes rather than duplicate Rust enums.
- Pure generated programs produce identical terminal values in the HIR oracle and verified bytecode VM.
- Checker complexity tests reject pathological inputs without exponential search.
- No core identity depends on prelude membership.

---

## Week 4 — Operations, policy, and all three VM driving modes

### Land

- Canonical operation/group manifest; generated `sys` object; identity-indexed `Op`; `PERFORM`; row checking against direct/callee/higher-order effects; independent verifier row reconstruction.
- Dense exact/group policy arrays with default block, transitive `pass`, pure `mock`, and live table editing.
- Public native `Vm`/`Run[T]`, typed load/restore transitions, `step`, terminal `run`, `drive`, states, wait completions, stack views, fuel/limits, reentrancy checks, and one internal stop-mode interpreter loop.
- The typed request pattern `Call(op, call, args)`; typed `answer`; token-checked `reject`/`dispatch`; no `Answer(Any)` path.
- Initial host operations include byte I/O, clocks, sleep, and deterministic `Rand.Int`.
- Async completion channel with no Rust reference into guest memory.

### Runnable outputs

```lm
def greet(name: String) with Io.Write
  print("Hello #{name}!\n")
end

greet("Ada")
```

```text
$ lm run examples/04-effects/hello.lm --allow Io.Write
Hello Ada!
```

```lm
vm = sys.vm.Vm().activate_or_fault(do || with Io.Write, Clock.Now
  print("tick\n")
  sys.clock.now()
end, args: ())

captured: [String] = []
loop do
  case vm.drive()
  in Asked(q)
    case q
    in Call(Io.Write, call, (bytes,))
      text = bytes.utf8().expect("the output is UTF-8")
      captured.push(text)
      vm.answer(call, Ok(bytes.len()))
    in Call(Clock.Now, call, ())
      vm.answer(call, 123)
    in _
      vm.dispatch(q)
    end
  in Done(value) then return (captured.freeze(), value)
  in Fault(_)    then return (captured.freeze(), -1)
  end
end
```

```text
$ lm run --show-result examples/04-effects/manual-drive.lm --allow Vm
Done((["tick\n"], 123))
```

A blocked-print example terminates with `PolicyDenied`; a mock-clock example runs automatically with no manual path.

### Gates

- `run` shows no public event allocation per instruction in allocation profiling.
- Perform benchmarks cover exact pass, group pass, block, mock, `drive`, and async wait.
- A typed reply mismatch is rejected by the checker; forged/stale/cross-VM tokens fault safely at runtime.
- Effect-row conformance includes first-class operations, higher-order rows, overrides, and transitive grants.
- State-machine property tests enumerate legal and illegal transitions.

---

# Part II — Closed artifacts and machine state as data

## Week 5 — Deterministic artifacts, hashes, and linking

This week builds code and artifact identity on the single-file
pipeline. Packages and the multi-file build loop follow in week 6.

### Land

- The agreed surface amendments, before new code accumulates: callable
  `sys` members move to snake_case (`sys.io.write`, `sys.clock.now`;
  `sys.vm.Vm()` stays the one capitalized constructor), `use` becomes
  a keyword with the fixed-binding alias form (`use sys.vm`), and
  A `Call` pattern names an exact `Operation` descriptor.
- The sectioned artifact container: a semantic region (code,
  constants, signatures, rows, types), an export section, and a debug
  section, with atomic writes. The container hash covers exact bytes;
  the module semantic hash covers the semantic region only.
- Definition hashes per specification 3.7: canonical bytecode and
  constants, full signature and row, referenced definition
  identities, and the compiler ABI version. SCC hashing for mutually
  recursive definitions: canonical order, one component hash,
  domain-separated member hashes.
- Interface emission: exports with signatures and definition hashes.
- Hash linking replaces the week-4 name-based `corelink`: core
  references resolve by pinned definition hash in the verifier and
  the VM, and the positional per-module core copy retires.
- Verified-code cache keyed by semantic hash plus ABI/verifier
  version.
- `lm build file.lm` emits the artifact and interface with printed
  hashes; `lm run <path>.lma` executes a prebuilt artifact.
- Corruption-focused byte readers shared by artifact/snapshot work.

### Runnable outputs

```text
$ lm build examples/01-basics/factorial.lm
built factorial  sem=2cf4… container=91ab…
$ lm run build/debug/factorial.lma --show-result
Done(3628800)
```

The artifact is the deployment and sandbox unit and runs with no
source present. Editing only a comment changes the container hash
and leaves every semantic definition/module hash unchanged; the
demonstration prints both hash sets before and after the edit.

A rebuild with unchanged inputs reports a verified-code cache hit
and skips re-verification.

### Gates

- Truncated, overlong, duplicate, reordered, hash-mismatched, and type-incompatible artifacts reject before allocation-heavy work.
- Reproducible artifact bytes across builds and hosts.
- Semantic hashes ignore comments, formatting, and debug sections;
  the container hash covers exact bytes.
- Verified code is never re-verified under the same hash/ABI cache key.
- Core references resolve by hash; no name-based or positional core
  lookup survives in the verifier or the VM.
- The `use` alias form never grants authority and never changes an
  effect row.

---

## Week 6 — Packages, modules, and the build loop

The developer ergonomics of this week follow `docs/specs/sidecar/packages.md`:
the package layout, the minimal TOML manifest with path dependencies,
modules from files, and the `use` declaration over interfaces.

### Land

- The module tree from files: `src/geometry/shapes.lm` is the module
  `geometry.shapes`; per-module compilation against dependency
  interfaces; `src/main.lm` holds the program entry.
- `use` for own modules and dependency packages: each cross-package
  path compiles to a named import slot that the build graph fulfills
  from the pinned interfaces.
- The `lm.package` manifest: a strict hand-written TOML subset
  (documented), path dependencies only, the dependency key as the
  local name.
- `lm new` scaffolding; the dependency DAG; the content-addressed
  build directory with cache hits; `lm build`/`lm run` on packages.
- Explicit `CompileEnv` and `LinkEnv` values; import slots with
  signatures, rows, and pinned hashes; and a pure build linker.
  Dynamic access uses `DynValue` in week 13. The build tool constructs
  these values. Ordinary development never uses them directly.

### Runnable outputs

```text
$ lm new hello
$ cat hello/lm.package
[package]
name = "hello"
version = "0.1.0"
```

A two-package workspace shows the loop end to end:

```text
examples/05-modules/
  mathlib/
    lm.package
    src/matrix.lm
  app/
    lm.package          # mathlib = { path = "../mathlib" }
    src/main.lm         # use mathlib.matrix
```

`matrix.lm` exports ordinary definitions; `main.lm` binds them with
`use mathlib.matrix` and the build graph fulfills the import slot
from the pinned interface.

```text
$ lm build examples/05-modules/app
built mathlib  2cf4…
built app      91ab…
$ lm run examples/05-modules/app --allow Io.Write
Hello Ada!
$ lm run build/debug/app.lma --allow Io.Write
Hello Ada!
```

`lm run <package>` is sugar for build plus artifact execution; both
paths admit code through the same verifier. A second build with
unchanged inputs reports cache hits.

A runtime-compilation example supplies provider modules through
`CompileEnv`. It compiles and verifies a module. It installs that
module with `LinkEnv`, requests a typed entry, and runs it.

### Gates

- Linking installs no global names and performs no host operation.
- Build-cache and verified-code-cache responsibilities remain separate.
- A dependency-name collision is a compile error with the manifest
  rename as the stated fix; resolution never picks silently.
- The `use` declaration never grants authority and never changes an
  effect row.
- Editing one module rebuilds only the packages whose interfaces
  change.

---

## Week 7 — Graph consolidation, resource policy, and snapshot foundations

The current VM already has iterative collection, deep freeze,
cycle-preserving transfer, and one activation stack for nested VMs.
This week consolidates those paths. It does not replace them.

### Land

- Add `lm-graph` with one non-recursive traversal engine and one
  deterministic child-order contract per native shape.
- Move the existing mark, deep freeze, and transfer/copy paths behind
  that contract.
- Build canonical digest, frozen verification, detached inspection,
  and snapshot traversal as new modes of the same engine. No digest of
  runtime values exists today.
- Keep the current heap, three-pass transfer behavior, and nested VM
  driver as migration oracles until the new paths match them.
- Add a cyclic and shared-subgraph transfer test before the migration
  starts. The current transfer tests cover flat graphs only.
- Preserve cycles and sharing; add canonical traversal ordinals,
  bounded work tables, stable map semantics, and digest caching on
  frozen objects.
- Add object, edge, byte, and work limits to each graph mode.
- Make transfer and copy failure-atomic for the destination heap.
- Resolve transferred code and classes through verified semantic
  identity. Keep numeric slots local to one linked program.
- Classify every native shape as machine state or host attachment.
  Machine state can enter snapshot bytes; a live host attachment
  blocks the snapshot instead.
- Classify every suspending host operation the same way. No live
  callback enters snapshot bytes.
- Add a host-side resource registry to each VM. Record resource kind,
  owning VM, scope identity, and pending operation ordinal.
- Separate serializable `VmState` from policy, execution ownership,
  active host work, and resource control state.
- Add parent resource reservation for nested VM creation.
- Add brace closures and trailing closure arguments. Lower both
  closure spellings to one typed HIR node and one bytecode form.
- Keep `Vm.activate`, terminal publication, and nested VM examples
  on the production path throughout the migration.

### Runnable outputs

```lm
class Node
  value: Int
  next: Option[Node] = None
end

# A test helper creates a cycle and computes its stable digest.
```

```text
$ lm run --show-result examples/06-graphs/cycle-digest.lm
Done(6f58…)
```

```lm
def apply_twice(f: (Int) -> Int, value: Int): Int
  f(f(value))
end

apply_twice({ |x: Int| x + 1 }, 40)
```

```text
$ lm run --show-result examples/06-graphs/brace-closure.lm
Done(42)
```

The existing nested-sandbox example runs unchanged. A migration test
compares the old and new transfer outputs before the old path retires.

### Gates

- One graph-shape definition controls child reachability and order for
  every mode.
- Deep graphs never recurse on the Rust stack.
- Freeze and transfer retain their current observable behavior.
- Copy and transfer preserve cycles and sharing.
- Digest is stable across allocation order and host process runs.
- Code and classes cross by verified semantic identity.
- Every graph mode rejects work past its published limits.
- A failed transfer leaves the destination heap unchanged.
- Every native shape and suspending operation declares one snapshot
  classification.
- Parent resource reservation is fail-atomic.
- Both closure spellings produce identical typed HIR and bytecode.
- Nested VM depth still does not multiply interpreter loops or Rust
  stack depth.

---

## Week 8 — Procs, mailboxes, and scheduler barriers

Logical proc state lands before snapshot bytes. This order keeps the
snapshot format from defining scheduler semantics by accident.

### Land

- Add the proc scheduler, scheduler/holder ownership transfer,
  `Handle[M,R]`, `Proc.Run`, and compiler `spawn` sugar.
- Add stable proc references with identity and generation. Preserve
  `M` and `R` across every transfer.
- Make proc handles sendable through the graph codec.
- Add bounded FIFO mailboxes, `send`, `receive`, `close`, `done`,
  pause/resume, and dead-peer results.
- Keep one VM per proc, one logical guest thread, no shared mutable
  guest memory, and live parent table chains.
- Add scheduler completion and pause channels using proc IDs and
  ordinals rather than guest references.
- Add deterministic scheduler mode, proc tracing, and mailbox metrics.
- Add one snapshot barrier. It pauses the root and each reachable
  machine at a safe boundary, then closes the set over the handles in
  their captured state.
- Freeze mailbox acceptance for the closed set at one cut marker.
- Serialize every control call on a machine through the scheduler, so
  a barrier never races a holder.
- Run resource-registry preflight across the whole paused set.
- Resume every paused machine after a barrier failure.

### Runnable outputs

```lm
enum Work
  Double(value: Int)
end

class Worker < Proc[Work]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(Double(n)) then n * 2
    in Closed         then 0
    end
  end
end

h = Worker.spawn()
h.send(Double(21))
h.done()
```

```text
$ lm run --show-result examples/07-concurrency/worker.lm --allow Proc
Done(Ok(42))
```

A second example sends one handle through another proc mailbox. The
handle still targets the original proc. A barrier example pauses a
root, a worker, and a helper found only through a mailbox message.

### Gates

- Message and result types never erase to `Any`.
- Handle transfer preserves the exact proc reference.
- The barrier set is closed: every handle in the paused state targets
  a paused machine.
- FIFO acceptance, close/drain, pause/resume, parent death,
  revocation, and dead-peer behavior have deterministic model tests.
- Mailbox limits are checked before copy and acceptance.
- One VM never executes concurrently.
- The barrier records one consistent mailbox cut.
- Barrier failure resumes every paused machine.
- Scheduler records contain no guest heap reference.
- Proc send/receive, spawn, pause, and terminal publication benchmarks
  are committed.

---

## Week 9 — Machine-world snapshots, restore, and branching execution

A snapshot copies the machine world reachable from its root. The
world is closed under the handles it contains, so a reference cannot
leave it. Restore builds a complete independent copy. The plan
carries no ownership records, no external references, and no restore
bindings, because the closed world makes them unnecessary.

### Land

- Add a canonical snapshot writer over trusted state and an external
  snapshot loader/verifier.
- Convert external bytes once to trusted `VmSnapshot`; retain typed
  `RunSnapshot[T]` result casting.
- Capture the root and every reachable machine as one snapshot world,
  using the Week 8 barrier.
- Serialize each machine heap, code/class/type manifests, frames,
  locals/operands, limits/fuel, state, and pending request.
- Serialize mailbox types and limits, accepted queues, close state,
  blocked receives, terminal proc results, and holder-paused state.
- Assign canonical machine ordinals. Encode every handle by ordinal
  and static type.
- Relocate every handle during restore, including handles in heaps,
  frames, captures, mailboxes, pending arguments, and results.
- Preflight every registry entry. A live host attachment returns
  `ResourceActive` with its bounded machine path.
- Return ordinary typed errors for the two snapshot blockers and the
  restore limit. No resource ever becomes an inert guest value.
- Give every restored machine a fresh default-deny table. Restore
  internal pass chains against the new parent tables.
- Keep restored procs stopped behind one world gate until the root
  resumes.
- Restore between-instruction, `asked`, terminal, holder-paused, and
  receiverless self-snapshot states. A machine in `waiting` blocks
  the snapshot instead.
- Support multi-shot restore. Each restore is a complete independent
  world.
- Add `lm snapshot verify/run`, snapshot-aware `lm inspect`,
  source-mapped `stack()`, and deterministic snapshot diffs.

### Runnable outputs

```lm
def restore_run(snap: RunSnapshot[Int]): Int with Vm
  case sys.vm.Vm().restore(snap)
  in Ok(restored)
    case restored.run()
    in Done(value) then value
    in Fault(_)    then -1
    end
  in Err(_) then -2
  end
end

vm = sys.vm.Vm().activate_or_fault({ || 20 + 22 }, args: ())
vm.step()
case vm.snapshot()
in Ok(snap) then (restore_run(snap), restore_run(snap))
in Err(_)   then (-3, -3)
end
```

```text
$ lm run --show-result examples/08-snapshots/branch.lm --allow Vm
Done((42, 42))
```

A machine-world example captures a root, a worker, and a helper the
worker reaches through a stored handle. Each restore gets its own
complete copy of all three. The original three continue unchanged.

A manual-drive snapshot restores the same operation and reply type.
The holder calls `drive()` to obtain a fresh request token.

```text
$ lm snapshot verify checkpoints/asked-tree.lms
valid: state=asked machines=3 mailboxes=2
```

### Gates

- Snapshot round trips cover every bytecode boundary in the example
  corpus.
- Machine ordinals are deterministic and independent from scheduler
  IDs.
- Every handle in snapshot bytes targets a captured machine.
- Handle relocation covers every VM and mailbox root.
- Multi-shot restore creates complete independent worlds. Nothing is
  shared between two restores, or with the original.
- Policy tables and root grants never enter snapshot bytes.
- A failed restore exposes no partial world.
- A failed snapshot resumes the original world.
- The loader checks machine references, limits, and lifecycle records.
  `VmSnapshot` is the admitted host state, and `Image` is the
  editable decoded state.
- Whole-image structural verification occurs once on external load.
  Admission proves structure. The interpreter tests each value tag, and
  the world checks each VM boundary
  (`docs/specs/sidecar/snapshot-image-admission.md` section 5.2).
- In-process trusted restore and external byte load remain separate
  APIs.
- Snapshot size/load/write benchmarks are tracked by workload shape.

---

# Part III — A practical distribution by Week 13

## Week 10 — Handles, waits, filesystem, and network effects

Week 10 starts with the handle foundation in
`docs/specs/sidecar/handles.md`. Later slices add scoped leases, broader host
operations, and TCP.

The collection extension follows
`docs/specs/sidecar/collections-and-iteration.md`. It adds native `Option`,
nominal interfaces, iteration, core collections, and collection views.

### Land

- Add the full operation manifest for I/O, filesystem, clock, random,
  TCP, and optional process environment/current-directory operations.
- Add Unix and Windows platform adapters with ordinary portable error
  enums.
- Add typed resource-registry entries for every live host resource and
  pending host continuation.
- Add typed handle values and holder-local resource controls.
- Add holder-local, one-shot `Wait[T]` values.
- Add waits for VM drive boundaries and proc mailbox receives.
- Add choice, waiting, and cancellation operations for typed waits.
- Add general `select` syntax over two or more `Wait[T]` expressions.
- Park each proc on one scheduler wait set.
- Let a drive wait lend temporary child execution to the scheduler.
- Withdraw every losing drive lease at an interpreter boundary.
- Let a holder enumerate and close resources in its controlled world.
- Let a driver return an existing handle or mint a driver-backed
  handle for a current typed request.
- Add pure `std/path` and explicit finite root policy profiles.
- Add `FileLease` as a scoped native designator and
  `std/fs.with_open` as the standard file entry point.
- Close a lease before a normal callback return. Close every remaining
  lease when its VM terminates, without a guest callback.
- Preserve the original machine fault when cleanup also fails.
- Add `std/fs.open_handle` for deliberate long-lived ownership.
- Keep explicit read, write, seek, flush, and close on `FileHandle`.
- Register every live raw file handle as a host attachment. A live
  attachment blocks snapshot creation.
- Keep closed handle values as ordinary machine state. Restored closed
  handles remain closed.
- Add `Handle.snapshot_wait(fuel)` for transient target-proc resources.
- Count only retired target-world instructions against snapshot fuel.
- Let known host completions extend elapsed time without consuming fuel.
- Register live TCP streams and listeners the same way. Do not reopen
  a connection silently.
- Add transparent effect sets for TCP, TLS, and HTTP clients.
- Normalize each set to its transitive exact-operation closure.
- Include set membership in the operation manifest digest.
- Add bounded DNS workers and one nonblocking TCP reactor.
- Keep sockets and TLS state outside `lm-vm`.
- Add `TcpStream`, `TcpListener`, and `TlsStream` native resources.
- Let a typed driver create each network resource for a current call.
- Add explicit TLS roots, server names, versions, ALPN, and buffers in `std.tls`.
- Consume the TCP resource after each submitted TLS handshake.
- Use pinned rustls only inside `lm-host`.
- Add bounded HTTP/1.1 parsing and serialization in `std.http` Loom code.
- Use one response parser for TCP and TLS readers.
- Keep cleartext and secure HTTP effect sets separate.
- Defer checkpointable file and connection types with explicit
  restore contracts to a later version.
- Report precise resource blocker paths along reachability from the
  snapshot root.
- Add cancellation for blocking reads, sleeps, connects, accepts, and
  proc pause.
- Route blocking platform I/O through a bounded host service.
- Keep platform I/O waits off the scheduler thread.
- Keep completion sinks single-use after cancellation or VM death.
- Add durations/instants, random selection/shuffle, and text helpers.

### Runnable outputs

```lm
case files.with_open(path, ReadOnly()) { |file|
  file.read_text(max_bytes: 1_000_000)
}
in Ok(Ok(text))   then print(text)
in Ok(Err(error)) then print_error(display(error))
in Err(error)     then print_error(display(error))
end
```

```text
$ lm run examples/09-handles-and-supervision/cat.lm \
    --allow Fs.Open,Fs.Read,Fs.Close -- data.txt
first line
second line
```

```text
$ lm run examples/09-handles-and-supervision/word-count.lm \
    --allow Fs.Open,Fs.Read,Fs.Close -- book.txt
lines=1240 words=18302 bytes=100771
```

```text
$ lm run --show-result --allow Tcp \
    examples/12-network-effects/02-tcp-loopback.lm
Done("hello")
```

```text
$ lm run --show-result --allow Vm \
    examples/12-network-effects/03-drive-tls.lm
Done(5)
```

A proc performs long work inside `with_open`. A snapshot during that
scope returns `ResourceActive` with the lease path. A later snapshot
succeeds after the scope closes. A live raw handle reports its
machine path.

A supervisor selects between a child drive wait and its mailbox. A
mailbox command can stop or reconfigure active child supervision.

A TCP echo client/server pair runs in separate procs. A snapshot
attempt reports the active stream path. Deterministic manual clock and
random answers produce byte-for-byte repeatable output.

### Gates

- The standard file path leaks no live handle after callback return.
- Scoped designators cannot escape through source or bytecode.
- Cleanup runs on normal return and VM termination.
- Cleanup invokes no guest callback.
- No live file or socket enters snapshot bytes.
- A closed handle carries no host authority and returns a typed closed
  error.
- No wrapper hides or widens its exact underlying row.
- Platform error mapping has cross-platform golden tests.
- Async completions are single-use and safe after cancellation or VM
  death.
- Selection commits one ready arm and withdraws every losing arm.
- A losing receive leaves its mailbox message queued.
- A losing drive stops child progress at an interpreter boundary.
- Wait tokens reject reuse after completion, choice, or cancellation.
- Snapshot and restore preserve active wait descriptions.
- Snapshot blocker paths follow reachability from the root.
- Host-operation latency is excluded from interpreter dispatch
  benchmarks; completion overhead is measured separately.

---

## Week 11 — Full minimal core/standard library

### Land

- Harden the existing core method tables. This set includes collections, Text, String, Substring, Char, Bytes, builders, Option, and Result.
- Optimize measured costs and finish edge-case conformance. Add no ABI-expanding convenience method here.
- `std/set`, eager effect-polymorphic collection algorithms, sorting, text utilities, deterministic formatting, numeric/math helpers, `Range`, value utilities, and pure path operations.
- Iterative bounded `std/json` parser/stringifier over ordinary core values.
- Library docs generated from canonical signatures and executable examples.
- Conformance tests for order, mutation/freeze behavior, Unicode/UTF-8 boundaries, numeric corner cases, and collection effects.

### Runnable outputs

```text
$ lm run examples/10-std/json-format.lm -- config.json
{"name":"demo","enabled":true,"ports":[8000,8001]}
```

```lm
[1, 2, 3, 4, 5]
  .filter(do |n: Int| n % 2 == 1 end)
  .map(do |n: Int| n * n end)
  .fold(0, do |sum: Int, n: Int| sum + n end)
```

```text
$ lm run --show-result examples/10-std/list-pipeline.lm
Done(35)
```

A CSV-to-JSON command-line example combines strings, lists, maps,
scoped files, and JSON. It is the first broad allocation/GC workload.

### Gates

- Every public method has run-pass, edge, fault, freeze, and row tests where relevant.
- The library contains no convenience `Any` result where a generic type is available.
- JSON and text parsers obey depth/byte/fuel limits.
- Library algorithms are ordinary verified bytecode unless a measured intrinsic is justified.
- Collection/text benchmarks cover realistic pipelines, not only micro-operations.

---

## Week 12 — Test runner and developer tooling

### Land

- Harden the Week 6 package commands, interface-driven rebuilds, and
  content-addressed cache. Add precise cache explanations and recovery
  from corrupted entries.
- `lm check`, `build`, `run`, `test`, `inspect`, `disasm`, `snapshot`, and cache diagnostics with stable exit codes.
- Compile-pass, UI, run-pass, run-fail, verifier, corruption, conformance, and benchmark test modes in one harness; `--bless` for intentional diagnostic/IR changes.
- Child-VM test execution, per-test policy/limits, deterministic operation transcripts, parallel host scheduling with deterministic result ordering.
- Source maps, stack traces, concise artifact/row summaries, and reproducible failure bundles.
- Snapshot inspection prints machine ordinals, reachability paths,
  mailbox state, and resource blockers.
- Add focused test modes for scoped-designator diagnostics and
  snapshot blocker diagnostics.

### Runnable outputs

```text
$ lm test examples/11-package
PASS math::range_sum
PASS io::captured_print
PASS snapshot::resume
3 passed; 0 failed
```

```text
$ lm inspect build/debug/app.lma
module 91ab…
entry  (List[String]) -> Int with Io.Write, Fs.Read
imports 2
verified yes
```

A three-package application rebuild shows only the changed module and
dependent semantic units. A failing test prints the child VM fault,
bounded trace, and captured operation transcript.

### Gates

- UI snapshots are minimal, deterministic, and organized by semantic rule.
- Flaky tests are treated as failures; time/random/network tests use controlled policies.
- Cache tests simulate source edits, dependency interface changes, compiler/core ABI changes, and corrupted cache entries.
- Developer commands never grant effects merely because they appear in an artifact row.
- Snapshot tools never expose host secrets in resource paths or
  external references.
- Full smoke suite runs locally in one command and within an interactive development cycle.

---

## Week 13 — Reified compiler, verified code, syntax, and dynamic programs

### Land

- `Compiler.Compile` and `Compiler.CompileSyntax` use the bootstrap compiler and explicit `CompileEnv` values.
- `Compiler.Verify` creates opaque `VerifiedModule` values through the independent verifier.
- `Vm.install` uses `LinkEnv` and returns one `Instance`.
- Typed entry lookup returns `FunctionDef[A,R]`. Dynamic entry lookup requires a declared `DynValue` result.
- Public syntax trees preserve source text, trivia, invalid fragments, and grammar versions.
- Syntax builders create immutable nodes without exposing the private compiler AST.
- VM and compiler libraries can add name maps, revisions, rollback, and interaction policy.
- An end-to-end example compiles, verifies, installs, activates, drives, and snapshots generated code.

### Runnable outputs

```text
$ lm run examples/12-meta/compile-and-run.lm \
    --allow Compiler,Vm,Io.Write
compiled hash=7b2f… row={Io.Write}
Hello generated world!
```

A plugin host accepts source that defines a pure transformer. It verifies and installs the module.

The host requests a typed `(Json) -> Json` entry. It rejects a plugin that requests I/O.

A syntax example inspects and builds a tree without accessing the private compiler AST.

### Gates

- Runtime compilation and command-line compilation produce identical semantic artifacts for identical inputs.
- Dynamic access requires an explicit `DynValue`. Normal typed paths do not widen.
- Public syntax cannot mutate compiler state or bypass frozen boundaries.
- Compiler operation can be blocked independently and has deterministic ordinary errors.
- By the end of Week 13, every version 0.2 surface area has at least one executable example on the production path.

---

# Part IV — Close the semantics by invariant, not by adding isolated features

## Week 14 — Static-semantics closure

### Land

- Implement every remaining static rule in the specification as a table-driven conformance matrix: namespace resolution, recursion, annotations, tuple/function ambiguity, generic inference failures, variance, initialization, mutation capability, scoped-designator escape, flow refinement, exhaustiveness, override rows, first-class operations, and grant charging.
- Normalize diagnostics around stable codes/spans/notes; eliminate cascades and accidental dependence on hash-map iteration.
- Add property-generated well-typed pure programs and ill-typed near misses, including scoped returns, captures, storage, and sends.
- Compare pure HIR oracle, bytecode VM, and snapshot-resumed VM results.

### Runnable outputs

A small expression-language compiler written in the language uses enums, generics, lists/maps, closures, and exhaustive patterns:

```text
$ lm run --show-result examples/13-static/expr-compiler.lm
Done(42)
$ lm run --show-result examples/13-static/cyclic-graph.lm
Done(Digest("7b6c…"))
$ lm check --explain E2407
E2407: override widens the inherited effect row
```

The cyclic-graph program also exercises `Option`, higher-order rows, freeze, and digest.

### Gates

- Every normative static rule maps to positive and negative tests.
- No source checker panic/ICE for arbitrary parser output.
- Generated well-typed programs verify and agree with the oracle.
- Compile-time distributions are tracked for large modules, deep generics, and large enum matches.

---

## Week 15 — VM/state-machine closure

### Land

- Model every VM and machine-world transition explicitly, including waiting completion races, request token reuse, terminal idempotence, pause, parent death, cancellation, resource reservation, nested tables, snapshot barriers, mailbox cuts, and barrier abort.
- Add a small executable state-machine model in tests and compare random command sequences against the production VM and scheduler.
- Complete fuel/heap/frame/operand/boundary/mailbox/mock/snapshot limit interactions and fail-atomic updates.
- Model barrier closure over handles found late in the walk, and the serialization of overlapping barriers.
- Audit all host callbacks and completion sinks for single use and dead-VM behavior.

### Runnable outputs

```text
$ lm run --show-result examples/14-vm/tower.lm --allow Vm
Done(42)
$ lm run --show-result examples/14-vm/all-transitions.lm --allow Vm,Proc
Done(TransitionSummary(legal: 31, rejected: 24))
$ lm inspect build/all-transitions-paused.lms
state=asked owner=holder ordinal=8 procs=3 mailboxes=2 fuel=9912
```

The tower constructs five nested machines and grants only `Vm`; the transition supervisor drives, snapshots, restores, rejects, dispatches, pauses, and revokes a child through every legal state.

### Gates

- Random model traces cover all transition edges and reproduce failures as compact scripts.
- No illegal caller action mutates the controlled VM or machine world.
- Nested depth does not change Rust stack usage.
- Limit exhaustion leaves heaps, frames, pending requests, tables, and mailboxes internally valid.

---

## Week 16 — Artifact, snapshot, boundary, and security closure

### Land

- Full adversarial validation pass over artifact/snapshot decoders, verifier, graph codec, type/class layouts, code availability, machine ordinals, handle relocation, machine references, resource classifications, and size arithmetic.
- Streaming/preflight checks that reject impossible sizes before large allocations.
- Fuzz dictionaries/corpora derived from real artifacts and snapshots; structure-aware mutators; coverage reporting.
- Verified-code and trusted-snapshot cache poisoning tests.
- Threat model for untrusted source, bytecode, snapshots, host implementations, and proc messages.

### Runnable outputs

```text
$ lm snapshot verify fuzz-regressions/bad-frame-pc.lms
error[S3017]: frame 2 PC 91 is not an instruction boundary
```

```text
$ lm inspect fuzz-regressions/row-forgery.lma
error[A2041]: PERFORM Net.Connect not contained in claimed row
```

A sandbox example consumes intentionally hostile source/artifact/snapshot inputs under strict limits and returns ordinary diagnostics without process failure.

### Gates

- Continuous fuzz jobs for scanner, parser, artifact, verifier, snapshot, graph, and VM transitions.
- No unchecked `offset + count * width` arithmetic.
- Verification caches bind exact bytes/hash/ABI/verifier version.
- External snapshots are verified once; trusted resume paths contain no hidden reparse.
- Fuzzing covers machine counts, mailbox counts, relocation type
  mismatches, and malformed machine references.
- Unsafe modules have written invariants, Miri tests, and independent review checklists.

---

## Week 17 — Library/API closure and example-driven redesign

### Land

- Run the complete example corpus as an API design review. Remove awkward names, duplicate mechanisms, accidental `Any`, and wrappers that hide rows before freezing version 0.2 interfaces.
- Fill only gaps demonstrated by real programs: list/map/string convenience, path edge cases, scoped file loops, explicit raw resource ownership, typed VM helpers, proc supervision, JSON diagnostics, and test ergonomics.
- Review every external resource through its scoped and raw APIs. Keep raw handles only where long ownership is required.
- Add reference documentation with complexity, mutation/freeze behavior, effects, ordinary errors, faults, and snapshot classification for every public API.
- Establish compatibility policy for core hashes, operation revisions, standard modules, and diagnostic stability.

### Runnable outputs

At least twelve substantial programs run without private runtime hooks:

1. calculator/parser;
2. grep-like text filter;
3. word count;
4. CSV-to-JSON;
5. JSON configuration transformer;
6. snapshot checkpoint/resume;
7. deterministic simulator with clock/random mocks;
8. manually driven output capture;
9. nested sandbox runner;
10. proc worker pool;
11. TCP echo client/server;
12. runtime-compiled pure plugin host.

```text
$ cargo xtask examples --release
12 substantial examples passed; 0 private/test-only APIs used
```

### Gates

- Every example is a black-box release test and documentation source.
- No example reaches into bootstrap-only or test-only APIs.
- File examples use scoped access unless long ownership is essential.
- Public API inventory matches the specification exactly.
- A clean checkout can build and run the corpus with one command.

---

# Part V — Self-host the compiler in four weekly cuts

## Week 18 — Self-hosted source, parser, and resolver

### Land

- Port scanner, parser, source map, diagnostics skeleton, module predeclaration, and lexical/name resolution to the language.
- Port brace closures and trailing closure arguments first. Compare both closure spellings against the Rust frontend.
- Keep the Rust compiler as stage 0 and compare canonical token/AST/resolution dumps on the full corpus.
- Use ordinary `List`, `Map`, `Result`, builders, and explicit diagnostics; no privileged parser runtime.
- Compile the self-hosted frontend with stage 0 and run it in a VM with only file-read/write operations required by the tool.

### Runnable outputs

```text
$ lm-stage0 run compiler/frontend.lma -- examples/01-basics/factorial.lm
AST hash: 34df…
```

The self-hosted parser successfully parses core, std, compiler sources, and every UI test. A parser error file produces the same stable code/span as the Rust frontend.

### Gates

- Canonical token/AST/resolution output matches stage 0 across the corpus.
- Equivalent brace and `do` closures produce identical typed HIR.
- The self-hosted frontend can be manually driven with file operations supplied by a holder.
- Parser fuzz inputs are run against both implementations; crashes and divergent successful parses are regressions.

---

## Week 19 — Self-hosted types, initialization, and effects

### Land

- Port interned type forms, subtype checks, bidirectional checking, generic unification, flow environments, enum exhaustiveness, definite initialization, effect-row calculation, and scoped-designator escape checks.
- Emit the same typed-HIR and row summaries as the Rust compiler.
- Keep verifier/runtime in Rust; self-hosting changes no execution trust boundary.

### Runnable outputs

```text
$ lm-stage0 run compiler/checker.lma -- core std examples
checked 186 modules; 0 mismatches against stage0
```

A deliberate row-understatement, invalid constructor, and ambiguous generic call produce matching diagnostic codes and primary spans in both compilers.

### Gates

- Typed-HIR semantic hashes match stage 0 for all accepted sources.
- Negative UI tests match stable codes/spans; wording may differ only where explicitly blessed.
- Compile-time and heap use remain bounded on pathological type inputs.

---

## Week 20 — Self-hosted lowering, bytecode, artifacts, and interfaces

### Land

- Port closure conversion, typed CFG, stack planning, bytecode emission, canonical artifact/interface writer, definition/SCC hashing, and build-key computation.
- Every self-hosted output still passes the independent Rust verifier.
- Add stage comparison tools that explain the first differing semantic section/instruction rather than byte-dumping whole files.

### Runnable outputs

```text
$ lm-stage0 run compiler/lmc.lma -- build examples/10-std/json-format
$ lm run build/selfhost/json-format.lma -- config.json
...
```

The self-hosted compiler builds core, std, the example corpus, and its own source into verified artifacts.

### Gates

- Stage-0 and self-hosted semantic artifact hashes match on the corpus.
- Any differing debug/source-map section is explained and either normalized or deliberately excluded from semantic identity.
- The Rust verifier remains the admission authority for self-hosted output.

---

## Week 21 — Bootstrap closure and reproducible stages

### Land

- Stage orchestration:
  - stage 0: Rust compiler builds core/std/compiler stage 1;
  - stage 1: language compiler rebuilds core/std/compiler stage 2;
  - stage 2: compiler rebuilds stage 3 for same-result comparison.
- Reproducible bootstrap manifests, compiler semantic hashes, checked generated ABI tables, and release bootstrap archives.
- Developer fast path uses stage 1; release path performs full stage comparison.

### Runnable outputs

```text
$ lm bootstrap --compare
stage1 compiler  8a41…
stage2 compiler  8a41…
stage3 compiler  8a41…
semantic match
```

The stage-2 compiler builds and runs all examples/tests without invoking the Rust frontend.

### Gates

- Stage 2 and stage 3 semantic outputs are identical.
- A clean bootstrap works offline from pinned Rust/Cargo dependencies and checked core/ABI inputs.
- Bootstrap failure reports the first differing definition/section.
- The runtime, verifier, host, and scheduler remain Rust; only the compiler and ordinary libraries self-host.

---

# Part VI — Production engineering driven by measurement and adversarial testing

## Week 22 — Profile and tighten the interpreter/compiler hot paths

### Land

- Run production profiles on the full corpus and benchmark suite before changing representation.
- Tighten decoded instruction layout, dispatch loop, frame/operand indexing, virtual calls, intrinsic calls, request creation, policy lookup, and verified-code loading where profiles justify it.
- Add safe superinstructions or inline fast paths only for measured common sequences; retain canonical bytecode and verifier semantics.
- Add compiler self-profiling by immutable phase/query-like unit without committing to a heavyweight general query engine.

### Runnable outputs

```text
$ lm bench compare base current
dispatch       +11.8%  [10.9%, 12.6%]
virtual-call    +7.1%  [ 6.4%,  7.8%]
$ lm profile run examples/16-worker-pool
interpreter=41% allocation=17% gc=6% graph=8% policy=3% host_wait=25%
```

All prior examples remain byte-for-byte or semantically identical as appropriate.

### Gates

- No optimization lands without a benchmark showing benefit and conformance/differential tests showing equivalence.
- `run` still allocates no per-instruction event/request.
- Optimized and baseline/debug interpreter modes are different configurations of the same engine, not separate semantic implementations.
- Performance changes include regression thresholds in CI.

---

## Week 23 — Heap, GC, collection, and graph performance

### Land

- Profile allocation lifetime and object-size distributions from real programs.
- Tune page sizes, free lists, mark worklists, object-slot locality, string/bytes sharing, list growth, map index/load factors, and digest caching.
- Introduce a young generation only if measured survival/allocation data justifies it; `ObjRef` permits this without changing guest references. Otherwise retain the simpler collector.
- Optimize graph modes independently while keeping one shape/reachability definition.

### Runnable outputs

```text
$ lm bench heap examples/22-memory/million-collections.lm
list_push=1,000,000 map_put=1,000,000 peak_heap=188MiB collections=14
$ lm bench graph examples/22-memory/cyclic-snapshot.lm
digest=84ms snapshot=117ms load_verify=92ms restore=3ms
```

Large JSON, CSV, proc-message, and deep/cyclic graph workloads run under the same memory dashboard and continue to satisfy boundary/snapshot invariants.

### Gates

- Collection pause, allocation throughput, peak memory, and graph throughput improve or complexity is rejected.
- No optimizer changes insertion order, equality, digest, snapshot bytes, or frozen semantics.
- Miri/fuzz/debug generation checks remain available even if release builds elide some checks.

---

## Week 24 — Incremental compilation and build-cache precision

### Land

- Use immutable phase outputs and explicit dependency fingerprints to cache parsing, resolution, typed HIR, codegen, interfaces, and verified artifacts at module/definition granularity where profitable.
- Track actual imported interface/signature/hash dependencies rather than timestamps or broad directory invalidation.
- Add revision tests that change comments, private bodies, public signatures, rows, core ABI, and compiler versions and assert exactly which units rebuild.
- Persist caches with corruption/hash checks; keep a no-cache mode as the oracle.

### Runnable outputs

```text
$ lm build examples/large-app --explain-cache
parse util            hit
check util             hit
emit util              hit
check app              miss: imported row changed
```

A scripted edit sequence demonstrates no-op rebuild, private implementation rebuild, and downstream interface invalidation.

### Gates

- Cached and clean builds produce identical diagnostics and artifacts.
- Dependency graph tests assert both required invalidation and forbidden over-invalidation.
- Cache corruption degrades to a rebuild, never trusted output.
- Incremental machinery does not enter the runtime/kernel dependency graph.

---

## Week 25 — Continuous fuzzing and differential execution

### Land

- Structure-aware generators for valid/invalid source, typed HIR, bytecode, artifacts, snapshots, graphs, and VM command traces.
- Differential oracles:
  - pure HIR evaluator versus verified VM;
  - normal run versus step-to-terminal;
  - normal run versus snapshot/restore at random boundaries;
  - automatic mock policy versus equivalent typed manual drive;
  - stage-0 versus self-hosted compiler semantic outputs.
- Continuous fuzz jobs with minimized regression corpus checked into `tests/fuzz-regressions`.

### Runnable outputs

```text
$ lm fuzz replay tests/fuzz-regressions/request-token-017.case
reproduced and passed: stale token rejected without VM mutation
```

Generated pure programs run through all execution oracles and print a compact seed on divergence.

### Gates

- Coverage includes every decoder/verifier fault family and VM transition.
- Every discovered bug becomes a minimized permanent regression test.
- Fuzz harnesses apply strict resource limits and cannot hang CI.
- Determinism fuzzing recompiles identical inputs and compares semantic bytes/hashes.

---

## Week 26 — Cross-platform runtime and embedding

### Land

- Tier-1 Linux x86-64, macOS arm64/x86-64 as available, and Windows x86-64 builds with identical semantic/ABI vectors.
- Stable Rust embedding API for ABI initialization, operation registration, artifact/snapshot load, VM creation, policy configuration, drive/run, and code-hash resolution.
- Generated optional C ABI shim with opaque handles, explicit ownership, no direct guest pointers, and example embedder.
- Host-adapter conformance kit for third-party operation implementations.

### Runnable outputs

```text
$ cargo xtask cross-platform-vectors
linux-x86_64  ok  artifact=4fe2… snapshot=71a0… result=42
macos-aarch64 ok  artifact=4fe2… snapshot=71a0… result=42
windows-x86_64 ok artifact=4fe2… snapshot=71a0… result=42
$ cargo run -p embed-telemetry-example
Telemetry.Emit({"request": 7, "status": "ok"})
Done(())
```

The Rust embedder registers `Telemetry.Emit`, manually drives a guest, and records typed requests. The optional C-compatible shim has a separate opaque-handle smoke test; it is not an alternate implementation.

### Gates

- Cross-platform canonical vectors for UTF-8, integers, floats, hashes, artifacts, snapshots, and digests.
- Foreign-function misuse returns errors or closes handles; it cannot install unchecked code/state.
- Host conformance tests check reply types, completion single use, cancellation, and boundary modes.

---

## Week 27 — Security and reliability release candidate

### Land

- Independent audit of unsafe modules, verifier assumptions, snapshot loader, graph canonicalization, policy/pass chains, host completion lifetimes, scheduler ownership transfer, and resource limits.
- Panic/abort policy: untrusted input yields diagnostics/faults; internal invariant failures are distinguished and produce reproducible crash bundles in debug tools.
- Dependency/license/SBOM generation, reproducible release builds, signed ABI/core/compiler manifests, and security-response process.
- Long-running stress: compiler daemon-like workloads, millions of VM creates/runs, repeated snapshot branches, proc churn, and host cancellation.

### Runnable outputs

```text
$ lm doctor --release-candidate
ABI/core manifests verified
unsafe audit checks passed
fuzz corpus passed
cross-platform vectors passed
benchmark regressions none
```

The full sandbox-service example runs for an extended soak with bounded memory and deterministic request logs under mocked nondeterminism.

### Gates

- No known verifier/snapshot/boundary escape.
- All release binaries/artifacts are reproducible from the pinned bootstrap set.
- Stress runs show stable memory and handle counts.
- Release candidate freezes core/operation identities except for correctness fixes.

---

## Week 28 — Documentation, examples, and migration-quality diagnostics

### Land

- Complete language reference cross-linked to conformance tests, core/std API docs, embedding guide, host-operation authoring guide, and bootstrap/runtime architecture notes.
- Curated examples from basics through nested sandboxes/procs/snapshots/dynamic compilation, each runnable in CI.
- Error explanations and fix suggestions for common type/effect/boundary/policy mistakes.
- `lm fmt` only if a stable formatter already follows naturally from the parser; otherwise ship a canonical pretty-printer for generated/debug output and defer source formatting rather than rushing it.

### Runnable outputs

```text
$ lm examples run-all
20 passed
```

A new user can clone, bootstrap, build a multi-module program, add a test, inspect its row, run under an explicit policy, snapshot it, and embed it by following only shipped docs.

### Gates

- Every documentation snippet is compiled or run in CI.
- Public names/signatures in docs are generated from the pinned core/std interfaces.
- No example requires unstated ambient grants.
- Diagnostics use stable codes and point to the smallest actionable source span.

---

## Week 29 — Version 0.2 release

### Land

- Final ABI/core/compiler hashes, release artifacts, source archive, bootstrap archive, platform packages, conformance kit, benchmark report, fuzzing status, and known-limitations document.
- Version-policy checks for artifact/snapshot/core/operation compatibility.
- Post-release branch and patch process for verifier/runtime/security fixes without silent ABI mutation.

### Runnable outputs

The release demonstration builds and runs a program that:

1. parses JSON configuration;
2. compiles a typed plugin at runtime;
3. verifies its empty or declared row;
4. links it through typed environments;
5. launches it in a nested VM with worker procs and finite limits;
6. manually intercepts one typed operation and automatically dispatches another;
7. snapshots the suspended machine world;
8. restores two complete independent worlds;
9. collects typed results and writes output through scoped filesystem operations.

```text
$ lm run examples/28-release/sandbox-service.lm \
    --profile examples/28-release/policy.toml -- config.json
compiled_plugin=9c21… intercepted=Io.Write snapshots=2 results=[41,42]
```

### Gates

- Clean bootstrap stage comparison succeeds.
- Full UI/run/corruption/conformance/fuzz-regression/cross-platform suites pass.
- No benchmark exceeds the agreed release regression budget.
- Release artifacts and core/compiler semantic hashes reproduce independently.
- The system remains one Rust VM, one bytecode verifier, one operation boundary, one graph-reachability definition, and one explicit authority model.

---

## 3. Resulting compiler/runtime pipeline

```text
UTF-8 source
  -> tokens + spans
  -> AST
  -> predeclared names / resolved IDs
  -> typed HIR + proven effect rows
  -> CFG + initialization/return facts
  -> closure conversion + stack plan
  -> canonical bytecode/artifact/interface
  -> independent verifier
  -> decoded numeric code/class/type/operation slots
  -> one Rust interpreter loop
  -> explicit perform boundary
  -> policy table or typed manual holder decision
```

Machine state is always explicit data. Guest calls always push VM frames. External artifacts and snapshots are verified at admission. `run`, `step`, and `drive` select stop behavior on the same engine. Core nominal types are source-defined and hash-pinned. The standard library is ordinary code. Self-hosting replaces the bootstrap frontend without changing the runtime trust boundary.

---

## 4. Practice basis

The sequence adapts concrete production practices rather than copying any one project's architecture:

- [rustc compiletest](https://rustc-dev-guide.rust-lang.org/tests/compiletest.html): source-level UI, compile-pass, run-pass, run-fail, incremental, and tool-oriented suites as durable compiler interfaces;
- [rustc staged bootstrapping](https://rustc-dev-guide.rust-lang.org/building/bootstrapping/what-bootstrapping-does.html): explicit stage-0/stage-1/stage-2 construction followed by a same-result stage;
- [Cranelift verification](https://docs.rs/cranelift-codegen/latest/cranelift_codegen/struct.Context.html#method.verify): independently validate IR and its control-flow facts instead of assuming producer correctness;
- [Wasmtime fuzzing](https://docs.wasmtime.dev/contributing-fuzzing.html): continuous structured fuzzing with generators and explicit oracles;
- [Wasmtime support tiers](https://docs.wasmtime.dev/stability-tiers.html): require continuous fuzzing, and differential fuzzing where possible, before treating a compiler/runtime surface as production-ready.

The project-specific conclusions are the vertical first month, one production execution path, admission-time verification caches, explicit bootstrap identity checks, and benchmark distributions for representation decisions.
