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
