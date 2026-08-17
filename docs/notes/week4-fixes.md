# Week 4 Fixes

This note records the fix set for the user-reported findings. The
findings came from the week-3 build. Each entry states the status on
the week-4 master, the fix, and the evidence. Bytecode format version
5 carries the new surfaces. The core pin moved to
`887c3bd83c4219bc45eee82e10702dcfadea9c26b3ef5e740f588a9debfa13f8`.

## 1. Static local widening survives verification (fixed)

Status on master: partly masked, still live. The week-4 sibling
relaxation hid the original `a is Cat` repro. Two variants still
failed: reference equality between two widened locals rejected with
"reference equality needs related object types", and a function-typed
local with different closure rows per branch failed with "load from a
local without a value" after a failed join.

The fix: every function record carries a declared local-type table
(`local_types`, one entry per slot). Lowering emits the checker slot
types and gives every scratch slot its true type, including the
`<new>` default temporaries and the pattern destructuring slots. The
verifier validates the table: entries must be valid types, variables
must be in scope, and the parameter prefix must equal the signature.
A store must fit the declared slot type. A load produces the declared
type. Local joins at merges are now trivial, because both sides hold
the declared type. `local_count` is now the table length, so a forged
count is a decode-time `BadLength`.

`lm check` now runs lowering and verification after the type check.
A check success means `run` admission.

Evidence: all seven widening variants run (`fixes.rs`), and three
corruption cases reject a forged table (narrowed entry, out-of-range
entry, wrong parameter prefix).

## 2. Labeled arguments (fixed)

Status on master: reproduced. Only `args:` on `from_object` was
accepted (`E1006`).

The fix: labels resolve against declared parameter names for direct
calls, method calls, class constructors, enum constructors, and
`super` calls. Labels follow the positional arguments and match in
any order. The checker reorders the arguments to declaration order at
check time; the call ABI does not change. Precise `E1006` errors
cover an unknown label, a duplicate label, a positional argument
after a label, and a label for a parameter that a positional argument
fills. A call through a function value has no declared names and
rejects labels. The `from_object` path is unchanged.

## 3. Sibling inference for arm-typed constructors (fixed)

Status on master: reproduced. `if`/`case` joins, list literals, and
map literals rejected a bare `None` with `E1045`.

The fix: when an element or branch fails with `E1045`, the checker
computes a hint: the family-widened join of the siblings that
resolved alone. It then checks every element or branch against the
hint, so nested constructor arguments adopt one shared
instantiation. The pass never invents a type and never searches.
`[None, None]` and all-`None` branch joins keep the `E1045` error.
The rule covers `if`/`elsif` chains, `case` joins, list literals, map
literal values, nested cases such as `[Some(None), Some(Some(1))]`,
and user generic enums.

## 4. `mut` markers in function types (fixed, full carry)

Status on master: reproduced. A read-only list parameter passed into
a closure `mut` parameter mutated (`Done(2)`).

The fix carries `mut` end to end. `Type::Fn` and `BcType::Fn` hold
one marker per parameter, and function records hold `param_muts`.
The fn-type syntax accepts `(mut [Int]) -> ()`. Subtyping rejects a
`mut`-requiring function where the expected type promises a read-only
call, in the checker and in the verifier. A call through a function
value requires mutable capability at `mut` positions. Overrides must
keep the markers in the verifier too. The repro now rejects with
`E1035`, and a flipped marker byte in a module is a verifier
rejection; an invalid flag byte is a decode error.

## 5. Constructor collision note (fixed)

Status on master: reproduced. A user enum arm `Pair` lost to the
prelude `Pair`, and the error was only "expected Pairing, found
Pair[Int, Int]".

The fix: when a call mismatch names a constructor and the expected
enum has a same-named arm, the one `E1004` diagnostic gains a note:
"the enum `Pairing` has an arm named `Pair`; write
`Pairing.Pair(...)` to select it".

## 6. Nested exact-arm exhaustiveness (fixed)

Status on master: reproduced. `s = Some(Some(3)); case s in
Some(Some(v))` reported `E1042`.

The fix: the exhaustiveness matrix injection is now recursive. An
arm-typed position excludes its sibling arms at the top level and
inside every arm-typed field position. The injected rows cover only
values outside the static scrutinee type, so family-typed positions
still need full coverage.

## 7. Map hash index (fixed)

Status on master: reproduced. Insertion was a linear scan.

Before: 4,000 insertions in under 10 ms; 32,000 in about 110 ms
(release CLI, 8x entries cost more than 20x time). After: 32,000 in
under 10 ms; 256,000 in about 55 ms. 8x entries now cost about 8x
time.

The fix: each map holds a derived index from key hash to entry
indices. The index is a cache: lookups index the appended suffix on
demand, so `put`, `has`, and `at` are amortized constant time.
Iteration, display, equality, and digest semantics still use the
insertion-ordered entries. The index holds hashes and positions only
and never an object reference, so the tracer and the freezer skip it
by design. The logical heap cost still charges the entries; the index
adds one bounded bucket entry per map entry, and this note documents
that choice.

## 8. Literal string interning (fixed)

Status on master: reproduced. One million literal loads created
1,000,001 live heap objects, 37 MB of logical heap, and 96 MB of
process memory in 0.10 s.

After: the same program holds 2 live objects, 69 logical bytes, and
3.3 MB of process memory in 0.06 s.

The fix: each machine holds a literal table indexed by the module
string pool. The first `ConstStr` allocates one frozen object; every
later load reuses it. Literals are collection roots for the machine
lifetime. Boundary transfer is unchanged, because interned strings
are frozen strings.

## 9. Compressed dispatch rows (fixed)

Status on master: reproduced by construction. The table was dense
over classes times selectors: about 4,000 classes and selectors cost
64 MB.

The fix: each class row spans only the selector range the class
answers, with a base offset. The hot path stays an indexed load
chain: subtract the base, index the row. The 300-class smoke now
builds 342 cells against a dense equivalent of 102,364 cells; the
4,000-class shape drops from 64 MB to the method count times 4
bytes. The virtual-call smoke is unchanged.

## 10. Scaling smokes (added)

`bench_smoke.rs` gained three checks: map insertion at 4,000 and
32,000 entries (printed timings), the literal loop with a structural
live-object assert (under 16 objects after 200,000 loads), and the
many-class dispatch shape with a structural cell-count assert (under
one tenth of dense).

## 11. Verifier dataflow cost (note only)

Re-measured on master before these fixes: generated programs with
100x100, 200x200, and 400x400 locals times branches, with class-typed
locals that force joins, all verify in under 10 ms in the release
CLI. The week-3 blowup (2.3 ms to 45.8 ms at 4x) is gone; the week-4
cell budget bounds the state. After the declared local tables, local
merges compare equal declared types, which removes the per-store
re-join work entirely. No further work is planned.

## Expectation changes

- The core pin moved with format version 5 (expected churn); the
  determinism gates pass unchanged.
- The fuzz corpus regenerated for format version 5. The local-count
  bomb now patches the encoded local-type table count, and the
  decoder length guard rejects it.
- Three collector gates in `gc.rs` churned garbage with string
  literals. Literals now intern, so the churn uses list literals.
  The gate meanings are unchanged.
- The stray-label diagnostic text changed from the `from_object`
  wording to the general rule.
- `check_if` now checks every condition before the branch bodies. A
  later condition runs only when the earlier bodies were skipped, so
  it now checks against the fork entry constructor state. No existing
  test changed.

## Deferred

- The differential oracle still models the pure subset only; the new
  semantics (labels, sibling inference, the `mut` rule) are in the
  shared checker, and the corpus gained covering programs.
- Real benchmark infrastructure with committed distributions stays
  deferred as before; the smokes stand in.

## Driver regression fix

The week 4 driver returned to the activation-stack dispatch after
every instruction, and the four workload timings roughly doubled
against week 3. The fix batches ordinary instructions: run and
drive execute a tight loop inside the machine until an instruction
reaches a world boundary. Step keeps exact one-instruction
semantics. Fuel decrements per instruction inside `exec_instr`, so
`OutOfFuel` stays exact. Measured recovery: integer loop 72 to 33
ms, direct calls 95 to 46 ms, virtual calls 146 to 71 ms, list
allocations 172 to 114 ms, with process start included. The user
supplied the fix on the `batch-vm-boundaries` branch.
