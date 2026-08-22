# Week 2 Status

This note records what landed in week 2, the known simplifications,
and the deferred work.

## Landed

- A real heap in `lm-vm`: a per-VM object table with
  generation-checked references, slot pages of fixed size, and a
  stop-the-VM mark/sweep collector. The tracer uses an iterative
  worklist. One `trace_children` walker defines reachability for the
  mark phase and for `freeze`. Native shape descriptors give the
  tracer and the printers the layout of each object kind.
- The week-1 hard heap cap remains. An allocation past the cap first
  runs a collection. The VM faults `HeapLimit` only when live data
  still exceeds the cap. The sweep raises dead slot generations, so a
  stale reference to a collected slot is detected.
- A frozen bit on every heap object. `freeze()` deeply freezes a
  graph with an iterative walk, keeps cycles and sharing, and returns
  the same reference. Every write into a frozen object faults
  `FrozenWrite`: fields, list elements, map entries, and builders.
  Strings and closures are born frozen.
- Classes: fields with mandatory types and optional pure defaults,
  one optional `init` with `mut self`, `self` and `mut self` methods,
  construction with `ClassName(args)`, field read and write, single
  inheritance with `class Child < Parent`, override checks,
  `super.init`, and `super.method`. The compiler synthesizes one
  `<new>` function for each class: allocate, store defaults, call
  `init`, return the instance.
- Definite-initialization checking for constructors: every required
  field is assigned on every path before its first read, `self` does
  not escape before completion, and `super.init` runs exactly once
  when the parent declares `init`.
- Dispatch is data-driven. The load step builds one sealed dense
  selector table for each class. A virtual call is two indexed loads:
  class slot, then selector slot. The interpreter loop performs no
  textual method lookup. `super` and `init` calls are direct calls.
- Closures: `do |x: Int|: Int ... end` literals, capture by value at
  creation, transitive capture through nested closures, a
  `CallValue` instruction, and closures as first-class values inside
  lists. Rebinding an outer local after capture does not change the
  capture.
- Collections as native types: `[T]`/`List[T]` and
  `{K: V}`/`Map[K, V]` in type positions, list and map literals,
  index syntax as sugar for `at`, and the methods `len`, `at`,
  `push`, `has`, and `put`. Maps keep insertion order. Map keys are
  `Bool`, `Int`, or `String`.
- `StringBuilder` with `append`/`build` and `ByteBuffer` with
  `append`/`len`/`build`.
- That slice used `"Hello {name}!"` interpolation for `Int`, `Bool`,
  and `String` values. It lowered interpolation to builder instructions.
  The current scanner uses `#{...}` and treats plain braces as text.
- Equality per specification section 6.4 for the in-scope types:
  scalars and strings by value; instances, lists, maps, closures,
  and builders by reference identity.
- Reference-capability checking: parameters are read-only without
  `mut`, `mut self` methods and mutating native methods need a
  mutable receiver, and field writes need a mutable reference.
- Bytecode format version 2 with type, selector, and class tables,
  plus the new instruction families for fields, calls, closures,
  collections, builders, freezing, and reference equality.
- The independent verifier covers every new surface. It checks table
  canonicality: the primitive type prefix, no duplicate type entries,
  only references to earlier entries, valid map key types, parent
  classes before subclasses, field layouts that extend the parent
  layout, method `self` types, and override signatures. The dataflow
  pass reconstructs operand and local types at each block entry and
  joins subclasses at their nearest common ancestor.
- `lm inspect --live <file>`: run the program, then print the
  outcome, heap statistics, active frames, and every live object in
  slot order. The format is deterministic.
- Value display for results: `[1, 2]` for lists,
  `{"red": 3, "blue": 2, "green": 1}` for maps in insertion order,
  and `Counter{value: 5}` for instances, with cycle and depth guards.
- Examples with checked output: `examples/02-objects/counter.lm`
  (`Done(5)`), `examples/02-objects/counts.lm`
  (`Done({"red": 3, "blue": 2, "green": 1})`), and
  `examples/02-objects/closures.lm` (`Done(42)`).
- Test suites: 183 tests. New suites cover the checker rules, the
  collector gates, corruption of every new format surface, run-pass
  and run-fault programs, capability rules, and compile-twice
  determinism for the new surfaces.

## Simplifications inside the slice

- String ordering with `<`, `<=`, `>`, and `>=` is not implemented.
  Specification section 6.4 defines lexicographic string comparison.
  The checker rejects a string ordering with `E1004`. The comparison
  arrives with the text work in a later week.
- The host-root API on the heap (`push_host_root`/`pop_host_root`) has
  no production caller yet. The interpreter roots values through the
  frame and operand arenas. A scoped RAII guard for host callers is
  future work; until then, a mis-nested pop fails with an assertion in
  every build profile.

- Map lookup was a linear scan over the insertion-order entries.
  The post-week-4 fix set added the hash index; see
  docs/notes/week4-fixes.md. Insertion order, equality, and
  display do not depend on this choice.
- An interpolation expression cannot contain a string literal or a
  brace. The scanner rejects these forms with `E0006`.
- `ByteBuffer.build()` decodes the bytes as UTF-8 and faults
  `BadCast` on invalid input. `append` checks the range `0..=255`
  and faults `IntegerOverflow` outside it. A `Bytes` type arrives
  with the core image.
- `StringBuilder.append` and `ByteBuffer.append` return the builder
  for call chains. `push` and `put` return `()`.
- Function types did not carry `mut` parameter markers in this week.
  The post-week-4 fix set closed the hole: function types and
  function records now carry the markers, and call sites check the
  argument capability. See docs/notes/week4-fixes.md.
- The verifier does not prove field initialization across the
  `<new>`/`init` function boundary. Fields start with an internal
  uninitialized marker, and a field read of that marker faults
  `UninitializedField`. This fault is an implementation subcode.
  Checked source programs cannot reach it; only hand-built bytecode
  can.
- A class without an explicit `init` needs a default on every field,
  inherited fields included. Its constructor takes no arguments and
  does not run the parent `init`.
- A parent class must be declared before its subclass.
- `super.init` is not valid inside a loop, and branch merges require
  the same `super.init` state on every path. A required field must
  be assigned before its first read; a later re-assignment inside
  `init` is permitted.
- A local name takes its reference capability from its first
  initializer. A later assignment of a read-only heap value to a
  mutable name is an `E1035` error.
- Equality on unit-typed values stays an `E1017` error. Class and
  function names are not first-class values (`E1018`).
- `freeze` is a reserved method name on classes.
- User-declared generic classes and functions are rejected with
  `E1024`. Tuples, enums, `case`, and `Option` arrive in week 3.
- The week-1 heap gate changed with the collector: dropped garbage
  under a tiny cap no longer faults, because collection reclaims it.
  A new gate proves that live data past the cap still faults
  `HeapLimit`.
- The full nesting depth near 100 source levels needs a standard
  8 MiB main-thread stack. The depth guard `E1022` covers the new
  recursive productions: nested literals, closure bodies, method
  bodies, and type annotations. The strict rule for `if` without
  `else` is unchanged.
- Generation checks on object references are always on, not only in
  debug builds. The cost is one integer compare per access.

## Deferred work

- CI workflow files and a Miri job. The code still has no `unsafe`,
  so the heap is safe Rust and the Miri job is not urgent.
- `cargo-fuzz` targets for the new decoder and verifier surfaces.
  These need a nightly toolchain. The corruption suite covers
  hand-written cases now.
- Real benchmark infrastructure with committed distributions for
  `List.push`, field access, virtual calls, allocation, and full
  collection. A timing smoke test
  (`crates/lm-testkit/tests/bench_smoke.rs`) prints durations for
  these paths and asserts completion.
- The map hash index, `Option`-based collection methods, `digest()`,
  and the boundary codec.
- Effect rows, `sys`, and host operations (week 4).
