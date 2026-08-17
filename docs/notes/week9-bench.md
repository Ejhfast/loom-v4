# Week 9 Benchmarks

This note records the benchmark numbers after the runtime performance
work merged. It states the current values only. `docs/notes/benchmarks.md`
holds the earlier run, the full method, and the analysis of each shape.

## Host and build

- AMD Ryzen 9 9950X, 16 cores.
- `rustc` 1.91.1, release profile.
- CPython 3.13.12.

## How to run

```sh
nix-shell --run "cargo test --release -p lm-testkit --test bench \
  -- --ignored --nocapture --test-threads=1"
nix-shell --run "python3 benchmarks/ops.py"
```

Every case is `#[ignore]`, so the ordinary suite never pays for them.

## Method

Each language case compiles and loads once outside the timed region,
then times the run alone. The cost subtracts an empty-program
baseline, so machine construction stays out. Each case runs one
warm-up round, then nine measured rounds, and reports the median.
A workload returns a value the program consumes, so no work is dead.

The language table below reports the median of nine such runs, not
one. The cases that allocate on every iteration vary by up to 10
percent between runs, so one run cannot separate them.

CPython is a frame of reference, not a target. Both systems execute
bytecode with a dispatch loop, so the ratio is meaningful. The
absolute numbers belong to this host and this build.

Four cases have no exact CPython analogue, and the table marks them
`loose`:

- `generic_call` runs a plain CPython function, because CPython
  erases type arguments.
- `option_case` compares a real `Option` allocation against a `None`
  check that allocates nothing.
- `string_builder` compares the builder against list-append plus
  `join`, which is a different algorithm.
- `enum_case` compares a Loom enum and `case` against a CPython
  `match` over two tagged classes.

## Language operations

Nanoseconds per operation, sorted by ratio.

| Case | Loom | CPython | Ratio |
| --- | ---: | ---: | ---: |
| `recursion` | 28.6 | 45.2 | **0.63** |
| `arith_mix` | 46.8 | 53.3 | **0.88** |
| `list_index` | 43.5 | 38.7 | 1.12 |
| `int_loop` | 32.2 | 27.9 | 1.15 |
| `direct_call` | 43.5 | 36.1 | 1.20 |
| `enum_case` | 173.7 | 140.5 | 1.24 (loose) |
| `generic_call` | 49.2 | 39.0 | 1.26 (loose) |
| `closure_call` | 92.1 | 70.0 | 1.32 |
| `closure_capture` | 112.3 | 82.4 | 1.36 |
| `map_lookup` | 65.7 | 47.5 | 1.38 |
| `virtual_call` | 58.2 | 40.8 | 1.43 |
| `inherit_call` | 58.4 | 40.7 | 1.43 |
| `field_rw` | 64.4 | 41.3 | 1.56 |
| `byte_buffer` | 36.9 | 23.6 | 1.56 |
| `list_push` | 39.7 | 23.2 | 1.71 |
| `branch` | 51.5 | 29.9 | 1.72 |
| `string_builder` | 40.4 | 21.7 | 1.86 (loose) |
| `map_str_lookup` | 66.3 | 34.1 | 1.94 |
| `class_init` | 178.4 | 78.1 | 2.28 |
| `string_interp` | 164.1 | 64.1 | 2.56 |
| `option_case` | 224.3 | 68.8 | 3.26 (loose) |
| `map_insert` | 112.4 | 31.8 | **3.53** |

The median ratio is 1.43 over twenty-two operations. The summed cost
of the twenty-two cases is 1.65 times the CPython total.

Two results sit below 1.0.

- **`recursion`, 0.63.** Loom beats CPython on a thousand-deep call
  chain. The driver loop keeps an explicit activation stack and never
  recurses on the Rust stack, so depth costs a push, not a frame.
- **`arith_mix`, 0.88.** Multiply, divide, and modulo on machine
  integers, against the arbitrary-precision integers of CPython.

Three sit above 2.0.

- **`map_insert`, 3.53.** The largest gap in the suite. `map_lookup`
  sits at 1.38 on the same table, so table growth carries the
  difference, not the hash path.
- **`string_interp`, 2.56.** Each iteration formats one integer and
  allocates one fresh string.
- **`class_init`, 2.28.** Construction runs the generated `<new>`
  function, which then calls `init`, so it pays two calls plus the
  allocation. CPython runs one `__init__`.

## The world loop

The cases above build a bare `Vm`. Every tool builds a `World`, which
adds the aggregate ledgers and the activation loop. These two cases
run a workload from the table above inside a `World` with no proc.

| Case | Loom | Bare `Vm` case | World cost |
| --- | ---: | ---: | ---: |
| `world_int_loop` | 33.6 | 32.2 | +1.4 ns |
| `world_class_init` | 180.3 | 178.4 | +1.9 ns |

The `World` now costs about 1.4 ns per instruction on a loop that
allocates nothing, and about 2 ns per iteration on a loop that
allocates. A program with one machine keeps local heap counters, and
the world attaches the shared ledger only before it builds a second
machine.

## Procs

One proc receives twenty thousand messages from the root machine,
sums them, and returns the total. The timed region covers the world
construction, the spawn, every send, the close, and the join.

| Case | Messages | ns per message | Total ms |
| --- | ---: | ---: | ---: |
| `proc_send_receive` | 20,000 | 299.6 | 5.99 |

The cost covers one boundary copy, one mailbox append, one scheduler
event, and the activation of two machines.

## The type checker

The timed region covers the parse, the check, and the lowering. Seven
generated shapes run, so one shape cannot hide a cost in another.

| Shape | n | Lines | ms | Lines per second |
| --- | ---: | ---: | ---: | ---: |
| `methods_and_chain` | 16 | 101 | 0.216 | 467,592 |
| `methods_and_chain` | 64 | 389 | 0.427 | 911,007 |
| `methods_and_chain` | 256 | 1,541 | 1.377 | 1,119,099 |
| `methods_and_chain` | 1024 | 6,149 | 5.917 | 1,039,209 |
| `classes` | 16 | 162 | 0.240 | 675,000 |
| `classes` | 64 | 642 | 0.526 | 1,220,532 |
| `classes` | 256 | 2,562 | 1.670 | 1,534,131 |
| `inherit_chain` | 16 | 38 | 0.159 | 238,993 |
| `inherit_chain` | 64 | 134 | 0.233 | 575,107 |
| `inherit_chain` | 256 | 518 | 0.574 | 902,439 |
| `generics` | 16 | 82 | 0.216 | 379,629 |
| `generics` | 64 | 322 | 0.444 | 725,225 |
| `generics` | 256 | 1,282 | 1.331 | 963,185 |
| `inference_chain` | 16 | 20 | 0.152 | 131,578 |
| `inference_chain` | 64 | 68 | 0.209 | 325,358 |
| `inference_chain` | 256 | 260 | 0.439 | 592,255 |
| `enum_case_arms` | 16 | 40 | 0.185 | 216,216 |
| `enum_case_arms` | 64 | 136 | 0.339 | 401,179 |
| `enum_case_arms` | 256 | 520 | 1.054 | 493,358 |
| `wide_body` | 64 | 69 | 0.173 | 398,843 |
| `wide_body` | 256 | 261 | 0.283 | 922,261 |
| `wide_body` | 1024 | 1,029 | 0.730 | 1,409,589 |

The twenty-two cases total 16.89 ms.

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
- `wide_body` — one function of `n` statements, against `n`
  functions.

No shape is superlinear over this range. The rate rises with size in
every shape, which is the fixed cost of the core prelude amortizing.
`inference_chain` costs the most per line, which is the cost of a
generic instantiation at every step. `wide_body` costs the least, so
one large function costs less than many small ones for the same line
count.

The runtime performance work did not touch this path. The type
checker never reads the closed-type tables of the runtime.

## Artifact verification

`lm_verify::verify_module` on a decoded module.

| Case | Bytes | Functions | ms | MiB per second |
| --- | ---: | ---: | ---: | ---: |
| `tiny` | 14,384 | 71 | 0.0442 | 310.4 |
| `methods_and_chain_16` | 18,457 | 103 | 0.0540 | 326.0 |
| `methods_and_chain_1024` | 277,153 | 2,119 | 0.6490 | 407.3 |
| `classes_16` | 20,977 | 118 | 0.0598 | 334.5 |
| `classes_256` | 123,817 | 838 | 0.2624 | 450.0 |
| `inherit_chain_16` | 17,202 | 87 | 0.0512 | 320.4 |
| `inherit_chain_256` | 60,702 | 327 | 0.1319 | 438.9 |
| `generics_16` | 16,988 | 86 | 0.0564 | 287.3 |
| `generics_256` | 57,776 | 326 | 0.2330 | 236.5 |
| `inference_chain_16` | 14,899 | 71 | 0.0485 | 293.0 |
| `inference_chain_256` | 22,819 | 71 | 0.1017 | 214.0 |
| `enum_case_arms_16` | 19,032 | 88 | 0.0596 | 304.5 |
| `enum_case_arms_256` | 87,972 | 328 | 0.3398 | 246.9 |
| `wide_body_64` | 15,660 | 71 | 0.0471 | 317.1 |
| `wide_body_1024` | 34,860 | 71 | 0.0915 | 363.3 |

Every artifact carries the embedded core image, so the smallest
program is 14.0 KiB and 71 functions.

`enum_case_arms_256` and `inference_chain_256` stay the two slowest
shapes per byte. `docs/notes/benchmarks.md` records the scaling probe
and the reason: both passes grow faster than the arm count, and the
verifier grows faster than the checker. Verification reads untrusted
bytes, so this is a performance characteristic to bound, not a
soundness defect.

The whole load path, which is the decode, the identity preflight, the
verifier, and the dispatch rows:

| Case | Bytes | ms |
| --- | ---: | ---: |
| `load_tiny` | 14,384 | 0.0680 |
| `load_generated_256` | 79,633 | 0.3207 |

The runtime performance work did not change verification or load.
