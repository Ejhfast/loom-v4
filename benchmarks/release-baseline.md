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
