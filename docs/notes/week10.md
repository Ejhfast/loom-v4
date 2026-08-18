# Week 10 Status

This note records the week 10 work so far. The week is not complete:
`docs/specs/build-order.md` section "Week 10" also lists TCP, the
platform adapters, `std/path`, root policy profiles, and `FileLease`
with `std/fs.with_open`. Those are not started. This note covers the
slice that landed.

Two pieces are the week. The first is scoped files and the handle
foundation of `docs/specs/sidecar/handles.md`. The second is the
driving surface a supervisor needs to use them: typed selectable
waits, bounded drive turns, and the request patterns that make a
driver readable. Everything else in this note supports one of those
two, or fell out of using them.

Bytecode format version 20, interface format 8, compiler ABI 16,
verifier 14, operation manifest ABI 7, snapshot container format 6.
The core image pin is
`14cd0e479b753c3fedf0c45123c1991f69278b041197e120a341499848b04676`
and `core/pinned-core-defs.txt` holds 68 definition hashes. The shape
table holds 20 shapes.

The branch is 135 files, about 15,000 lines added. It carries 16 new
test files, 16 new examples, and two new sidecar specifications.

## 1. Files and handles

`Fs.Open`, `Fs.Read`, `Fs.Write`, `Fs.Seek`, `Fs.Flush`, and
`Fs.Close` join the manifest. `FileHandle` is a typed designator, and
every live handle registers in the resource table of the machine that
owns it.

A holder gets a second view of the same resource. `Vm[T].handles()`
lists the resource controls of its controlled world, and
`Vm[T].resource(FileHandle)` names one. A `ResourceHandle` answers
`is_open`, `close`, `kind`, and `same_resource`, so a supervisor can
revoke a file it handed out.

A driver can also serve the request itself. `serve_file(call)` does
three things in one step: it registers a resource that the child owns
and the driver backs, it answers the pending `Fs.Open` with
`Ok(FileHandle)`, and it returns the driver-side control. The child
then performs ordinary file operations, and each one returns to the
driver as a request. No host file stands behind the entry.

A live handle is a host attachment, so it blocks a snapshot. A closed
handle is ordinary machine state, and a restored closed handle stays
closed. Example 11 keeps a closed control in a plain object field
across a capture.

## 2. Waits and the driving surface

`Wait[T]` is a holder-local one-shot value. `wait`, `choose`, and
`cancel` are its operations, and `select` is the source syntax over
two or more of them. A proc parks on one scheduler wait set.

`drive_wait` lends child execution to the scheduler for one quantum,
so a supervisor can wait for its child and for its own mailbox at the
same time. `drive_for(n)` bounds one turn and returns `None` when the
child neither finished nor asked. Together they let a supervisor stay
responsive to a child that never yields.

The request surface got the shape a driver actually writes. `Call(op,
call, args)` matches an exact operation and binds the typed token, and
`as_call` went with it, because one question had two spellings. A
wildcard arm reads `request.op_name()` for the operation as text.

## 3. Strings and bytes

The filesystem exchanges `Bytes`, so `Bytes` had to be a real type
before the filesystem could be one. `core/string.lm` and
`core/bytes.lm` declare `String`, `Bytes`, `StringBuilder`, and
`ByteBuffer` as final core classes over native storage.

`SharedText` and `SharedBytes` are reference-counted storage with a
start, a length, and a cached lookup hash. A slice clones the handle
and moves the window, so it copies nothing, and a text slice refuses a
position that is not a character boundary. The host boundary carries
`SharedBytes`, so a file read reaches the guest without a copy. That
is the property that makes `Bytes` a foundation rather than a wrapper.

Fallibility follows the read path. `slice` returns
`Result[Bytes, IndexError]`, `utf8()` returns
`Result[String, Utf8Error]`, `at` faults and `get` answers `Option`.
Bytes from a file are untrusted, so decoding is a result and not a
fault. `text()` is the faulting conversion and reports `BadCast`.

Specification 24.6 splits the surface into an implemented tier A and a
reserved tier B, so the thin `String` surface is a recorded staging
decision. `String.bytes()` is tier B; `Bytes(text)` covers the
direction the filesystem needs today.

Operators reach these classes through paired-underscore hooks. `Int`,
`Bool`, `String`, and `Bytes` declare `__add__` and the rest, opt-in
`final` classes allow a direct call, and trivial expression bodies
inline, so `a + b * 2` still emits `Mul` and `Add`.

## 4. What fell out of using it

These were not planned work. Each one is a defect that writing the
examples and the specification exposed.

- `vm.reject` could not be called. Nothing built a `Fault`, so a
  driver could not refuse a request whose reply carries no error
  value. `Fault.denied(reason)` builds the one fault a program can
  make. Appendix C of the specification called a helper that never
  existed and had never compiled.
- A `loop` had the type `()`, so every supervisor carried a dead tail
  expression to satisfy its declared result type. A loop with no
  `break` now has the type `Never`. Twenty programs lost a statement
  that could never run, and `Never` became writable for the first
  time.
- `args:` was one hardcoded label on `from_fn`. Specification 6.1
  already stated the general rule and used `args:` as its example, so
  the checker did not implement the specification it had. Native
  methods now declare parameter names.
- `mint_file` read as a constructor and answered the call. A driver
  that answered afterwards faulted itself. It is `serve_file` now, and
  the name states the duty the driver takes on.
- The operator hooks were core-only. The names promised an extension
  point that no program could reach. Any class declares them now.

## 5. Decisions

**A driver-backed file is an obligation, not a value.** `serve_file`
binds the resource to the driver, so every later operation on it
returns to that driver. A request that reaches the root host gives the
child an ordinary `FsError` instead. The alternative was a detached
handle the driver could ignore, which would have made a silent hole in
the file surface.

**A fault code that a program mints is fixed.** `Fault.denied` always
carries `PolicyDenied`. Nothing in the runtime branches on a fault
code, so a free constructor would have been safe and dishonest: a
program could claim `OutOfFuel`. The fixed code keeps the
machine-internal codes meaningful.

**A `Fault` value is pure.** `FaultDenied` is a plain instruction and
not an operation, so the manifest digest does not move and no artifact
changes. Only `reject` installs the value, and `reject` charges `Vm`.

**A loop tests its exit, not its body.** A body that returns on one
path and repeats on another still never reaches the statement after
the loop. The rule reads the absence of `break`, and the lowering
emits no exit edge, so the verifier skips the block after it.

**Operator hooks carry no rule.** A hook may return any type and may
declare an effect row, so `a + b` can perform an operation and the
caller must hold the row. The effect system is already the guarantee,
and forbidding rows would have been the one place in the language
where the answer to "can this have effects" is no rather than yes and
visible. A later interface feature can require properties of a class
that claims them.

**`__eq__` governs `==` alone.** `Map` keys, `digest`, and
`deep_equal` keep structural identity. A class can therefore make
`a == b` disagree with a map lookup. Specification 6.4 says so,
because the same gap is a common defect in other languages.

## 6. Open questions

- **`Request` inspection is still narrow.** `op_name` gives text.
  `q.op()` needs an identity-erased `Operation` value that version 0.2
  does not define. Design it with the deferred first-class
  `PolicyTarget` of `docs/notes/week4.md`: one erased target type
  serves both, and building two would be the mistake.
- **The builders carry four alias pairs.** `StringBuilder.append` and
  `push_string` have one body, and so do `build` and `finish`;
  `ByteBuffer.append` and `push`, and `build` and `finish`, do the
  same. Specification 24.6 lists both spellings of each. The project
  rule asks for one term per thing, and nothing records which name is
  the transitional one. Choosing now is cheap.
- **`fault.message()` does not exist.** A denial reason reaches a
  snapshot dump and an embedder, and no guest accessor reads it. An
  accessor would make every internal runtime message an observable
  interface. Threading the message into `WorkerOutcome` so `lm run`
  prints it is the smaller answer.
- **A canonical data digest moves when the operation manifest moves.**
  Every definition hash covers `manifest_digest`, so renaming an
  unrelated operation moved the checked digest of
  `examples/06-graphs/cycle-digest.lm`, whose own comment says the
  output never moves. If a graph digest is meant to be stable across
  toolchain versions, that coupling is wrong.
- **Nothing tests that the identity opcode bytes are distinct.**
  Merging two branches that each added instructions produced a silent
  duplicate: it compiled, and two instructions hashed alike. A
  duplicate-byte assertion over the identity encoding is cheap.
- **`while true` with a dead tail has no spelling.** A mock that must
  match an exact non-`Never` signature and never returns cannot be
  written directly. One week-4 mock uses `0 == 0` for its condition.

## 7. Deferred work

Carried from the build order and not started: TCP, the Unix and
Windows platform adapters, `std/path`, explicit finite root policy
profiles, `FileLease` as a scoped designator, `std/fs.with_open`,
`std/fs.open_handle`, and the process environment operations.

Deferred from the language surface: `q.op()`, `q.ordinal()`,
`q.args_view()`, `q.reply_type()`, tier B of the `String` surface,
`StringBuilder.push_char`, `Char` methods, and float parsing.

## 8. Maintenance note

The operation manifest moved this week, so every checked artifact
moved with it. `docs/notes/week9.md` lists two commands. The core pins
need a third, and this note records all three:

```sh
cargo test -p lm-testkit --test core_image regenerate_core_pins -- --ignored
lm snapshot save --allow Proc,Vm,Clock \
  checkpoints/asked-tree.lm checkpoints/asked-tree.lms
cargo test -p lm-testkit --test fuzz regenerate_fuzz_corpus -- --ignored
```

Each failing test names its own command, so a stale artifact cannot
pass unnoticed. One checked graph digest in
`crates/lm-testkit/tests/week7_graph.rs` also moves with the manifest,
and it names no command. See the open question above.
