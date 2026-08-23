# Latest benchmark baseline

The measured source revision is `26f4652`.

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
| Core checking | 2.124 ms |
| Core lowering | 0.927 ms |
| Core compilation | 3.298 ms |
| Core decoding | 0.385 ms |
| Core verification | 1.264 ms |
| Structural verification | 0.454 ms |
| Verification hash | 0.128 ms |
| Semantic identity | 2.383 ms |
| Decoded loading | 1.444 ms |
| Core loading | 1.834 ms |

## Language operations

Each result is a nine-run median.

The runtime process used CPU 0.

| Operation | Result |
| --- | ---: |
| `int_loop` | 33.6 ns |
| `direct_call` | 31.1 ns |
| `string_interp` | 256.9 ns |
| `float_add` | 32.9 ns |
| `string_builder` | 41.1 ns |
| `byte_buffer` | 37.5 ns |
| `direct_clock` | 107.5 ns |

String measurements can vary with process layout.

## Workspace suite

The warm debug workspace suite completed in 42.90 seconds.

The suite used the existing worker count and full coverage.

The standard codec modules link only when source imports them.

## Scheduler foundation

These results use the deterministic scheduler.

| Measurement | Result |
| --- | ---: |
| Proc send and receive | 346.9 ns per message |
| File open and close | 7.079 us per lifecycle |
| Cached 1 KiB file read | 3.152 us per read |
| One 35 ms sleep | 1 park, 1 timeout wakeup |
| Sleep with a signal guardian | 1 park, 1 timeout wakeup |
| Pure-run allocation gate | fewer than 100 allocations |

The shared host queue removed the ten-millisecond polling interval.

The shared host queue also removed the mixed output and child-wait deadlock.
