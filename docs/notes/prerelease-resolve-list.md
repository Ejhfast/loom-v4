# Pre-release Resolve List

This note records an adversarial pass over the language surface. It
lists what a user meets before a release, and what each item needs.

The pass ran against `59eecc8`. An earlier pass ran against `fce7246`,
and this note replaces it. Section "Resolved since the first pass"
records what closed between the two.

The pass targeted the complex areas first: nested machine effect
routing, snapshot capture and restore, and interface conformance. It
also collected small warts.

The pass found no soundness defect.

## Method

Each probe is one small program that a user could write. The pass ran
it through `lm run` and read the result. A probe that failed for a
mistake in the probe does not appear here.

Each item states how the pass checked it. "Read" means the pass read
the source. "Probe" means the pass ran a program and read the result.

The operation table now holds 106 operations in 13 groups: `Vm` 58,
`Proc` 10, `Tcp` 9, `Tls` 7, `Fs` 6, `Io` 3, `Clock` 3, `Wait` 3,
`Compiler` 3, `Rand` 1, `Dns` 1, `Choose` 1, `Reflect` 1.

## What held

The list below reads correctly only beside this section. The pass
re-ran every case here against `59eecc8`.

- Three levels of machine route one effect through two `pass` tables
  to the root, and the root answers it.
- A snapshot taken at a pending request keeps that request. Two
  restores from one image run independently and give different
  answers.
- A snapshot refuses while the machine holds an open file handle.
- **A snapshot carries a spawned proc.** A machine that spawns a proc,
  sends it a message, and stops at an effect captures and restores.
  The restored machine holds the live proc.
- **The container round trip carries a proc world.**
  `checkpoints/asked-tree.lm` saves a world of three machines and two
  mailboxes into 1117 bytes. `lm snapshot verify` reports `state=asked
  machines=3 mailboxes=2`, and `lm snapshot run` restores that world
  and resumes it at `Asked(Clock.Now)`.
- **The compiler and reflection surface runs.** All eleven programs of
  `examples/15-compiler-and-hot-code-reloading` answer correctly. They
  cover `sys.compiler.compile`, `sys.compiler.compile_syntax`, and
  `sys.reflect.parse_syntax`. The untrusted-code program reports
  `PolicyDenied`, so a grant still bounds compiled code.
- A request token names one machine. Machine B rejects the token of
  machine A with `InvalidRequestToken`.
- A second answer to one token faults with `InvalidRequestToken`.
- `drive` on a finished machine returns `Done` again.
- `mock` takes an exact operation only. The checker rejects a group,
  and it checks the handler signature.
- The command line rejects a snapshot that names another program.
- `case` exhaustiveness, conformance, and arity diagnostics name the
  cause and the position.

The `Vm` type moved to `Run[T]` for an active invocation. The
migration diagnostic names the repair: "`Vm` takes no type arguments;
use `Run[T]` for an active invocation".

## Resolved since the first pass

The pass confirmed each of these against `59eecc8`.

- **A runtime fault names its source position.** A fault now prints a
  call chain, for example `at level1 (file.lm:7:1, bytecode 2,
  40408322)`. This closed the first blocker.
- **`then` accepts `return`.** `in 1 then return 5` gives `Done(5)`.
- **The propagation operator `?` exists.** `v = inner()?` gives
  `Done(Ok(6))`.
- **The examples spell unary minus.** No `0 - N` remains under
  `examples/`.
- **`Compiler` and `Reflect` carry operations.** Both were reserved
  groups with no operation. `Compiler` now holds `Verify`, `Compile`,
  and `CompileSyntax`. `Reflect` holds `ParseSyntax`.

Two pieces of residue remain, and the list holds them as items 12 and
16.

## Blockers

### 1. `SnapshotError` carries no information

`vm.snapshot()` returns `Err(e)`. The value has no `code` method and
no `message` method. Interpolation rejects it.

A refusal has many causes: an open resource, an unreachable machine, a
budget, or a value that does not send. The user separates none of
them.

`NetError` has `message`. `Fault` has `code`. `SnapshotError` has
neither.

*Checked: probe.*

## Language gaps

### 2. An interface cannot name the conforming type

`Self` is not a type. It is the left half of the projection
`Self.<AssociatedType>`, and it works inside an interface contract
only.

| Position | Result |
| --- | --- |
| `def clone(self): Self` in an interface | `E1013` unknown type name `Self` |
| `def me(self): Self` in a class | `E1013` unknown type name `Self` |
| `def same(self, other: Self)` in an interface | `E1013` unknown type name `Self` |
| `next: Option[Self]` field | `E1013` unknown type name `Self` |
| `Self.Item` in a class | `E1053` `Self` is available only in an interface contract |
| `Self.Item` in an interface | works |

**Effect.** A contract cannot promise that a method returns the
conforming type. `clone`, `same`, and a builder chain are all
unwritable.

The associated type workaround compiles and promises nothing. This
program runs and answers `"not a Box at all"`:

```
interface Cloneable
  type Me
  def clone(self): Self.Me
end
final class Box implements Cloneable
  type Me = String
  def clone(self): String
    "not a Box at all"
  end
end
```

Nothing binds `Me` to `Box`, so the contract holds no meaning.

*Checked: probe.*

### 3. Two interfaces cannot share a method name

```
interface A  def name(self): String  end
interface B  def name(self): String  end
final class C implements A, B ...
def both[T: A + B](x: T): String
  x.name()
end
```

This gives `E1053`: the interface method `name` is ambiguous. The
class holds one method, and both contracts name the same signature.

A single bound `[T: A]` works, so the class conforms correctly. Only
the joined bound fails, and the language has no syntax to choose a
contract.

**Effect.** Interfaces do not compose. A second interface that reuses
a common name such as `name`, `size`, or `value` blocks every joined
bound.

*Checked: probe.*

### 4. The core carries two interfaces, and closed lists elsewhere

The core declares `Iterable` and `Iterator` and nothing else. Two
capabilities that belong to an interface are closed lists instead.

**A map key reads from a closed list.** A user class as a key gives
`E1033`: a map key must be `Bool`, `Int`, `Text`, `String`,
`Substring`, or `Bytes`. Six built-in types, and no `Hashable`
contract. A user class can never be a map key.

**Interpolation reads from a closed list.** A user class in a string
gives `E1034`: this slice interpolates `Int`, `Bool`, and `Text`.
Three built-in types, and no `Display` contract.

The operator hooks come close and do not reach either. A class may
declare `__add__`, `__sub__`, `__mul__`, `__div__`, `__rem__`,
`__neg__`, `__eq__`, `__ne__`, `__lt__`, `__le__`, `__gt__`, `__ge__`,
and `__not__`. There is no `__hash__` and no `__text__`.

`docs/notes/week10.md` records a decision beside this one: "`__eq__`
governs `==` alone. Map lookup never calls a user hook." So a
`Hashable` contract needs that decision again, and it is not an
oversight.

`Iterable` is the precedent. It already lets `for` read a user type.

*Checked: probe and read.*

### 5. The language carries no `panic` and no `expect`

`panic("boom")` gives `E1005`: cannot find a function named `panic`.

`Option` and `Result` carry no `expect` and no `unwrap`. All three
calls give `E1026`, no method named.

**Effect.** Every fallible access needs a full `case`, even where the
program knows the value is present. This lengthens every example.

*Checked: probe.*

### 6. `then` accepts `return` and rejects `break`

`in P then return 0` now parses. `in P then break` still gives
`E1001`: expected an expression, found `break`.

`return` became an expression. `break` and `continue` did not, so the
two arm forms still do not accept the same text.

*Checked: probe.*

### 7. Loom has no floating point

`x = 1.5` gives `E0005`: float literals are not supported in this
language slice.

*Checked: probe.*

## Diagnostics that name the wrong cause

### 8. An interface in type position reads as an unknown name

`x: Priced = Book()` gives `E1013`: unknown type name `Priced`.

`Priced` is a known interface. The language documents the rule: an
interface names a bound, and it is not a type. The compiler does not
say so.

*Checked: probe.*

### 9. A bare associated type gives no direction

`def get(self): Item` inside an interface gives `E1013`: unknown type
name `Item`. The correct text is `Self.Item`. Nothing points there.

The compiler holds a precise message for the other direction:
`Self.Item` inside a class gives `E1053`, which names the rule. The
two paths do not match.

*Checked: probe.*

## Small warts

### 10. A dropped message needs no acknowledgement

`h.send(2)` after `h.close()` answers `Dropped`. A program that
ignores the result loses the message in silence.

`SendResult` holds `Sent` and `Dropped`, so the design is right. The
language accepts a discarded result of any type.

*Checked: probe.*

### 11. `lm --help` reports an error

`lm --help` prints `error: unknown command --help` above the usage
text, and it exits with a failure code.

*Checked: probe.*

### 12. The examples still answer with negative integers

The `?` operator closed the language gap. The examples did not move
with it. They hold 17 arms that answer with a negative integer, for
example `in Fault(_) then -1`.

An integer error code carries no type. It collides with a real
result, and the checker sees nothing wrong.

The examples teach this shape to every new reader.

*Checked: probe.*

### 13. An interface cannot extend an interface

`interface B: A` fails to parse with `E1001`.

*Checked: probe.*

### 14. `b"..."` is reserved and unimplemented

`b"hello"` gives `E0009`: byte-string literals are not supported in
this language slice. `Bytes("hello")` works.

The diagnostic states that the scanner holds the syntax already. The
same shape covers float literals, which give `E0005`.

Interpolation now uses the `#{x}` marker. Plain strings keep all other braces as text.

*Checked: probe.*

## Missing capabilities

An item belongs in this section only when neither user code nor a core
module can supply it. A capability that a library can supply stays
out, unless it carries a cost that a library cannot avoid.

### 15. `Fs` has no directory or metadata surface

`Fs` holds six operations: `Open`, `Read`, `Write`, `Seek`, `Flush`,
and `Close`. The whole host crate calls `std::fs::read` and
`std::fs::remove_file`, and the second one is internal cleanup at
`crates/lm-host/src/lib.rs:889`. No operation exposes it.

A program cannot list a directory. It cannot delete, rename, or move
a file. It cannot read a size, a time, or a permission. It cannot ask
whether a path exists.

*Checked: read.*

### 16. Loom cannot read the environment

No group carries an environment operation. `env::var` appears nowhere
in `lm-host`, `lm-cli`, or `lm-vm`.

A program cannot read `HOME`, `PATH`, or any configuration that the
environment carries.

**A secret needs a separate decision, and the scope is open.** One
reading is a library over `Fs` and `Env`: read a secret file, keep the
value out of a log, and redact it. That reading needs no new
operation. The other reading is a keychain of the operating system,
which needs a new effect. The two cost very different amounts.

An environment variable carries a secret badly, because every child
process inherits it.

*Checked: read.*

### 17. A program cannot read its arguments

`crates/lm-cli/src/main.rs` reads `std::env::args()` for the flags of
`lm` itself. Nothing carries an argument to the program.

`lm run probe.lm extra-arg` accepts `extra-arg` and ignores it. It
reports no error.

*Checked: read and probe.*

### 18. Stdin carries no byte path

`Io.ReadBytes` is the only input operation. The host answers it with
`std::io::stdin().lock().read_line()`, which fills a `String`.

| Input | Result |
| --- | --- |
| `printf 'hello\n'` | `Done("got: hello")` |
| empty stdin | `Done("eof")` |
| `printf '\xff\xfe\n'` | `Done("error")` |

The first two rows are correct. `Io.ReadBytes` strips the line ending
and reports the end of input as `Ok(None)`.

The third row is the gap. A program cannot read bytes from stdin, so
it cannot read a pipe that carries anything except text.

This breaks one symmetry. `Fs.Read` answers with `Bytes`, and
`Bytes.utf8_view` validates once and shares the allocation. Stdin
skips that design.

*Checked: read and probe.*

### 19. A closed output pipe ends the program with a fault

`Io.Write` and `Io.WriteError` both answer with `AbiType::UNIT`. A write
that fails has no path back to the program.

A reader that stops early closes the pipe. The program then meets a
fault:

| Command | `lm` exit | Standard error |
| --- | ---: | --- |
| `lm run p.lm \| head -1` | 1 | `Fault(HostFault)` and a call chain |
| `lm run p.lm 2>&1 \| head -1` | 101 | none |
| `lm run p.lm > /dev/null` | 0 | none |

Row one ends a correct program with a fault. Row two ends it with 101,
which is a Rust panic. The panic follows the fault, because the fault
report writes to the same closed pipe.

`head`, `less`, and every other early reader produce this.

*Checked: read and probe.*

### 20. A program cannot turn a snapshot into bytes

`vm.snapshot()` answers `Ok(RunSnapshot[T])`. The value carries no way
to reach its bytes. Every one of these gives `E1026`, no method named:
`bytes`, `to_bytes`, `encode`, `serialize`, `save`, `write`, `store`,
and `len`. Only `digest` answers, and it gives a `Digest`.

`Fs.Write` takes `Bytes`. So a program holds a snapshot, and it cannot
write that snapshot to a file.

**The host already does this.** `lm snapshot save --allow Vm p.lm
out.lms` writes a 507-byte container and reports `valid: state=ready
machines=1 mailboxes=0`. The command line reaches the encoder, and the
language does not.

**Effect.** A snapshot lives and dies inside one process. A program
cannot checkpoint to disk, send a snapshot over a socket, or hand one
to another machine of the operating system. Restore works only from a
snapshot that the same run captured.

This removes most of the reason to capture a snapshot. The language
builds the world, proves it admits, and then cannot keep it.

*Checked: probe.*

### 21. Loom cannot start another program

No group carries a subprocess operation. `std::process::Command`
appears nowhere in `lm-host` or `lm-cli`.

`Proc.Spawn` starts a Loom proc inside the world. It does not start a
program of the operating system. The two are separate needs.

*Checked: read.*

### 22. `Set` belongs in a core module

The language holds `List` and `Map` and no `Set`. A set literal fails:
`{1, 2, 3}` gives `E1003`, because the parser reads a map literal.

A user can write a set today. This class compiles and answers
`(2, true, false)`:

```
final class Set[T]
  items: {T: Bool} = {}
  def add(mut self, value: T)    self.items.put(value, true) end
  def has(self, value: T): Bool  self.items.has(value)       end
  def len(self): Int             self.items.len()            end
end
```

So `Set` is not a language gap. It is still not a good thing to leave
to users. `docs/notes/week10-fs-bench.md` records the reason: a core
method names a raw intrinsic, and user code cannot, because
`intrinsic` needs core scope. The note measures about 104 ns for each
allocation and about 168 ns for each fallible return. A user-written
set pays an `Option` on every lookup. A core set does not.

Item 4 carries a related decision. A `Set` over user classes needs a
`Hashable` contract first.

*Checked: probe.*

### 23. Loom has one integer type

`Int` is a checked 64-bit integer. `9223372036854775807 + 1` gives
`Fault(IntegerOverflow)`, so the arithmetic is correct.

`Int64`, `Int32`, `UInt`, `Uint`, `U8`, and `Nat` all give `E1013`,
unknown type name.

This costs the most in protocol code, which is a target of the
language. `Bytes`, `ByteBuffer`, TCP, TLS, and the HTTP codec example
all exist, and `bytes.at` answers with `Int`. A length prefix or a
checksum needs masking by hand.

*Checked: probe.*

### 24. A program cannot ask about its terminal

No operation reports whether a stream is a terminal. No operation
reports the size of a terminal. No operation sets the terminal mode.

These need system calls, so no library can supply them. Most terminal
work does not: colour, cursor movement, and layout are text that
`Io.Write` already carries.

These belong in a separate `Tty` group, and not in `Io`. `Io` works on
any stream. Every terminal operation fails without a terminal, and the
terminal mode is one global resource that a fault must restore. A
grant of `Io` must not carry the power to change the terminal.

`Tty` classifies as `HostAttachment`, which matches `Io`, `Fs`, `Tcp`,
and `Tls`. Terminal state stays outside a snapshot.

This item ranks below every other item in this section.

*Checked: read.*

### 25. The command line answers with two exit codes

`crates/lm-cli/src/main.rs` returns `ExitCode::SUCCESS` at 15 places
and `ExitCode::from(1)` at 5 places. It returns nothing else.

A program cannot choose its own status. A tool that reports "found
nothing" separately from "failed" cannot do so.

*Checked: read.*

## Reading the list

Item 1 blocks a release. A user who meets a refused snapshot cannot
act on it.

Items 2, 3, and 4 bound what a user can express with interfaces. All
three appear as soon as a second interface exists. Item 4 also blocks
a `Set` over user classes, which is item 22.

Item 5 costs the most keystrokes and the least engineering.

Items 15 to 21 decide whether Loom writes ordinary tools. A program
that cannot list a directory, read its arguments, read the
environment, or survive `| head` is not a tool yet.

The semantics under the list are correct. The work ahead is the
surface the user reads.

## Items that this pass did not test

- `Self` as a value or a constructor, for example `Self()`.
- `Self` inside a generic bound.
- Interface conformance across module boundaries.
- The 58 `Vm` operations one at a time. The pass ran the examples that
  use them, and it did not probe each operation.
