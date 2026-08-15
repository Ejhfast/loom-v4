# Week 3 Status

This note records what landed in week 3, the known simplifications,
the changed tests, and the deferred work.

## Landed

- Generic classes, functions, and methods with explicit type
  parameters (`class Box[T]`, `def choose[T](...)`). Class arguments
  are invariant. The checker checks each generic body once with type
  variables. The bytecode holds one shared body; there is no
  monomorphization. Call and allocation sites carry dense type
  applications (`apps` table) that the verifier substitutes into the
  callee signature.
- First-order generic argument inference. The checker binds type
  parameters from the expected result and from the synthesizable
  arguments, then checks the remaining arguments against the
  substituted types. A call without a unique solution is an `E1045`
  error that asks for explicit arguments. Inference never searches
  and never invents `Any`.
- Tuples: fixed arity, structural, immutable, covariant elements,
  compile-time-literal indexing only (`t[0]`), one-element tuples
  with a trailing comma, and a maximum portable arity of 16. The
  unit literal `()` is also an expression now.
- Enums: `enum` declarations lower to one abstract closed parent
  class plus one final case class per arm. Methods after the arms
  live on the parent and dispatch through the sealed selector
  tables. Constructors use canonical qualified names
  (`Option.Some(1)`) or unqualified names when the expected type,
  the scrutinee, or a unique in-scope family selects the arm.
- Patterns: wildcard, binding, Int/Bool/String literal, and
  constructor patterns with nesting. Exhaustiveness is proven for
  `Bool` and sealed enums with the standard specialized-matrix
  usefulness algorithm under a fixed work budget (`E1049` past the
  budget). Duplicate and unreachable arms are `E1043` errors.
  Non-exhaustive cases are `E1042` errors.
- Flow refinement: inside the true branch of `if local is Type`, the
  branch scope shadows the name with a narrowed local behind a
  checked cast, so the verifier sees the narrowed type. The
  refinement does not survive the branch. `as` casts check the
  nominal relation statically and fault `BadCast` at run time.
- `Never` joins in `if` and `case` branch typing; diverging branches
  do not contribute to the join.
- Effect-row representation: `with` rows on functions, methods,
  closures, and function types; `effect e` parameters on generic
  definitions; rows stored in signatures, HIR, and bytecode. The
  checker charges every call with the substituted callee row and
  requires inclusion in the declared row (`E1046`). Overrides may
  narrow but not widen their row. The verifier independently stores
  and checks claimed rows: canonical order, variable bounds, call
  inclusion, and override inclusion. Row inclusion resolves an exact
  identity `G.Op` inside its group name `G` by text; the real
  operation manifest arrives in week 4.
- The interned type DAG gained `Inst`, `Tuple`, `Var`, and rows on
  function types. Subtype queries are memoized. Joins cover nominal
  ancestors with equal arguments and element-wise tuples.
- Pinned source-defined core image: `core/option.lm`,
  `core/result.lm`, `core/ordering.lm`, `core/pair.lm`, and
  `core/range.lm` are ordinary source compiled by the same pipeline
  into every module, after the user definitions, so user definition
  indices stay stable. `lm_hir::core_image()` compiles the core
  alone; its encoded bytes are pinned by SHA-256 in
  `core/pinned-hash.txt`. A test recompiles the image, compares the
  bytes, and compares the hash against the pin, and fails loudly
  with the new hash on a deliberate change.
- Prelude as a pure name-import layer: `Option`, `Some`, `None`,
  `Result`, `Ok`, `Err`, `Ordering`, `Pair`, `Range`, `List`, and
  `Map` resolve unqualified. Core identity never depends on the
  prelude: the core image compiles with the prelude off, and tests
  prove a prelude-free program compiles to identical bytes.
- `List.get(i) -> Option[T]` and `Map.get(k) -> Option[V]` complete
  the collection surfaces without changing the week-2 storage. The
  lowering expands `get` into bounds tests around the existing
  instructions plus calls of the pinned core `Option` constructors.
  The faulting `at` and index forms are unchanged.
- Bytecode format version 3: type applications, rows on functions
  and function types, `Inst`/`Tuple`/`Var` type entries, class
  generic arity and kind (normal, abstract enum parent, final case),
  and the instructions `CallG`, `CallVirtualG`, `NewG`, `TupleNew`,
  `TupleGet`, `IsType`, and `CastType`. The verifier rejects
  allocation of an abstract parent and subclassing of a case class.
- A test-only typed-HIR oracle (`lm-testkit/src/oracle.rs`, not part
  of `lm-cli`). Differential tests run every run-pass case, every
  run-fault case, all seven examples, and twenty hand-written
  feature programs through the oracle and the verified-bytecode VM
  and require identical terminal text. The corpus covers generics,
  enums, patterns, tuples, refinement, faults, and freezing.
- Examples with checked output: `examples/03-types/expr.lm`
  (`Done(42)`) and `examples/03-types/generics.lm`
  (`Done(("yes", "no"))`).
- Negative UI examples: `non-exhaustive-case.lm` (`E1042`),
  `invariant-list.lm` (`E1004`), `ambiguous-generic.lm` (`E1045`),
  `row-widening-override.lm` (`E1046`), `unreachable-arm.lm`
  (`E1043`), and the kept `self-escape.lm`.
- Checker complexity tests: a 300-arm enum match, deep generic
  nesting, a 200-branch join chain, 200 distinct instantiations, and
  nested pattern analysis, each under a wall-clock bound.
- A defect fix found during the work: a field default that contains
  checker temporaries (a `case`, pattern binds) now has its local
  slots moved into fresh `<new>` scratch slots during lowering.
- A second fix: deep `freeze` now uses a visited set, so it passes
  through born-frozen containers (tuples, closures) and freezes
  their mutable children. The week-2 walk stopped at the first
  frozen object.

## Simplifications inside the slice

- `Any`, `DynValue`, `Float`, `Byte`, `Char`, `Bytes`, and `Digest`
  stay out of the slice. `is`/`as` work on nominal class and enum
  instance types only (`E1047` otherwise).
- A generic class cannot take part in inheritance (`E1024`), except
  the internal enum family shape. This avoids variance and
  substitution rules in the ancestor tables.
- A constructor expression has the arm type, not the family type.
  The subtype relation widens it wherever a family value is
  expected. An arm-typed scrutinee is exhausted by its own arm, and
  a sibling-arm pattern on it is an `E1041` error.
- Generic inference binds a covariant parameter from the first
  argument and joins later arguments only when they stay compatible;
  unrelated later arguments are `E1004` errors, not silent
  widenings. Explicit arguments select a common supertype.
- Effect arguments have no explicit syntax. They are inferred from
  function-typed arguments; an unconstrained effect variable becomes
  the empty row, which is the principal solution.
- Row inference through function-typed parameters binds a declared
  row only when it is exactly one effect variable. Mixed rows in an
  inferred position must match exactly.
- Rows name operations and groups by text (`Io.Print`, `Io`).
  Unknown names are accepted as operation names when they start with
  an upper-case letter; the ABI manifest arrives in week 4.
- Tuple equality with `==` is rejected (`E1017`); the specification
  does not define it for reference-versus-structural purposes yet.
- Ordinary class constructor patterns are rejected (`E1041`); only
  enum arms destructure. Case bodies over non-enum scrutinees need a
  binding or `_` arm.
- Explicit type arguments attach to named calls only; a closure
  value takes none. `List[T]()` and `Map[K, V]()` construct empty
  collections when the arguments are explicit.
- Pattern analysis runs under a fixed work budget (one million
  matrix steps). A case past the budget is an `E1049` error instead
  of an unbounded search.
- The per-module copy of the core sits after the user definitions,
  so its function and class indices shift with the user module. The
  pinned identity applies to the standalone core image; real linking
  with definition hashes arrives in week 5.
- The parser resolves `name[...](...)` with a bounded backtrack: it
  tries a type-argument list and falls back to indexing. A `with`
  row after a function-type result binds to the innermost function
  type.
- The depth-guard tests now run on 8 MiB stacks. The week-2 note
  already required a standard 8 MiB main-thread stack for full
  nesting; the larger week-3 AST pushed the guarded worst case past
  the old 4 MiB test threads.

## Review fixes

An independent review confirmed three defects. All three are fixed.

- A field default with checker temporaries (a `case` expression)
  compiled to bytecode with a local slot past `local_count`, and the
  verifier rejected the module. The scratch counter now advances to
  `base + max_slot`, because the shift records the highest slot in
  the pre-shift space. Run tests now cover defaults with `case`.
- The verifier read a `CallVirtualG` type-application index before
  any range check, so a crafted module was a host panic instead of a
  rejection. The structural pass now bounds the index and the
  variable scopes; the arity check stays in the dataflow pass.
- The verifier compared only the classes in `IsType` and `CastType`,
  so a crafted module held a cast from one generic instantiation to
  another. The verifier now requires equal argument vectors; every
  legal nominal relation in this slice keeps the argument vector.
  Corruption tests now attack both verifier rules.

The review also recorded three accepted observations. A `case` has
no runtime no-arm backstop; the static exhaustiveness proof is the
single guard, and a hardening fault is planned with the week 4
format work. Sibling-arm injection was top level only, so a nested
arm-typed scrutinee reported a spurious `E1042`; the post-week-4 fix
set made the injection recursive (docs/notes/fixes-post-week4.md).
An explicit row on a closure argument does not bind an effect
variable; performs arrive in week 4 and the binding rule lands
there.

## Tuple equality

The user decided structural tuple equality, and specification
section 6.4 now defines it. The implementation landed with the
review fixes. Equal static tuple types are required. Elements
compare under the rules for their declared element types: scalars
and strings by value, heap references by identity, nested tuples
structurally, and unit elements always equal. A type-variable
element has no rule inside a shared generic body and is an `E1017`
error. The oracle implements the same rule, so the differential
suite covers it.

## Changed tests

Existing expectations changed only where a construct moved from
rejected to supported:

- `lm-source` parser tests: tuples now parse
  (`rejects_tuple_literal` became `parses_tuple_literals`), and the
  reserved-keyword rejection uses `loop` instead of `enum`.
- `lm-testkit/tests/checker.rs`: `enum Color ... end` moved from
  `E1002` (reserved word) to `E1040` (an enum needs one arm); the
  tuple-literal rejection was removed; the reserved-word case now
  uses `loop`.
- `lm-testkit/tests/robustness.rs`: the guard-test threads grew from
  4 MiB to 8 MiB, as described above. No expectation changed.
- All other pre-week-3 tests pass unchanged, including every
  diagnostic text.

## Deferred work

- Host ABI generation from pinned core hashes: there is no host ABI
  or operation manifest until week 4, so nothing consumes the pin
  except the determinism gate.
- The VM and proc event enums from the core-image list (`RunResult`,
  `StepEvent`, `DriveEvent`, `Recv`, `ProcResult`, portable error
  enums, request tokens): no VM surface exists to type against until
  week 4, so the core image ships the five data files only.
- `loop ... end` stays reserved for week 4.
- CI workflow files, Miri (still no `unsafe`), `cargo-fuzz` targets,
  and committed benchmark distributions remain deferred as in the
  week-1 and week-2 notes.
