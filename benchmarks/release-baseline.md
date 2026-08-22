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
| Core checking | 2.538 ms | — |
| Core lowering | 0.796 ms | — |
| Core compilation | 3.673 ms | +66.7% |
| Core loading | 1.979 ms | +101.5% |
| Workspace suite | 54.183 seconds | +50.7% |

The first completed feature build compiled core in about 4.309 ms.

The checker optimization pass reduced this result by 14.8%.

The pass caches type graph properties and removes repeated signature copies.

It also uses direct paths for monomorphic calls and indexed method lookup.

The full suite first took 74.192 seconds after these features landed.

Repeated portable-code verification dominated the admission sweep.

A bounded proof cache reduced that sweep from 33.62 seconds to 6.36 seconds.

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
