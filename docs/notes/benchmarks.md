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

## Language operations

Release build. CPython 3.13.12. Nanoseconds per operation.

| Case | Loom | CPython | Ratio |
| --- | ---: | ---: | ---: |
| `int_loop` | 31.3 | 26.8 | 1.17 |
| `direct_call` | 42.3 | 35.9 | 1.18 |
| `virtual_call` | 57.0 | 40.8 | 1.40 |
| `field_rw` | 64.6 | 39.7 | 1.63 |
| `closure_call` | 81.2 | 70.1 | 1.16 |
| `class_init` | 175.4 | 76.2 | 2.30 |
| `list_push` | 38.3 | 22.1 | 1.73 |
| `list_index` | 41.0 | 37.8 | 1.08 |
| `map_insert` | 146.2 | 30.7 | 4.76 |
| `map_lookup` | 76.0 | 47.8 | 1.59 |
| `string_interp` | 152.0 | 64.6 | 2.35 |

The median ratio is 1.59, and the range is 1.08 to 4.76.

Three cases sit above the band the rest occupy.

- **`map_insert`, 4.76.** A separate probe splits the cost: `put` on a
  growing table costs 137 ns, and `put` that overwrites an existing
  key costs 73 ns. The steady-state put is close to the lookup at
  76 ns, so growth carries the difference. Table growth is the target,
  not the hash path.
- **`string_interp`, 2.35.** Interpolation builds one short string per
  iteration. The `String` method surface of specification 24.6 is not
  implemented, so this is the one string workload available, and the
  case measures allocation and formatting together.
- **`class_init`, 2.30.** Construction runs a generated `<new>`
  function that calls `init`, so it pays two calls plus the
  allocation. CPython runs one `__init__` call.

## The type checker

The generated source is `n` methods on one class plus `n` chained
functions. The timed region covers the parse, the check, and the
lowering.

| Definitions | Lines | Median ms | Lines per second |
| ---: | ---: | ---: | ---: |
| 16 | 101 | 0.188 | 537,514 |
| 64 | 389 | 0.389 | 999,217 |
| 256 | 1,541 | 1.364 | 1,130,107 |
| 1,024 | 6,149 | 6.017 | 1,021,965 |

The rate is flat from 64 definitions upward, so the checker is linear
in the source over this range. About one million lines per second.

## Artifact verification

`lm_verify::verify_module` on a decoded module.

| Case | Bytes | Functions | Median ms | MiB per second |
| --- | ---: | ---: | ---: | ---: |
| `tiny` | 8,848 | 45 | 0.0218 | 387.8 |
| `class_small` | 9,120 | 46 | 0.0217 | 399.9 |
| `generated_64` | 24,965 | 173 | 0.0404 | 588.7 |
| `generated_256` | 74,093 | 557 | 0.1002 | 705.5 |
| `generated_1024` | 271,613 | 2,093 | 0.3507 | 738.7 |

Every artifact carries the embedded core image, so the smallest
program is 8.8 KiB and 45 functions. The rate climbs to about
740 MiB/s as the module grows past the fixed core.

The whole load path, which is the decode, the identity preflight, the
verifier, and the dispatch rows:

| Case | Bytes | Median ms |
| --- | ---: | ---: |
| `load_tiny` | 8,848 | 0.0378 |
| `load_generated_256` | 74,093 | 0.2275 |

Verification is 0.1002 ms of the 0.2275 ms load of
`generated_256`, so the decode, the preflight, and the dispatch rows
together cost about as much as the verifier.

## What this run does not cover

- One host and one build. There is no committed distribution and no
  regression gate on these numbers.
- No allocation or memory measurements.
- No effect-row or machine-control workload: `perform`, `drive`, and
  nested machines are unmeasured.
- No multi-module or package-scale build timing beyond the numbers in
  `week6.md`.
- The `String` method surface is unimplemented, so string work is
  represented by interpolation alone.
