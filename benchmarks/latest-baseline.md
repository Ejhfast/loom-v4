# Latest benchmark baseline

The measured source revision is `1e9ac3c`.

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
| Core checking | 2.125 ms |
| Core lowering | 0.960 ms |
| Core compilation | 3.430 ms |
| Core decoding | 0.383 ms |
| Core verification | 1.256 ms |
| Structural verification | 0.451 ms |
| Verification hash | 0.127 ms |
| Semantic identity | 2.384 ms |
| Decoded loading | 1.431 ms |
| Core loading | 1.818 ms |

## Language operations

Each result is the median of four benchmark processes.

Each process reports one nine-run median.

| Operation | Result |
| --- | ---: |
| `int_loop` | 33.4 ns |
| `direct_call` | 31.9 ns |
| `string_interp` | 254.2 ns |
| `float_add` | 33.4 ns |
| `string_builder` | 44.3 ns |
| `byte_buffer` | 40.9 ns |
| `direct_clock` | 110.3 ns |

String and interpreter measurements can vary with process placement.

## Workspace suite

The warm debug workspace suite completed in 44.41 seconds.

The suite used Cargo's default test concurrency and full coverage.

The test harness used deterministic mode by default.

Parallel scheduler tests used up to four workers.

CLI integration tests used the parallel default.

The standard codec modules link only when source imports them.

## Scheduler foundation

These results use the deterministic scheduler.

| Measurement | Result |
| --- | ---: |
| Proc send and receive | 362.4 ns per message |
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

These results use parallel scheduler mode and the default 4,096-instruction turn.

Each result is the median of three complete benchmark runs.

Each benchmark run uses nine measured executions after one warm execution.

| Workload | Tasks | Workers | One worker | Stated workers | Speedup |
| --- | ---: | ---: | ---: | ---: | ---: |
| Integer loop | 2 | 2 | 57.084 ms | 30.161 ms | 1.893x |
| Integer loop | 4 | 4 | 111.908 ms | 30.097 ms | 3.718x |
| Text building | 8 | 4 | 103.283 ms | 27.699 ms | 3.729x |
| Text building | 8 | 8 | 103.283 ms | 14.447 ms | 7.149x |
| Split n-queens | 12 | 12 | 210.004 ms | 24.947 ms | 8.418x |

The pool starts only when a second task can run.

A root-only parallel run uses the inline execution path.

Boundary-heavy message tasks also remain on the coordinator fast path.

### Parallel messages

Each row reports a five-run median and its measured p95.

Each table value is the median of five complete benchmark processes.

The ratio is deterministic time divided by parallel time.

| Case | Messages | Workers | Deterministic | Deterministic p95 | Parallel | Parallel p95 | Ratio |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Ping-pong | 4,003 | 4 | 4.200 ms | 4.445 ms | 4.126 ms | 4.151 ms | 1.018x |
| Stream | 500 | 4 | 0.179 ms | 0.184 ms | 0.181 ms | 0.184 ms | 0.989x |
| Independent pairs | 4,012 | 4 | 4.251 ms | 4.261 ms | 4.267 ms | 4.299 ms | 0.996x |
| Many senders | 800 | 4 | 0.335 ms | 0.339 ms | 0.333 ms | 0.336 ms | 1.006x |
| Allocated stream | 200 | 4 | 0.153 ms | 0.159 ms | 0.153 ms | 0.163 ms | 1.000x |

The aggregate message ratio is 1.006x.

## Host readiness observations

The former host used a ten-millisecond readiness quantum.

Raw terminal input could wait for two quanta. This path could add 20 milliseconds.

A mixed output and child wait could block for more than 15 seconds.

The shared completion queue removed all three conditions.

The execution boundary now keeps every normal ledger writer on the coordinator.
