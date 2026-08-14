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
  lm-embed/        stable Rust embedding API and optional C shim
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
- Dense exact/group policy arrays with default block, transitive `pass`, pure frozen `mock`, and live table editing.
- Public native `EmptyVm`/`Vm[T]`, typed load/restore transitions, `step`, terminal `run`, `drive`, states, wait completions, stack views, fuel/limits, reentrancy checks, and one internal stop-mode interpreter loop.
- Typed `Request.as_call(op) -> PendingCall[Args,Reply]`; typed `answer`; token-checked `reject`/`dispatch`; no `Answer(Any)` path.
- Initial host operations: `Io.Print`, `Io.Error`, `Io.ReadLine`, `Clock.Now`, `Clock.Monotonic`, `Clock.Sleep`, and deterministic `Rand.Bytes`/`Rand.Int` adapters.
- Async completion channel with no Rust reference into guest memory.

### Runnable outputs

```lm
def greet(name: String) with Io.Print
  sys.io.Print("Hello {name}!\n")
end

greet("Ada")
```

```text
$ lm run examples/04-effects/hello.lm --allow Io.Print
Hello Ada!
```

```lm
vm = sys.vm.Vm().from_object(do || with Io.Print, Clock.Now
  sys.io.Print("tick\n")
  sys.clock.Now()
end, args: ())

captured: [String] = []
loop do
  case vm.drive()
  in Asked(q)
    case q.as_call(sys.io.Print)
    in Some(call)
      (text,) = call.args()
      captured.push(text)
      vm.answer(call, ())
    in None
      case q.as_call(sys.clock.Now)
      in Some(call) then vm.answer(call, 123)
      in None       then vm.dispatch(q)
      end
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

## Week 5 — Deterministic artifacts, interfaces, linking, and packages

### Land

- Canonical artifact/interface containers, definition/module/container hashes, SCC hashing for mutually recursive definitions, debug sections, and atomic writes.
- Explicit `CompileEnv` and `LinkEnv` typed builders; import slots with signatures/rows/pinned hashes; typed `LinkedEntry[A,R]`; dynamic access only through `DynValue`.
- Pure linker, code/class load tables, verified-code cache keyed by semantic hash plus ABI/verifier version.
- `lm build`, artifact execution, interface emission, package manifest reader, dependency DAG, and content-addressed build directory.
- Corruption-focused byte readers shared by artifact/snapshot work.

### Runnable outputs

```text
examples/05-modules/
  lm.package
  src/main.lm
  src/greeter.lm
```

`greeter.lm` exports `Greeter`; `main.lm` receives its pinned interface through the build graph and returns a closure.

```text
$ lm build examples/05-modules
built Greeter  2cf4…
built app      91ab…
$ lm run build/debug/app.lma --allow Io.Print
Hello Ada!
```

A second build with unchanged inputs reports cache hits. Editing only a comment leaves semantic definition/module hashes unchanged while the exact source/input cache key changes appropriately.

A runtime-compilation example binds a frozen `Config` with `CompileEnv.bind`, compiles a module, links it with `LinkEnv`, requests a typed entry, and runs it.

### Gates

- Truncated, overlong, duplicate, reordered, hash-mismatched, and type-incompatible artifacts reject before allocation-heavy work.
- Reproducible artifact bytes across CI hosts.
- Linking installs no global names and performs no host operation.
- Verified code is never re-verified under the same hash/ABI cache key.
- Build-cache and verified-code-cache responsibilities remain separate.

---

## Week 6 — One graph engine, boundaries, freezing, and nested sandboxes

### Land

- Iterative graph engine and native shape table for mark, freeze, frozen verification, transfer/copy, canonical digest, detached inspection, and later snapshot encoding.
- Cycle/sharing preservation, canonical traversal ordinals, bounded work tables, digest cache on frozen objects, and stable map semantics.
- Transfer versus control-envelope modes; sendability checks; code/classes by hash; holder-local VM/table handles; typed proc handles reserved for Week 8; inert resource descriptors.
- `Vm.from_object` and terminal publication fully routed through the codec.
- Nested VM authority chains and resource reservation from parent budgets.

### Runnable outputs

```lm
class Node
  value: Int
  next: Option[Node] = None
end

# test helper creates a cycle, freezes it, and computes a stable digest
```

```text
$ lm run --show-result examples/06-graphs/cycle-digest.lm
Done(6f58…)
```

```lm
def sandbox(program: () -> Int with Clock.Now): RunResult[Int] with Vm
  vm = sys.vm.Vm().from_object(program, args: ())
  vm.table().mock(Clock.Now, do || 1000 end)
  vm.run()
end

sandbox(do || with Clock.Now sys.clock.Now() + 1 end)
```

```text
$ lm run --show-result examples/06-graphs/sandbox.lm --allow Vm
Done(Done(1001))
```

A boundary-negative example attempts to return a mutable list from a child VM and receives `Fault(UnsendableValue)`.

### Gates

- One graph-shape definition controls every mode; no native class has a one-off serializer or freezer.
- Deep graphs never recurse on the Rust stack.
- Differential property tests compare freeze/copy/digest reachability and cycle preservation.
- Nested VM depth does not multiply interpreter loops or Rust stack depth.
- Graph benchmarks cover flat data, deep chains, cycles, shared subgraphs, large bytes, and maps.

---

## Week 7 — Snapshots, restore, inspection, and branching execution

### Land

- Canonical snapshot writer over trusted state; external snapshot loader/verifier; one-time conversion from bytes to trusted `SnapshotImage`; typed `Snapshot[T]` result casting.
- Serialization of heap, code/class/type manifests, frames, locals/operands, limits/fuel, pending request, and inert descriptors; exclusion of tables/grants/live callbacks/scheduler ownership.
- Restore of between-instruction, `asked`, and supported inert-wait states; multi-shot restore; receiverless self-snapshot.
- `lm snapshot verify/run`, `lm inspect` for artifacts/snapshots, source-mapped `stack()`, and deterministic snapshot diffs for tests.

### Runnable outputs

```lm
vm = sys.vm.Vm().from_object(do ||
  x = 20
  x + 22
end, args: ())

vm.step()
snap = vm.snapshot()
left = sys.vm.Vm().restore(snap).run()
right = sys.vm.Vm().restore(snap).run()
(left, right)
```

```text
$ lm run --show-result examples/07-snapshots/branch.lm --allow Vm
Done((Done(42), Done(42)))
```

A self-checkpoint example requests `sys.vm.Snapshot()`, receives a `SnapshotImage`, writes its bytes through an explicitly granted file wrapper, and resumes. A manual-drive snapshot captured in `asked` restores to the same operation and typed reply expectation; its holder calls `drive()` to mint a fresh request token before answering.

```text
$ lm snapshot verify checkpoints/asked.lms
valid: state=asked op=Clock.Now frames=2 objects=37
```

### Gates

- Snapshot round-trip tests at every bytecode boundary in the example corpus.
- Loader rejects malformed object references, frame PCs, stack shapes, pending replies, descriptors, and limit overflows.
- Instrumentation proves whole-image structural verification occurs once on external load, not on every resume/step.
- In-process trusted snapshot restore and external byte load remain separate APIs.
- Snapshot size/load/write benchmarks are tracked by workload shape.

---

## Week 8 — Typed procs, mailboxes, supervision, and live revocation

### Land

- Proc scheduler, scheduler/holder ownership transfer, `Handle[M,R]`, `Proc.Run`, compiler `spawn` sugar, bounded FIFO mailboxes, `send`, `receive`, `close`, `done`, pause/resume, and dead-peer results.
- One VM per proc, one logical guest thread, no shared mutable guest memory, transfer-checked messages/results, and live parent table chains.
- Scheduler completion/pause channels using VM IDs and ordinals rather than guest references.
- Proc tracing, mailbox metrics, and deterministic test scheduler mode.

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
h.close()
h.done()
```

```text
$ lm run --show-result examples/08-procs/worker.lm --allow Proc
Done(Done(42))
```

A logger example drains messages after close. A sandbox-service example launches a configured VM on a proc thread, revokes `Io.Print` while it runs, pauses it, inspects frames, snapshots it, and resumes.

### Gates

- Message and result types never erase to `Any`.
- FIFO acceptance, close/drain, pause/resume, parent death, revocation, and dead-peer behavior have deterministic model tests.
- Mailbox limits are checked before copying/acceptance.
- Thread sanitizer equivalents are unnecessary for guest memory because no guest heap is shared; Rust concurrency tests focus on scheduler/table/handle state.
- Proc send/receive, spawn, pause, and terminal publication benchmarks are committed.

---

# Part III — A practical distribution by Week 12

## Week 9 — Files, paths, console, time, random, and TCP

### Land

- Full initial operation manifest for I/O, filesystem, clock, random, TCP, and optional process environment/current-directory operations.
- Platform adapters for Unix and Windows with ordinary portable error enums and inert snapshot descriptors.
- Pure `std/path`; explicit `File`/TCP wrappers; `read_exact`, `read_all`, `write_all`, text helpers; durations/instants; random selection/shuffle.
- Cancellation and async completion behavior for blocking reads, sleeps, connects, accepts, and proc pause.
- Root CLI policy profiles with explicit grants and finite limits.

### Runnable outputs

```text
$ lm run examples/09-host/cat.lm --allow Fs.Open,Fs.Read,Fs.Close -- data.txt
first line
second line
```

```text
$ lm run examples/09-host/word-count.lm \
    --allow Fs.Open,Fs.Read,Fs.Close -- book.txt
lines=1240 words=18302 bytes=100771
```

A TCP echo client/server pair runs in separate procs with explicit `Net.*` and `Proc` grants. A deterministic mode manually answers clock/random operations and produces byte-for-byte repeatable output.

### Gates

- No wrapper hides or widens the exact underlying row.
- OS handles cannot cross a snapshot as live resources.
- Platform error mapping has cross-platform golden tests.
- Async completions are single-use and safe after cancellation/VM death.
- Host-operation latency is excluded from interpreter dispatch benchmarks but completion overhead is measured separately.

---

## Week 10 — Full minimal core/standard library

### Land

- Harden, optimize where measured, and finish edge-case conformance for the already sealed `List`, `Map`, `String`, `Bytes`, builder, `Option`, and `Result` method tables from Week 3; no ABI-expanding convenience method is added here.
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

A CSV-to-JSON command-line example combines strings, lists, maps, files, and JSON and is used as the first broad allocation/GC workload.

### Gates

- Every public method has run-pass, edge, fault, freeze, and row tests where relevant.
- The library contains no convenience `Any` result where a generic type is available.
- JSON and text parsers obey depth/byte/fuel limits.
- Library algorithms are ordinary verified bytecode unless a measured intrinsic is justified.
- Collection/text benchmarks cover realistic pipelines, not only micro-operations.

---

## Week 11 — Package/build loop, test runner, and developer tooling

### Land

- Complete package manifest semantics, deterministic dependency resolution for path/pinned artifact dependencies, interface-driven rebuilds, and content-addressed cache.
- `lm check`, `build`, `run`, `test`, `inspect`, `disasm`, `snapshot`, and cache diagnostics with stable exit codes.
- Compile-pass, UI, run-pass, run-fail, verifier, corruption, conformance, and benchmark test modes in one harness; `--bless` for intentional diagnostic/IR changes.
- Child-VM test execution, per-test policy/limits, deterministic operation transcripts, parallel host scheduling with deterministic result ordering.
- Source maps, stack traces, concise artifact/row summaries, and reproducible failure bundles.

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
entry  (List[String]) -> Int with Io.Print, Fs.Read
imports 2
verified yes
```

A three-package application rebuild shows only the changed package and dependent semantic units. A failing test prints the child VM fault, bounded trace, and captured operation transcript.

### Gates

- UI snapshots are minimal, deterministic, and organized by semantic rule.
- Flaky tests are treated as failures; time/random/network tests use controlled policies.
- Cache tests simulate source edits, dependency interface changes, compiler/core ABI changes, and corrupted cache entries.
- Developer commands never grant effects merely because they appear in an artifact row.
- Full smoke suite runs locally in one command and within an interactive development cycle.

---

## Week 12 — Reified compiler, typed linker, reflection, and dynamic programs

### Land

- `Compiler.Compile` host operation backed by the same bootstrap compiler pipeline and explicit `CompileEnv`.
- Typed `LinkEnv`, `Type[T]` witnesses, `DynValue` pack/unpack for intentionally dynamic tooling, and typed `LinkedEntry[A,R]`.
- Read-only `Reflect.Mirror`, frame/code/class metadata, no dynamic selector invocation, and detached `ValueView` trees.
- Standard VM/compiler/reflection/test helpers built on these primitives.
- End-to-end admission example: compile source, inspect row/hash, choose policy, link, run in a child VM, capture requests, and snapshot.

### Runnable outputs

```text
$ lm run examples/12-meta/compile-and-run.lm \
    --allow Compiler.Compile,Vm,Io.Print
compiled hash=7b2f… row={Io.Print}
Hello generated world!
```

A plugin-host example accepts source defining a pure transformer, verifies its empty row, links a typed `(Json) -> Json` entry, runs it under a default-deny child VM, and rejects a plugin that requests I/O.

A reflection example prints class/field/code metadata without acquiring a callable dynamic method or live guest reference.

### Gates

- Runtime compilation and command-line compilation produce identical semantic artifacts for identical inputs.
- Dynamic access requires an explicit `DynValue`; normal typed paths do not widen.
- Reflection cannot invoke a selector, mutate code, or bypass frozen/boundary rules.
- Compiler operation can be blocked independently and has deterministic ordinary errors.
- By the end of Week 12, every version 0.2 surface area has at least one executable example on the production path.

---

# Part IV — Close the semantics by invariant, not by adding isolated features

## Week 13 — Static-semantics closure

### Land

- Implement every remaining static rule in the specification as a table-driven conformance matrix: namespace resolution, recursion, annotations, tuple/function ambiguity, generic inference failures, variance, initialization, mutation capability, flow refinement, exhaustiveness, override rows, first-class operations, and grant charging.
- Normalize diagnostics around stable codes/spans/notes; eliminate cascades and accidental dependence on hash-map iteration.
- Add property-generated well-typed pure programs and ill-typed near misses.
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

## Week 14 — VM/state-machine closure

### Land

- Model every VM state and control transition explicitly, including waiting completion races, request token reuse, terminal idempotence, proc ownership, pause, parent death, cancellation, resource reservation, and nested tables.
- Add a small executable state-machine model in tests and compare random command sequences against the production VM.
- Complete fuel/heap/frame/operand/boundary/mailbox/mock/snapshot limit interactions and fail-atomic updates.
- Audit all host callbacks and completion sinks for single use and dead-VM behavior.

### Runnable outputs

```text
$ lm run --show-result examples/14-vm/tower.lm --allow Vm
Done(42)
$ lm run --show-result examples/14-vm/all-transitions.lm --allow Vm,Proc
Done(TransitionSummary(legal: 23, rejected: 19))
$ lm inspect build/all-transitions-paused.lms
state=asked owner=holder ordinal=8 frames=3 fuel=9912
```

The tower constructs five nested machines and grants only `Vm`; the transition supervisor drives, snapshots, restores, rejects, dispatches, pauses, and revokes a child through every legal state.

### Gates

- Random model traces cover all transition edges and reproduce failures as compact scripts.
- No illegal caller action mutates the controlled VM.
- Nested depth does not change Rust stack usage.
- Limit exhaustion leaves heaps, frames, pending requests, and tables internally valid.

---

## Week 15 — Artifact, snapshot, boundary, and security closure

### Land

- Full adversarial validation pass over artifact/snapshot decoders, verifier, graph codec, type/class layouts, code availability, descriptor inertness, and size arithmetic.
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
- Unsafe modules have written invariants, Miri tests, and independent review checklists.

---

## Week 16 — Library/API closure and example-driven redesign

### Land

- Run the complete example corpus as an API design review. Remove awkward names, duplicate mechanisms, accidental `Any`, and wrappers that hide rows before freezing version 0.2 interfaces.
- Fill only gaps demonstrated by real programs: list/map/string convenience, path edge cases, explicit file loops, typed VM helpers, proc supervision, JSON diagnostics, and test ergonomics.
- Add reference documentation with complexity, mutation/freeze behavior, effects, ordinary errors, and faults for every public API.
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
- Public API inventory matches the specification exactly.
- A clean checkout can build and run the corpus with one command.

---

# Part V — Self-host the compiler in four weekly cuts

## Week 17 — Self-hosted source, parser, and resolver

### Land

- Port scanner, parser, source map, diagnostics skeleton, module predeclaration, and lexical/name resolution to the language.
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
- The self-hosted frontend can be manually driven with file operations supplied by a holder.
- Parser fuzz inputs are run against both implementations; crashes and divergent successful parses are regressions.

---

## Week 18 — Self-hosted types, initialization, and effects

### Land

- Port interned type forms, subtype checks, bidirectional checking, generic unification, flow environments, enum exhaustiveness, definite initialization, and effect-row calculation.
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

## Week 19 — Self-hosted lowering, bytecode, artifacts, and interfaces

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

## Week 20 — Bootstrap closure and reproducible stages

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

## Week 21 — Profile and tighten the interpreter/compiler hot paths

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

## Week 22 — Heap, GC, collection, and graph performance

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

## Week 23 — Incremental compilation and build-cache precision

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

## Week 24 — Continuous fuzzing and differential execution

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

## Week 25 — Cross-platform runtime and embedding

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

## Week 26 — Security and reliability release candidate

### Land

- Independent audit of unsafe modules, verifier assumptions, snapshot loader, graph canonicalization, policy/pass chains, host completion lifetimes, proc ownership, and resource limits.
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

## Week 27 — Documentation, examples, and migration-quality diagnostics

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

## Week 28 — Version 0.2 release

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
5. launches it in a nested VM/proc with finite limits;
6. manually intercepts one typed operation and automatically dispatches another;
7. snapshots the suspended machine;
8. restores two diverging copies under fresh policies;
9. collects typed results and writes output through explicit filesystem operations.

```text
$ lm run examples/28-release/sandbox-service.lm \
    --profile examples/28-release/policy.toml -- config.json
compiled_plugin=9c21… intercepted=Io.Print snapshots=2 results=[41,42]
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
