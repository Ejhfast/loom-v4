# Benchmarks

This note records the benchmark suite, the method, and the numbers of
one run. The suite lives in `crates/lm-testkit/tests/bench.rs` and
`benchmarks/ops.py`.

## How to run

```sh
nix-shell --run "cargo test --release -p lm-testkit --test bench \
  -- --ignored --nocapture --test-threads=1"
nix-shell --run "python3 benchmarks/ops.py"
```

Every case is `#[ignore]`, so the ordinary suite never pays for them.

## Method

- Each language case compiles and loads once outside the timed
  region, then times the run alone.
- The reported cost subtracts an empty-program baseline, so machine
  construction stays out. The baseline measures 90 ns, so the
  subtraction changes no conclusion.
- Each case runs one warm-up round, then nine measured rounds, and
  reports the median.
- A workload returns a value the program consumes, so no work is dead.
- CPython runs the same workload with the same shape: the same loop
  form, the same counts, and a median of nine rounds.

CPython is a frame of reference, not a target. It says whether a
number is reasonable for an interpreter of this kind. Both systems
execute bytecode with a dispatch loop, so the ratio is meaningful; the
absolute numbers belong to this host and this build.

Three cases have no exact CPython analogue, and the table marks them.
`generic_call` runs a plain CPython function, because CPython erases
type arguments. `option_case` compares a real `Option` allocation
against a `None` check that allocates nothing. `string_builder`
compares the builder against list-append plus `join`, which is a
different algorithm. Read those three ratios as indicative.

## Language operations

Release build. CPython 3.13.12. Nanoseconds per operation, sorted by
ratio.

| Case | Loom | CPython | Ratio |
| --- | ---: | ---: | ---: |
| `recursion` | 31.2 | 44.1 | **0.71** |
| `arith_mix` | 44.1 | 51.3 | **0.86** |
| `enum_case` | 142.9 | 143.2 | 1.00 |
| `closure_call` | 74.1 | 70.8 | 1.05 |
| `list_index` | 41.1 | 37.4 | 1.10 |
| `closure_capture` | 92.9 | 83.8 | 1.11 |
| `int_loop` | 31.4 | 27.1 | 1.16 |
| `direct_call` | 42.9 | 36.4 | 1.18 |
| `generic_call` | 53.6 | 38.0 | 1.41 (loose) |
| `virtual_call` | 58.2 | 40.8 | 1.43 |
| `inherit_call` | 59.2 | 40.7 | 1.45 |
| `byte_buffer` | 36.4 | 23.4 | 1.56 |
| `field_rw` | 63.5 | 40.1 | 1.58 |
| `list_push` | 37.8 | 23.0 | 1.64 |
| `map_lookup` | 76.3 | 46.5 | 1.64 |
| `branch` | 52.8 | 29.3 | 1.80 |
| `string_interp` | 122.0 | 65.2 | 1.87 |
| `class_init` | 150.3 | 76.8 | 1.96 |
| `string_builder` | 46.1 | 21.0 | 2.20 (loose) |
| `map_str_lookup` | 78.9 | 33.2 | 2.38 |
| `option_case` | 191.9 | 69.1 | 2.78 (loose) |
| `map_insert` | 130.3 | 31.9 | **4.08** |

The median ratio is 1.45 over twenty-two operations.

Two results sit below 1.0.

- **`recursion`, 0.71.** Loom is faster than CPython on a
  thousand-deep call chain. The driver loop keeps an explicit
  activation stack and never recurses on the Rust stack, so depth
  costs a push instead of a frame.
- **`arith_mix`, 0.86.** Multiply, divide, and modulo on machine
  integers, against CPython's arbitrary-precision integers.

Three sit above 2.0 for reasons worth naming.

- **`map_insert`, 4.08.** The largest gap in the suite. A probe splits
  the cost: `put` on a growing table costs 137 ns, and `put` that
  overwrites an existing key costs 73 ns. The steady-state put is
  close to the lookup at 76 ns, so **table growth carries the whole
  difference**. The hash path is not the target; the resize is.
- **`map_str_lookup`, 2.38** against `map_lookup` at 1.64. String
  keys cost about as much again as integer keys.
- **`class_init`, 1.96.** Construction runs the generated `<new>`
  function, which then calls `init`, so it pays two calls plus the
  allocation. CPython runs one `__init__`.

## The type checker

The timed region covers the parse, the check, and the lowering. Seven
generated shapes, so one shape cannot hide a cost in another.

| Shape | n | Lines | ms | Lines per second |
| --- | ---: | ---: | ---: | ---: |
| `methods_and_chain` | 16 | 101 | 0.177 | 569,457 |
| `methods_and_chain` | 64 | 389 | 0.389 | 1,001,248 |
| `methods_and_chain` | 256 | 1,541 | 1.351 | 1,140,453 |
| `methods_and_chain` | 1024 | 6,149 | 6.050 | 1,016,352 |
| `classes` | 16 | 162 | 0.203 | 798,415 |
| `classes` | 64 | 642 | 0.477 | 1,345,557 |
| `classes` | 256 | 2,562 | 1.654 | 1,549,186 |
| `inherit_chain` | 16 | 38 | 0.115 | 329,113 |
| `inherit_chain` | 64 | 134 | 0.176 | 761,355 |
| `inherit_chain` | 256 | 518 | 0.413 | 1,254,891 |
| `generics` | 16 | 82 | 0.176 | 466,222 |
| `generics` | 64 | 322 | 0.410 | 784,744 |
| `generics` | 256 | 1,282 | 1.316 | 973,995 |
| `inference_chain` | 16 | 20 | 0.112 | 178,712 |
| `inference_chain` | 64 | 68 | 0.174 | 390,980 |
| `inference_chain` | 256 | 260 | 0.406 | 641,064 |
| `enum_case_arms` | 16 | 40 | 0.146 | 274,100 |
| `enum_case_arms` | 64 | 136 | 0.290 | 468,168 |
| `enum_case_arms` | 256 | 520 | 1.009 | 515,171 |
| `wide_body` | 64 | 69 | 0.132 | 524,153 |
| `wide_body` | 256 | 261 | 0.244 | 1,067,948 |
| `wide_body` | 1024 | 1,029 | 0.694 | 1,483,136 |

The shapes:

- `methods_and_chain` — `n` methods on one class plus `n` functions
  that call their predecessor.
- `classes` — `n` independent classes, two fields and two methods
  each.
- `inherit_chain` — one inheritance chain `n` deep, so a method
  resolves through every level.
- `generics` — `n` generic functions, each instantiated at two types.
- `inference_chain` — `n` assignments whose types flow through a
  generic call, so each step infers from the step before.
- `enum_case_arms` — one enum of `n` arms and one `case` over all of
  them.
- `wide_body` — one function of `n` statements, against `n` functions.

No shape is superlinear over this range. The rate rises with size in
every shape, which is the fixed cost of the core prelude amortizing.
`inference_chain` is the most expensive per line, which is the cost of
a generic instantiation at every step. `wide_body` is the cheapest, so
one large function costs less than many small ones for the same line
count.

## Artifact verification

`lm_verify::verify_module` on a decoded module.

| Case | Bytes | Functions | ms | MiB per second |
| --- | ---: | ---: | ---: | ---: |
| `tiny` | 8,848 | 45 | 0.0208 | 406.5 |
| `methods_and_chain_16` | 12,917 | 77 | 0.0259 | 476.5 |
| `methods_and_chain_1024` | 271,613 | 2,093 | 0.3372 | 768.1 |
| `classes_16` | 15,377 | 92 | 0.0280 | 524.7 |
| `classes_256` | 117,257 | 812 | 0.1417 | 789.2 |
| `inherit_chain_16` | 11,602 | 61 | 0.0239 | 463.7 |
| `inherit_chain_256` | 54,142 | 301 | 0.0765 | 674.7 |
| `generics_16` | 11,452 | 60 | 0.0238 | 459.1 |
| `generics_256` | 52,240 | 300 | 0.0788 | 632.5 |
| `inference_chain_16` | 9,363 | 45 | 0.0214 | 417.3 |
| `inference_chain_256` | 17,283 | 45 | 0.0360 | 457.6 |
| `enum_case_arms_16` | 13,428 | 62 | 0.0293 | 437.5 |
| `enum_case_arms_256` | 81,408 | 302 | 0.2399 | 323.7 |
| `wide_body_64` | 10,124 | 45 | 0.0217 | 444.1 |
| `wide_body_1024` | 29,324 | 45 | 0.0453 | 617.3 |

Every artifact carries the embedded core image, so the smallest
program is 8.8 KiB and 45 functions. Most shapes reach 630 to 790
MiB/s once the module grows past that fixed core.

**One shape does not: `enum_case_arms_256` verifies at 323.7 MiB/s,
against 789.2 for a larger `classes_256`.** A scaling probe shows the
verifier is close to quadratic in the arm count:

| Arms | Bytes | Check ms | Verify ms |
| ---: | ---: | ---: | ---: |
| 16 | 13,428 | 0.149 | 0.030 |
| 32 | 17,908 | 0.190 | 0.040 |
| 64 | 26,868 | 0.281 | 0.064 |
| 128 | 44,928 | 0.469 | 0.112 |
| 256 | 81,408 | 0.971 | 0.237 |
| 512 | 154,368 | 2.383 | 0.911 |

From 256 arms to 512, the byte count grows 1.9 times and the
verification time grows 3.8 times. The checker grows 2.5 times over
the same step, so both passes are superlinear in the arm count and the
verifier is the steeper one. Verification runs on untrusted bytes, so
this is the same class of exposure as the refinement budget: a
performance characteristic to bound, not a soundness defect.

The whole load path, which is the decode, the identity preflight, the
verifier, and the dispatch rows:

| Case | Bytes | ms |
| --- | ---: | ---: |
| `load_tiny` | 8,848 | 0.0367 |
| `load_generated_256` | 74,093 | 0.2138 |

## What this run does not cover

- One host and one build. There is no committed distribution and no
  regression gate on these numbers.
- No allocation or memory measurements.
- No effect-row or machine-control workload: `perform`, `drive`, and
  nested machines are unmeasured.
- No package-scale build timing beyond the numbers in `week6.md`.
- The `String` method surface of specification 24.6 is unimplemented,
  so string work is interpolation and the builder alone. `Bytes` does
  not exist as a type; `ByteBuffer` is the only byte surface.
