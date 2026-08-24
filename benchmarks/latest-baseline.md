# Latest benchmark baseline

The measured source revision is `0e02349`.

The measurements use release builds unless this file states a different build.

The host uses an AMD Ryzen 9 9950X processor.

The host has 16 physical cores and 32 logical processors.

Release runtime benchmarks use deterministic scheduler mode and zero scheduler workers unless stated otherwise.

The operating system selects processors for each benchmark process.

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
| Core checking | 2.071 ms |
| Core lowering | 0.941 ms |
| Core compilation | 3.335 ms |
| Core decoding | 0.386 ms |
| Core verification | 1.276 ms |
| Structural verification | 0.468 ms |
| Verification hash | 0.186 ms |
| Semantic identity | 2.420 ms |
| Decoded loading | 1.456 ms |
| Core loading | 1.846 ms |

## Language operations

Each result is a nine-run median.

| Operation | Result |
| --- | ---: |
| `int_loop` | 30.7 ns |
| `direct_call` | 30.1 ns |
| `string_interp` | 222.6 ns |
| `float_add` | 32.5 ns |
| `string_builder` | 42.0 ns |
| `byte_buffer` | 39.0 ns |
| `direct_clock` | 108.2 ns |

String measurements can vary with process layout.

## Workspace suite

The warm debug workspace suite completed in 43.74 seconds.

The suite used Cargo's default test concurrency and full coverage.

Every test used deterministic scheduler mode and zero scheduler workers.

The standard codec modules link only when source imports them.

## Scheduler foundation

These results use the deterministic scheduler.

| Measurement | Result |
| --- | ---: |
| Proc send and receive | 356.0 ns per message |
| File open and close | 10.731 us per lifecycle |
| Cached 1 KiB file read | 4.116 us per read |
| In-memory file open and close | 1.666 us per lifecycle |
| One 35 ms sleep | 1 park, 1 timeout wakeup |
| Sleep with a signal guardian | 1 park, 1 timeout wakeup |
| Pure-run allocation gate | fewer than 100 allocations |

The shared host queue removed the ten-millisecond polling interval.

The shared host queue also removed the mixed output and child-wait deadlock.

The owned execution lease passes one real cross-thread execution test.

## Parallel scheduler

These results use parallel scheduler mode and the default 4,096-instruction lease.

Each result is the median of three complete benchmark runs.

Each benchmark run uses nine measured executions after one warm execution.

| Tasks | Workers | One worker | Stated workers | Speedup |
| ---: | ---: | ---: | ---: | ---: |
| 2 | 2 | 77.038 ms | 37.504 ms | 2.057x |
| 4 | 4 | 150.046 ms | 39.524 ms | 3.796x |

The pool starts only when a second task can run.

A root-only parallel run uses the inline execution path.

## Host readiness observations

The former host used a ten-millisecond readiness quantum.

Raw terminal input could wait for two quanta. This path could add 20 milliseconds.

A mixed output and child wait could block for more than 15 seconds.

The shared completion queue removed all three conditions.

The execution boundary now keeps every normal ledger writer on the coordinator.
