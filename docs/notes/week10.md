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

The text and byte storage pass now supports later network work.
Section 6 records its architecture and limits.

Bytecode format version 21, interface format 8, compiler ABI 17,
verifier 15, operation manifest ABI 7, snapshot container format 7.
The core image pin is
`97773efbe8eed25d8099225593578ae893f24c2688cd83ebdf989f2aea452075`
and `core/pinned-core-defs.txt` holds 71 definition hashes. The shape
table holds 21 shapes.

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

The filesystem and network exchange Bytes. Text decoding now produces
either a bounded String or an explicit shared Substring.

`Text` is a sealed abstract core class. Final classes String and
Substring provide its two concrete forms.

String is the durable text value. Its retained capacity cannot exceed
`max(4096, 2 * byte_len)`.

Substring is an explicit view. It can retain any source allocation.
`to_string` and `compact` enforce the String retention bound.

String, Substring, and Bytes can share one immutable byte allocation.
One heap charges that allocation once for all local views.

`Text.bytes` shares storage. `Bytes.utf8_view` validates and returns a
shared Substring.

`Bytes.utf8` validates and returns a bounded String. `Bytes.compact`
copies only the visible byte range.

Each byte view caches its UTF-8 validation result. Conversion does not
repeat the validation pass.

Text methods use Unicode scalar positions by default. `len`, `at`,
`slice`, `find`, `each`, and `map` use scalar values.

`byte_len`, `slice_bytes`, and `find_bytes` support byte-oriented code.
Byte slices reject a position inside a UTF-8 scalar.

One lazy sparse index records every 64th scalar. Later scalar boundary
lookups scan at most 63 scalars.

`each` and `map` decode each scalar once. They use a forward UTF-8 byte cursor.

`find_bytes` avoids the scalar conversion after a byte search. This
method keeps byte-oriented parsing to one search path.

Char uses an immediate VM value. `Text.at` does not allocate a Char
object.

Text equality and ordering compare visible content. String and
Substring also use one content relation for map keys.

The VM keeps map hashes internal. The public surface does not expose
`__hash__` or a process-specific hash value.

StringBuilder and ByteBuffer own unique mutable buffers. `build`
copies, while `finish` invalidates the builder.

ByteBuffer transfers its buffer. StringBuilder compacts first when its
retained capacity exceeds the String bound.

Operators reach these classes through paired-underscore hooks. Final
receivers allow direct calls, and trivial bodies inline.

The extraction surface follows one rule. A method that narrows its
receiver gives a Substring and copies nothing. A method that builds
new content gives a String. `split`, `lines`, `trim`, `trim_start`,
`trim_end`, `strip_prefix`, and `strip_suffix` give views.
`to_lower_ascii`, `to_upper_ascii`, and `replace` give durable values.

Every text method is total. Section 12.1 of the specification states
the rule: any argument can come from a file or a socket, so an
argument range is untrusted input. `split` with an empty separator
matches at every scalar boundary, and `parse_int` reports
`ParseIntError.BadRadix` rather than faulting.

`split_once`, `strip_prefix`, and `strip_suffix` give a valid piece by
construction. A parser reaches a key and a value with no `case` over a
boundary error that its input cannot cause.

Interpolation accepts any Text. Every narrowing method gives a
Substring, so the earlier String-only rule made a program copy for the
most common use of a piece.

`examples/11-text-and-bytes` holds five programs: a configuration
parser, an untrusted decode, a request head reader, a report builder,
and the view-against-durable-value contrast.

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

**`__eq__` governs `==` alone.** Map lookup never calls a user hook.
Text keys use one built-in content relation across String and
Substring. Other classes keep structural identity.

## 6. Text storage decisions

The implementation uses flat UTF-8 storage. It does not use ropes or
a permanent tree.

Flat storage keeps host boundaries and network writes simple. It also
keeps contiguous byte access available without flattening.

String uses bounded retention because it is the durable value.
Substring permits arbitrary retention because its type makes that cost
explicit.

Bytes follows the explicit-view rule. `compact` gives callers direct
control over retained binary storage.

Text and Bytes share physical storage because both expose immutable
byte ranges. UTF-8 metadata stays on the Text view.

Scalar indexing is the default because the surface presents text.
Explicit byte operations remain available for protocols and parsers.

Text does not normalize Unicode. Automatic normalization would change
content, equality, and protocol bytes.

Scalar traversal uses `each` and `map`. Version 0.2 still has no
iterator trait hierarchy.

## 7. Open questions

- **`Request` inspection is still narrow.** `op_name` gives text.
  `q.op()` needs an identity-erased `Operation` value that version 0.2
  does not define. Design it with the deferred first-class
  `PolicyTarget` of `docs/notes/week4.md`: one erased target type
  serves both, and building two would be the mistake.
- **Allocation cost sets every losing text ratio.** Loom pays about
  104 ns for one object where CPython pays about 25 ns, so every case
  that allocates once per operation loses by that factor and nothing
  else. `docs/notes/week10-text-bench.md` shows it, and the language
  table already showed it through `option_case` and `class_init`. The
  work has reach across the language and is not text work.
- **Unicode operations remain outside core.** Normalization, case
  folding beyond ASCII, and grapheme segmentation need a pinned
  Unicode data version. Their package placement remains open.
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

## 8. Not started

From the build order: TCP, the Unix and Windows platform adapters,
`std/path`, explicit finite root policy profiles, `FileLease` as a
scoped designator, `std/fs.with_open`, `std/fs.open_handle`, and the
process environment operations.

Waiting on a type that version 0.2 does not define: `q.op()`,
`q.ordinal()`, `q.args_view()`, `q.reply_type()`, and float parsing.

## 9. Maintenance note

The operation manifest moved this week, so every checked artifact
moved with it. `docs/notes/week9.md` lists two commands. The core pins
need a third, and this note records all three:

```sh
nix-shell --run "cargo test -p lm-testkit --test core_image \
  regenerate_core_pins -- --ignored"
nix-shell --run "lm snapshot save --allow Proc,Vm,Clock \
  checkpoints/asked-tree.lm checkpoints/asked-tree.lms"
nix-shell --run "cargo test -p lm-testkit --test fuzz \
  regenerate_fuzz_corpus -- --ignored"
```

Each failing test names its own command, so a stale artifact cannot
pass unnoticed. One checked graph digest in
`crates/lm-testkit/tests/week7_graph.rs` also moves with the manifest,
and it names no command. See the open question above.
