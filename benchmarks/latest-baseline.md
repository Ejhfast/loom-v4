# Latest benchmark baseline

The measured source revision is `3ccc5a5`.

The measurements use release builds unless this file states a different build.

The host uses an AMD Ryzen 9 9950X processor.

The host has 16 physical cores and 32 logical processors.

Release runtime benchmarks use deterministic scheduler mode and zero scheduler workers.

The runtime process uses CPU 0.

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
| Core checking | 2.063 ms |
| Core lowering | 0.941 ms |
| Core compilation | 3.369 ms |
| Core decoding | 0.391 ms |
| Core verification | 1.276 ms |
| Structural verification | 0.460 ms |
| Verification hash | 0.128 ms |
| Semantic identity | 2.422 ms |
| Decoded loading | 1.460 ms |
| Core loading | 1.849 ms |

## Language operations

Each result is a nine-run median.

| Operation | Result |
| --- | ---: |
| `int_loop` | 31.4 ns |
| `direct_call` | 30.5 ns |
| `string_interp` | 203.3 ns |
| `float_add` | 32.1 ns |
| `string_builder` | 43.3 ns |
| `byte_buffer` | 40.2 ns |
| `direct_clock` | 104.9 ns |

String measurements can vary with process layout.

## Workspace suite

The warm debug workspace suite completed in 43.68 seconds.

The suite used Cargo's default test concurrency and full coverage.

Every test used deterministic scheduler mode and zero scheduler workers.

The standard codec modules link only when source imports them.

## Scheduler foundation

These results use the deterministic scheduler.

| Measurement | Result |
| --- | ---: |
| Proc send and receive | 354.3 ns per message |
| File open and close | 7.127 us per lifecycle |
| Cached 1 KiB file read | 3.422 us per read |
| One 35 ms sleep | 1 park, 1 timeout wakeup |
| Sleep with a signal guardian | 1 park, 1 timeout wakeup |
| Pure-run allocation gate | fewer than 100 allocations |

The shared host queue removed the ten-millisecond polling interval.

The shared host queue also removed the mixed output and child-wait deadlock.

The owned execution lease passes one real cross-thread execution test.

## Host readiness observations

The former host used a ten-millisecond readiness quantum.

Raw terminal input could wait for two quanta. This path could add 20 milliseconds.

A mixed output and child wait could block for more than 15 seconds.

The shared completion queue removed all three conditions.

## Ledger update comparison

These release measurements used CPU 0 and deterministic scheduler mode.

| Ledger update | Revision | `direct_clock` | Proc send and receive |
| --- | --- | ---: | ---: |
| Atomic read-modify-write | `00e584e` | 117.6 ns | 355.5 ns |
| Serialized load and store | `57a0120` | 107.1 ns | 344.2 ns |

Serialized updates reduced both measured costs.

The execution boundary now keeps every normal ledger writer on the coordinator.
