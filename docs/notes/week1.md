# Week 1 Status

This note records what landed in week 1, the known simplifications,
and the deferred work.

## Landed

- Cargo workspace with nine crates: `lm-source`, `lm-types`, `lm-hir`,
  `lm-bytecode`, `lm-verify`, `lm-value`, `lm-vm`, `lm-cli`, and
  `lm-testkit`.
- Scanner and parser for the narrow slice: `Int`, `Bool`, `String`,
  locals, arithmetic, comparison, `and`/`or`/`not`, `if`/`elsif`/`else`,
  `while` with `break`/`continue`, top-level `def` functions, direct
  calls, `return`, and a trailing entry expression.
- Deterministic diagnostics with stable codes and a printable AST.
- Interned types, bidirectional local checking, typed HIR, basic-block
  lowering, and a printable CFG.
- Compact serialized bytecode, a fixed-size decoded instruction form,
  and an independent verifier. The VM load path verifies every function
  before execution.
- A VM with explicit frames, one operand arena, instruction fuel, a
  non-collecting page arena with a hard heap cap, and terminal
  `Done`/`Fault` results. Guest calls never grow the Rust stack.
- `lm check`, `lm run --show-result`, and `lm disasm`.
- UI, run-pass, run-fault, gate, and corruption test suites.

## Simplifications inside the slice

- A function with the unit result type discards the value of its final
  expression. The strict alternative rejects a non-unit tail. The
  specification does not decide this case for statement bodies.
- An integer literal must fit in `0..=i64::MAX`. Write `i64::MIN`
  with arithmetic, for example `0 - 9223372036854775807 - 1`.
- `%` with a zero divisor faults with `DivideByZero`. The one
  overflowing remainder case faults with `IntegerOverflow`.
- String interpolation is rejected with `E0006`, as the plan permits
  for week 1. Escapes and `{{`/`}}` work.
- The bidirectional checker lives in `lm-hir` and uses the interned
  type store from `lm-types`. The build order lists checking under
  `lm-types`; the split keeps the crate graph acyclic.
- The entry is a top-level statement sequence, and the last expression
  is the program result. This matches the week-1 example programs.
- `lm check` prints nothing on success and one diagnostic on failure.

## Deferred work

- CI workflow files for Linux, macOS, and Windows.
- A Miri job for the low-level crates. The code has no `unsafe`, so
  the job is not urgent.
- `cargo-fuzz` targets for the scanner, the parser, the decoder, and
  the verifier. These need a nightly toolchain. The corruption suite
  covers hand-written cases now.
- Benchmarks for parse/check/emit time, dispatch rate, and call cost.
  The run suites act as the smoke check now.
- The `lm-abi` crate and the `xtask` generator. The slice has no host
  operations yet, so there is no manifest to generate.
