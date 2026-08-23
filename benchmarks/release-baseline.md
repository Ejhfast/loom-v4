# Release benchmark baseline

This file records the baseline for the conditional collection work.

The source revision is `b1e6d8e`.

The measurements use a release build unless the table states otherwise.

## Core image

| Measurement | Baseline |
| --- | ---: |
| Classes | 187 |
| Functions | 565 |
| Artifact size | 131,407 bytes |
| Core compilation | 2.204 ms |
| Core loading | 0.982 ms |

## Language operations

| Operation | Baseline |
| --- | ---: |
| `int_loop` | 33.5 ns |
| `direct_call` | 30.9 ns |
| `list_push` | 41.7 ns |
| `list_index` | 43.8 ns |
| `map_insert` | 121.9 ns |
| `map_lookup` | 68.3 ns |
| `string_interp` | 189.4 ns |
| `partial_eq` | 95.6 ns |
| `map_str_lookup` | 59.0 ns |
| `map_bytes_lookup` | 53.9 ns |
| `map_hashable_lookup` | 207.6 ns |

## Workspace suite

The warm debug workspace suite completed in 35.955 seconds.

The command used the existing test worker count.

No later measurement can add workers to hide a regression.

## Native tuples and conditional conformances

These measurements include `Tuple2` through `Tuple16` and conditional conformances.

| Measurement | Result | Change |
| --- | ---: | ---: |
| Classes | 202 | +8.0% |
| Functions | 579 | +2.5% |
| Artifact size | 137,303 bytes | +4.5% |
| Core compilation | 2.258 ms | +2.5% |
| Core loading | 1.056 ms | +7.5% |
| Workspace suite | 38.088 seconds | +5.9% |

This stage adds ten conditional conformance integration tests.

## Completed collection protocols

These measurements include all accepted collection and protocol work.

| Measurement | Result | Change from baseline |
| --- | ---: | ---: |
| Classes | 202 | +8.0% |
| HIR functions | 477 | — |
| Bytecode functions | 679 | +20.2% |
| Artifact size | 218,077 bytes | +66.0% |
| Core checking | 1.778 ms | — |
| Core lowering | 0.792 ms | — |
| Core compilation | 2.832 ms | +28.5% |
| Core decoding | 0.326 ms | — |
| Core verification | 1.120 ms | — |
| Structural verification | 0.400 ms | — |
| Verification hash | 0.111 ms | — |
| Semantic identity | 1.920 ms | — |
| Decoded loading | 1.254 ms | — |
| Core loading | 1.577 ms | +60.6% |
| Admission sweep | 1.990 seconds | — |
| Workspace suite | 32.790 seconds | -8.8% |

The first completed feature build compiled core in about 4.309 ms.

The final compiler pass reduced this result by 34.3%.

The checker reuses type graph properties during one independent pass.

It also removes repeated signature copies and uses indexed method lookup.

Identity generation computes each interface digest once during one module pass.

Verifier indexes remove repeated conformance and constructor table scans.

All 256-bit content identities now use BLAKE3-256.

The identity version increments prevent mixed hash domains.

The admission sweep fell from 33.37 seconds to 1.99 seconds.

No new cross-invocation cache contributes to these results.

Admission keeps its pre-existing aggregate cache behavior.

The final suite keeps the existing worker count and capture coverage.

## Completed operation measurements

| Operation | Result | Baseline |
| --- | ---: | ---: |
| `int_loop` | 31.7 ns | 33.5 ns |
| `direct_call` | 31.0 ns | 30.9 ns |
| `list_push` | 43.1 ns | 41.7 ns |
| `list_index` | 44.0 ns | 43.8 ns |
| `map_insert` | 107.3 ns | 121.9 ns |
| `map_lookup` | 67.9 ns | 68.3 ns |
| `string_interp` | 192.5 ns | 189.4 ns |
| `partial_eq` | 93.5 ns | 95.6 ns |
| `map_str_lookup` | 57.3 ns | 59.0 ns |
| `map_bytes_lookup` | 52.6 ns | 53.9 ns |
| `map_hashable_lookup` | 207.4 ns | 207.6 ns |

The dense native map path skips tombstone checks when the map has no holes.

This change removed the measured string-key lookup regression.

## New operation measurements

| Operation | Result |
| --- | ---: |
| `map_remove_reinsert` | 166.3 ns |
| `list_eq` | 869.9 ns |
| `list_hash` | 795.3 ns |
| `tuple_hash` | 348.0 ns |
| `list_sort` | 19,254 ns |

## Numeric literals and bitwise operations

The exact parent revision is `0e92df8`.

The measurements use paired release runs from the parent and this branch.

| Core measurement | Parent | Result | Change |
| --- | ---: | ---: | ---: |
| Classes | 202 | 206 | +2.0% |
| HIR functions | 477 | 519 | +8.8% |
| Bytecode functions | 679 | 725 | +6.8% |
| Artifact size | 218,077 bytes | 225,082 bytes | +3.2% |
| Core checking | 1.745 ms | 1.822 ms | +4.4% |
| Core lowering | 0.806 ms | 0.831 ms | +3.1% |
| Core compilation | 2.920 ms | 2.886 ms | -1.2% |
| Core decoding | 0.335 ms | 0.333 ms | -0.6% |
| Core verification | 1.135 ms | 1.123 ms | -1.1% |
| Structural verification | 0.397 ms | 0.403 ms | +1.5% |
| Verification hash | 0.110 ms | 0.168 ms | +52.7% |
| Semantic identity | 1.939 ms | 1.970 ms | +1.6% |
| Decoded loading | 1.238 ms | 1.257 ms | +1.5% |
| Core loading | 1.571 ms | 1.603 ms | +2.0% |
| Warm workspace suite | 32.96 seconds | 33.90 seconds | +2.9% |

Total compilation and loading remain within ordinary measurement noise.

The suite comparison uses the existing worker count and identical warm commands.

The artifact adds Float, byte literals, and the new core methods.

| Operation | Parent | Result | Change |
| --- | ---: | ---: | ---: |
| `int_loop` | 31.8 ns | 31.2 ns | -1.9% |
| `direct_call` | 30.6 ns | 30.2 ns | -1.3% |
| `string_interp` | 261.3 ns | 265.8 ns | +1.7% |

The new operations have these costs.

| Operation | Loom | CPython 3.13.12 |
| --- | ---: | ---: |
| `int_bitwise` | 31.8 ns | 25.3 ns |
| `float_add` | 31.4 ns | 22.2 ns |
| `bytes_xor_32` | 109.2 ns | not available |

CPython does not define a bitwise XOR operator for bytes.

## Host-effects Stage 0

The source revision is `ce15844`.

These measurements precede every host-effects ABI change.

The manifest ABI version is 24.

The bytecode version is 51.

The snapshot format version is 29.

| Core measurement | Stage 0 |
| --- | ---: |
| Classes | 209 |
| HIR functions | 524 |
| Bytecode functions | 733 |
| Artifact size | 226,704 bytes |
| Core checking | 1.777 ms |
| Core lowering | 0.815 ms |
| Core compilation | 2.899 ms |
| Core decoding | 0.332 ms |
| Core verification | 1.111 ms |
| Structural verification | 0.400 ms |
| Verification hash | 0.112 ms |
| Semantic identity | 2.014 ms |
| Decoded loading | 1.253 ms |
| Core loading | 1.598 ms |

| Runtime measurement | Stage 0 |
| --- | ---: |
| `int_loop` | 32.3 ns |
| `direct_call` | 31.1 ns |
| Warm workspace suite | 35.34 seconds |

The suite used the existing worker count.

The release benchmarks used nine measured rounds after one warm-up round.

## Host-effects Stage 1

The parent revision is `c790b83`.

The manifest ABI version is 25.

The bytecode version is 52.

The snapshot format version remains 29.

| Core measurement | Stage 0 | Stage 1 |
| --- | ---: | ---: |
| Classes | 209 | 209 |
| HIR functions | 524 | 524 |
| Bytecode functions | 733 | 733 |
| Artifact size | 226,704 bytes | 226,704 bytes |
| Core checking | 1.777 ms | 1.797 ms |
| Core lowering | 0.815 ms | 0.815 ms |
| Core compilation | 2.899 ms | 2.902 ms |
| Core decoding | 0.332 ms | 0.334 ms |
| Core verification | 1.111 ms | 1.127 ms |
| Structural verification | 0.400 ms | 0.400 ms |
| Verification hash | 0.112 ms | 0.113 ms |
| Semantic identity | 2.014 ms | 1.986 ms |
| Decoded loading | 1.253 ms | 1.269 ms |
| Core loading | 1.598 ms | 1.613 ms |

| Runtime measurement | Stage 0 | Stage 1 | Change |
| --- | ---: | ---: | ---: |
| `int_loop` | 32.3 ns | 32.6 ns | +0.9% |
| `direct_call` | 31.1 ns | 31.1 ns | 0.0% |
| `direct_clock` | 102.9 ns | 105.4 ns | +2.4% |
| Warm workspace suite | 35.34 seconds | 35.27 seconds | -0.2% |

The prepared outcome keeps its argument values on the machine stack.

This form prevents new drop code in the hot interpreter loop.

The decoded instruction remains 16 bytes.

The result keeps static execution within normal measurement noise.

## Host-effects Stage 2

The parent revision is `15eceec`.

The manifest ABI version is 26.

The bytecode version is 53.

The snapshot format version is 30.

| Core measurement | Stage 1 | Stage 2 | Change |
| --- | ---: | ---: | ---: |
| Classes | 209 | 235 | +12.4% |
| HIR functions | 524 | 535 | +2.1% |
| Bytecode functions | 733 | 770 | +5.0% |
| Artifact size | 226,704 bytes | 235,172 bytes | +3.7% |
| Core checking | 1.797 ms | 1.859 ms | +3.5% |
| Core lowering | 0.815 ms | 0.839 ms | +2.9% |
| Core compilation | 2.902 ms | 3.000 ms | +3.4% |
| Core decoding | 0.334 ms | 0.343 ms | +2.7% |
| Core verification | 1.127 ms | 1.145 ms | +1.6% |
| Structural verification | 0.400 ms | 0.411 ms | +2.8% |
| Verification hash | 0.113 ms | 0.116 ms | +2.7% |
| Semantic identity | 1.986 ms | 2.092 ms | +5.3% |
| Decoded loading | 1.269 ms | 1.291 ms | +1.7% |
| Core loading | 1.613 ms | 1.645 ms | +2.0% |

| Runtime measurement | Stage 1 | Stage 2 | Change |
| --- | ---: | ---: | ---: |
| `int_loop` | 32.6 ns | 31.6 ns | -3.1% |
| `direct_call` | 31.1 ns | 31.9 ns | +2.6% |
| `direct_clock` | 105.4 ns | 104.9 ns | -0.5% |
| Warm workspace suite | 35.27 seconds | 37.60 seconds | +6.6% |

The suite result repeated at 37.76 and 37.60 seconds.

Stage 2 adds 16 integration tests and two examples to the admission corpus.

The larger core adds typed terminal and signal values to every program.

Focused runtime remains within normal measurement noise.

## Host-effects Stage 3

The parent revision is `4061438`.

The manifest ABI version is 27.

The bytecode version is 54.

The snapshot format version remains 30.

| Core measurement | Stage 2 | Stage 3 | Change |
| --- | ---: | ---: | ---: |
| Classes | 235 | 258 | +9.8% |
| HIR functions | 535 | 537 | +0.4% |
| Bytecode functions | 770 | 795 | +3.2% |
| Artifact size | 235,172 bytes | 242,074 bytes | +2.9% |
| Core checking | 1.859 ms | 1.903 ms | +2.4% |
| Core lowering | 0.839 ms | 0.870 ms | +3.7% |
| Core compilation | 3.000 ms | 3.117 ms | +3.9% |
| Core decoding | 0.343 ms | 0.367 ms | +7.0% |
| Core verification | 1.145 ms | 1.210 ms | +5.7% |
| Structural verification | 0.411 ms | 0.430 ms | +4.6% |
| Verification hash | 0.116 ms | 0.126 ms | +8.6% |
| Semantic identity | 2.092 ms | 2.253 ms | +7.7% |
| Decoded loading | 1.291 ms | 1.379 ms | +6.8% |
| Core loading | 1.645 ms | 1.745 ms | +6.1% |

| Runtime measurement | Stage 2 | Stage 3 | Change |
| --- | ---: | ---: | ---: |
| `int_loop` | 31.6 ns | 31.5 ns | -0.3% |
| `direct_call` | 31.9 ns | 30.4 ns | -4.7% |
| `direct_clock` | 104.9 ns | 106.6 ns | +1.6% |
| Warm workspace suite | 37.60 seconds | 38.61 seconds | +2.7% |

Static runtime remains within normal measurement noise.

The larger core adds portable file metadata and stable error cases.

## Host-effects Stage 4

The parent revision is `c4ae4cd`.

The manifest ABI version is 28.

The bytecode version remains 54.

The snapshot format version remains 30.

| Core measurement | Stage 3 | Stage 4 | Change |
| --- | ---: | ---: | ---: |
| Classes | 258 | 258 | 0.0% |
| HIR functions | 537 | 541 | +0.7% |
| Bytecode functions | 795 | 799 | +0.5% |
| Artifact size | 242,074 bytes | 245,959 bytes | +1.6% |
| Core checking | 1.903 ms | 1.925 ms | +1.2% |
| Core lowering | 0.870 ms | 0.869 ms | -0.1% |
| Core compilation | 3.117 ms | 3.122 ms | +0.2% |
| Core decoding | 0.367 ms | 0.367 ms | 0.0% |
| Core verification | 1.210 ms | 1.194 ms | -1.3% |
| Structural verification | 0.430 ms | 0.418 ms | -2.8% |
| Verification hash | 0.126 ms | 0.119 ms | -5.6% |
| Semantic identity | 2.253 ms | 2.191 ms | -2.8% |
| Decoded loading | 1.379 ms | 1.350 ms | -2.1% |
| Core loading | 1.745 ms | 1.720 ms | -1.4% |

| Runtime measurement | Stage 3 | Stage 4 | Change |
| --- | ---: | ---: | ---: |
| `int_loop` | 31.5 ns | 31.3 ns | -0.6% |
| `direct_call` | 30.4 ns | 30.5 ns | +0.3% |
| `direct_clock` | 106.6 ns | 105.5 ns | -1.0% |
| Warm workspace suite | 38.61 seconds | 38.97 seconds | +0.9% |

Static runtime remains within normal measurement noise.

Stage 4 adds four core console helpers.

The ABI now contains only three byte console operations.

## Host-effects Stage 5

The parent revision is `35f7877`.

The manifest ABI version is 29.

The bytecode version remains 54.

The snapshot format version remains 30.

| Core measurement | Stage 4 | Stage 5 | Change |
| --- | ---: | ---: | ---: |
| Classes | 258 | 294 | +14.0% |
| HIR functions | 541 | 556 | +2.8% |
| Bytecode functions | 799 | 850 | +6.4% |
| Artifact size | 245,959 bytes | 258,307 bytes | +5.0% |
| Core checking | 1.925 ms | 2.012 ms | +4.5% |
| Core lowering | 0.869 ms | 0.923 ms | +6.2% |
| Core compilation | 3.122 ms | 3.285 ms | +5.2% |
| Core decoding | 0.367 ms | 0.383 ms | +4.4% |
| Core verification | 1.194 ms | 1.248 ms | +4.5% |
| Structural verification | 0.418 ms | 0.447 ms | +6.9% |
| Verification hash | 0.119 ms | 0.187 ms | +57.1% |
| Semantic identity | 2.191 ms | 2.368 ms | +8.1% |
| Decoded loading | 1.350 ms | 1.466 ms | +8.6% |
| Core loading | 1.720 ms | 1.848 ms | +7.4% |

| Runtime measurement | Stage 4 | Stage 5 | Change |
| --- | ---: | ---: | ---: |
| `int_loop` | 31.3 ns | 33.7 ns | +7.7% |
| `direct_call` | 30.5 ns | 35.6 ns | +16.7% |
| `direct_clock` | 105.5 ns | 109.0 ns | +3.3% |
| Warm workspace suite | 38.97 seconds | 42.38 seconds | +8.8% |

Stage 5 adds typed pipe ends and operating-system child handles.

The stage adds eight integration tests and one checked example.

The final performance pass must repeat the static runtime measurements.

## Host-effects Stage 6

The parent revision is `61b3b9e`.

The manifest ABI version is 30.

The bytecode version remains 54.

The snapshot format version remains 30.

| Core measurement | Stage 5 | Stage 6 | Change |
| --- | ---: | ---: | ---: |
| Classes | 294 | 294 | 0.0% |
| HIR functions | 556 | 556 | 0.0% |
| Bytecode functions | 850 | 850 | 0.0% |
| Artifact size | 258,307 bytes | 258,307 bytes | 0.0% |
| Core checking | 2.012 ms | 2.001 ms | -0.5% |
| Core lowering | 0.923 ms | 0.907 ms | -1.7% |
| Core compilation | 3.285 ms | 3.249 ms | -1.1% |
| Core decoding | 0.383 ms | 0.382 ms | -0.3% |
| Core verification | 1.248 ms | 1.249 ms | +0.1% |
| Structural verification | 0.447 ms | 0.444 ms | -0.7% |
| Verification hash | 0.187 ms | 0.125 ms | -33.2% |
| Semantic identity | 2.368 ms | 2.343 ms | -1.1% |
| Decoded loading | 1.466 ms | 1.429 ms | -2.5% |
| Core loading | 1.848 ms | 1.817 ms | -1.7% |

| Runtime measurement | Stage 5 | Stage 6 | Change |
| --- | ---: | ---: | ---: |
| `int_loop` | 33.7 ns | 31.8 ns | -5.6% |
| `direct_call` | 35.6 ns | 32.0 ns | -10.1% |
| `direct_clock` | 109.0 ns | 108.1 ns | -0.8% |
| Warm workspace suite | 42.38 seconds | 42.38 seconds | 0.0% |

Stage 6 adds no core class or function.

The earlier Stage 5 runtime result did not repeat.

The final pass will compare the parent and branch in one session.

## Host-effects Stage 7

The parent revision is `59c49b9`.

The manifest ABI version is 31.

The bytecode version remains 54.

The snapshot format version remains 30.

| Core measurement | Parent | Stage 7 | Change |
| --- | ---: | ---: | ---: |
| Classes | 294 | 297 | +1.0% |
| HIR functions | 556 | 563 | +1.3% |
| Bytecode functions | 850 | 860 | +1.2% |
| Artifact size | 258,307 bytes | 260,082 bytes | +0.7% |
| Core checking | 2.062 ms | 1.977 ms | -4.1% |
| Core lowering | 0.907 ms | 0.910 ms | +0.3% |
| Core compilation | 3.226 ms | 3.237 ms | +0.3% |
| Core decoding | 0.380 ms | 0.387 ms | +1.8% |
| Core verification | 1.230 ms | 1.226 ms | -0.3% |
| Structural verification | 0.437 ms | 0.440 ms | +0.7% |
| Verification hash | 0.125 ms | 0.125 ms | 0.0% |
| Semantic identity | 2.324 ms | 2.339 ms | +0.6% |
| Decoded loading | 1.421 ms | 1.399 ms | -1.5% |
| Core loading | 1.798 ms | 1.793 ms | -0.3% |

| Runtime measurement | Parent | Stage 7 | Change |
| --- | ---: | ---: | ---: |
| `int_loop` | 33.6 ns | 31.3 ns | -6.8% |
| `direct_call` | 31.0 ns | 30.4 ns | -1.9% |
| `direct_clock` | 108.4 ns | 106.7 ns | -1.6% |

These paired runs used the same release mode and benchmark process shape.

Each reported runtime is the median of nine measured runs.
