# Latest benchmark baseline

The measured source revision is `57a0120`.

The measurements use release builds unless this file states a different build.

The bytecode version is 55.

The verifier version is 36.

The snapshot format version is 30.

## Core image

| Measurement | Result |
| --- | ---: |
| Classes | 302 |
| HIR functions | 571 |
| Bytecode functions | 873 |
| Artifact size | 264,401 bytes |
| Core checking | 2.070 ms |
| Core lowering | 0.935 ms |
| Core compilation | 3.352 ms |
| Core decoding | 0.389 ms |
| Core verification | 1.281 ms |
| Structural verification | 0.462 ms |
| Verification hash | 0.131 ms |
| Semantic identity | 2.447 ms |
| Decoded loading | 1.449 ms |
| Core loading | 1.839 ms |

## Language operations

Each result is a nine-run median.

The runtime process used CPU 0.

| Operation | Result |
| --- | ---: |
| `int_loop` | 32.5 ns |
| `direct_call` | 31.7 ns |
| `string_interp` | 264.0 ns |
| `float_add` | 33.5 ns |
| `string_builder` | 43.7 ns |
| `byte_buffer` | 36.7 ns |
| `direct_clock` | 107.1 ns |

String measurements can vary with process layout.

## Workspace suite

The warm debug workspace suite completed in 43.42 seconds.

The suite used the existing worker count and full coverage.

The standard codec modules link only when source imports them.

## Scheduler foundation

These results use the deterministic scheduler.

| Measurement | Result |
| --- | ---: |
| Proc send and receive | 344.2 ns per message |
| File open and close | 7.127 us per lifecycle |
| Cached 1 KiB file read | 3.416 us per read |
| One 35 ms sleep | 1 park, 1 timeout wakeup |
| Sleep with a signal guardian | 1 park, 1 timeout wakeup |
| Pure-run allocation gate | fewer than 100 allocations |

The shared host queue removed the ten-millisecond polling interval.

The shared host queue also removed the mixed output and child-wait deadlock.

The owned execution lease passes one real cross-thread execution test.
