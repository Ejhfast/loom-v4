# Week 10 Text Benchmarks

This note records the text and byte cases against CPython. It states
the numbers and what they say about the shared-storage design.

`docs/notes/week9-bench.md` holds the language cases and the method.
The method here is the same: each Loom case compiles and loads once
outside the timed region, and each reported value is the median of
three runs of a case that itself reports a median of nine rounds.

## Host and build

- AMD Ryzen 9 9950X, release profile.
- CPython 3.13.12.

## How to run

```sh
nix-shell --run "cargo test --release -p lm-testkit --test bench \
  -- --ignored --nocapture --test-threads=1"
nix-shell --run "python3 benchmarks/ops.py"
```

## Text and byte operations

Nanoseconds per operation, sorted by ratio.

| Case | Loom | CPython | Ratio |
| --- | ---: | ---: | ---: |
| `bytes_decode_large` | 881.7 | 2140.3 | **0.41** |
| `text_compare` | 51.1 | 34.0 | 1.50 |
| `text_trim` | 80.9 | 44.1 | 1.83 |
| `text_split` | 76.9 | 15.9 | 4.84 |
| `text_each` | 66.2 | 17.5 | 3.78 |
| `bytes_decode` | 245.7 | 78.4 | 3.13 |
| `text_split_once` | 417.1 | 74.2 | 5.62 |

`text_split` varies between 62.1 and 77.0 across runs. Read its ratio
as a range from 3.9 to 4.8. The other cases hold within two percent.

## What the numbers say

**The shared representation pays, and the crossing point is
measurable.** `bytes_decode` and `bytes_decode_large` run one
workload at two sizes. At 512 bytes Loom takes 3.13 times the CPython
cost. At 64 KiB Loom takes 0.41 times the CPython cost, so Loom is
about 2.4 times faster.

Loom validates the bytes once and shares the allocation, so its cost
does not grow with the payload. CPython allocates and copies, so its
cost does. The crossing point sits between those two sizes. A file
read and a network frame both sit above it, and those are the paths
the design exists for.

**Allocation cost, not the text design, sets every other ratio.** The
cases that lose allocate one object for each operation:

- `text_split_once` allocates an `Option`, a tuple, and two
  `Substring` values. CPython `partition` allocates a tuple and two
  strings. Loom pays about 104 ns for each object, and CPython pays
  about 25 ns.
- `text_split` allocates one `Substring` for each piece plus one
  list. CPython allocates one string for each piece plus one list.
  The counts match, and the ratio is the per-object ratio.
- `bytes_decode` at 512 bytes allocates a `Result` and a `Substring`
  against one CPython string.

The cases that hold near parity allocate nothing for each operation.
`text_compare` reads two values and pushes a `Bool`. `text_trim`
allocates one `Substring` and nothing else.

This matches the language table: `option_case` at 3.26 and
`class_init` at 2.28 measure the same thing. The text surface did not
introduce the gap, and no change to the text surface closes it.

**The one text-level defect this exposed is fixed.** `split_once`
first composed `find_bytes` and `slice_bytes`, so it allocated an
`Option` and two `Result` values that no caller could observe. Naming
the raw intrinsics in the core bodies removed three allocations and
took the case from 1091.9 ns to 417.1 ns. `strip_prefix` and
`strip_suffix` had the same shape.

A checked wrapper is right at the public surface and wrong inside a
core method that already proved the bound.

## Next

Allocation is the lever. A per-object cost near the CPython figure
would move `text_split` and `text_split_once` to about 1.5, and it
would move `option_case` and `class_init` with them. That is one
piece of work with reach across the language, and it is not text
work.
