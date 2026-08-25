# Latest benchmark baseline

The measured source revision is `7e07f4d`.

The measurements use release builds unless this file states a different build.

The host uses an AMD Ryzen 9 9950X processor.

The host has 16 physical cores and 32 logical processors.

Release runtime benchmarks use deterministic scheduler mode and zero scheduler workers unless stated otherwise.

Core phase and language operation benchmarks pin each process to logical processor zero.

The operating system selects processors for scheduler benchmark processes.

The bytecode version is 56.

The verifier version is 36.

The snapshot format version is 30.

## Core image

| Measurement | Result |
| --- | ---: |
| Classes | 299 |
| HIR functions | 597 |
| HIR types | 565 |
| Bytecode functions | 896 |
| Bytecode instructions | 18,877 |
| Decoded instruction width | 16 bytes |
| Artifact size | 274,657 bytes |
| Core checking | 2.416 ms |
| Core lowering | 1.009 ms |
| Core compilation | 3.838 ms |
| Core decoding | 0.413 ms |
| Core verification | 1.411 ms |
| Structural verification | 0.512 ms |
| Verification hash | 0.144 ms |
| Semantic identity | 2.553 ms |
| Decoded loading | 1.601 ms |
| Core loading | 2.019 ms |
| Cached core loading | 0.169 ms |
| Default interface witnesses | 11 of 3,588 possible entries |

### Growth from the scheduler foundation

This comparison uses revision `308b55e` with the same processor placement.

| Measurement | Foundation | Current | Change |
| --- | ---: | ---: | ---: |
| HIR functions | 571 | 597 | +4.6% |
| HIR types | 534 | 565 | +5.8% |
| Bytecode functions | 873 | 896 | +2.6% |
| Bytecode instructions | 17,905 | 18,877 | +5.4% |
| Artifact size | 264,401 bytes | 274,657 bytes | +3.9% |
| Core checking | 2.065 ms | 2.416 ms | +17.0% |
| Core lowering | 0.945 ms | 1.009 ms | +6.8% |
| Core compilation | 3.335 ms | 3.838 ms | +15.1% |
| Core verification | 1.278 ms | 1.411 ms | +10.4% |
| Core loading | 1.837 ms | 2.019 ms | +9.9% |

The larger core adds 0.351 milliseconds to core checking.

Large checker inputs retain the prior scaling slope.

Each row subtracts the smallest input from the largest input.

| Checker shape | Sizes | Foundation growth | Current growth | Change |
| --- | ---: | ---: | ---: | ---: |
| Method chain | 16 to 1,024 | 10.058 ms | 10.113 ms | +0.5% |
| Interfaces | 16 to 256 | 1.777 ms | 1.819 ms | +2.4% |
| Wide body | 64 to 1,024 | 0.776 ms | 0.736 ms | -5.2% |

### Sparse default witness gate

Each timing is the median of seven pinned release processes.

Each process reports one median from nine measured runs.

Dense uses revision `4a2dc34`. Sparse uses revision `754db52`.

Cached loading computes the verification hash and rebuilds dispatch tables.

| Measurement | Dense | Sparse | Change |
| --- | ---: | ---: | ---: |
| Default interface call | 268.0 ns | 236.2 ns | -11.9% |
| Cached core loading | 0.171 ms | 0.166 ms | -2.9% |
| Decoded core loading | 1.602 ms | 1.600 ms | -0.1% |
| Full core loading | 2.017 ms | 2.011 ms | -0.3% |
| Witness entries | 3,552 | 11 | -99.7% |

## Language operations

Each result is the median of at least three benchmark processes.

Each process reports one nine-run median.

| Operation | Result |
| --- | ---: |
| `int_loop` | 31.4 ns |
| `direct_call` | 30.5 ns |
| `string_interp` | 208.1 ns |
| `float_add` | 34.2 ns |
| `string_builder` | 40.0 ns |
| `byte_buffer` | 37.1 ns |
| `direct_clock` | 114.5 ns |

String and interpreter measurements can vary with process placement.

### Interface runtime gate

This comparison uses three pinned processes for each revision.

| Operation | Foundation | Current | Change |
| --- | ---: | ---: | ---: |
| `partial_eq` | 92.8 ns | 98.7 ns | +6.4% |
| `list_eq` | 815.3 ns | 812.5 ns | -0.3% |
| `list_hash` | 800.8 ns | 817.7 ns | +2.1% |
| `tuple_hash` | 354.8 ns | 371.9 ns | +4.8% |
| `map_hashable_lookup` | 205.8 ns | 217.0 ns | +5.4% |
| `list_sort` | 19,162.1 ns | 19,011.0 ns | -0.8% |

The largest measured interface delta is 6.4 percent.

## Workspace suite

The warm debug workspace suite completed in 50.26 seconds.

Revision `308b55e` completed in 43.95 seconds under the same settings.

| Target | Foundation | Current | Change |
| --- | ---: | ---: | ---: |
| Full workspace | 43.95 s | 50.26 s | +14.4% |
| Snapshot admission | 10.93 s | 11.75 s | +7.5% |
| Source mutation | 11.48 s | 13.12 s | +14.3% |

Revision `754db52` completed the workspace suite in 48.55 seconds.

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

These results use parallel scheduler mode and local worker turns.

Each result is the median of three complete benchmark runs.

Each benchmark run uses nine measured executions after one warm execution.

| Workload | Tasks | Workers | One worker | Stated workers | Speedup |
| --- | ---: | ---: | ---: | ---: | ---: |
| Integer loop | 2 | 2 | 46.310 ms | 23.187 ms | 2.005x |
| Integer loop | 4 | 4 | 90.844 ms | 24.077 ms | 3.773x |
| Text building | 8 | 4 | 88.099 ms | 23.683 ms | 3.714x |
| Text building | 8 | 8 | 88.099 ms | 14.102 ms | 6.250x |
| Text churn | 8 | 8 | 679.415 ms | 103.645 ms | 6.556x |
| Split n-queens | 12 | 12 | 179.176 ms | 19.635 ms | 9.006x |

The pool starts only when a second task can run.

A root-only parallel run uses the inline execution path.

Boundary-heavy message tasks also remain on the coordinator fast path.

The text-churn row creates one formatted string for each append.

Adaptive local collection limits dead-object retention before the hard heap limit.

### Parallel messages

Each row reports a five-run median and its measured p95.

Each table value is the median of five complete benchmark processes.

The ratio is deterministic time divided by parallel time.

| Case | Messages | Workers | Deterministic | Deterministic p95 | Parallel | Parallel p95 | Ratio |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Ping-pong | 4,003 | 4 | 4.223 ms | 4.391 ms | 4.116 ms | 4.173 ms | 1.026x |
| Stream | 500 | 4 | 0.181 ms | 0.186 ms | 0.180 ms | 0.183 ms | 0.996x |
| Independent pairs | 4,012 | 4 | 4.186 ms | 4.203 ms | 4.245 ms | 4.258 ms | 0.987x |
| Many senders | 800 | 4 | 0.332 ms | 0.336 ms | 0.332 ms | 0.333 ms | 1.007x |
| Allocated stream | 200 | 4 | 0.154 ms | 0.161 ms | 0.155 ms | 0.160 ms | 0.981x |

The aggregate message ratio is 1.011x.

## Structured parallelism

Each result compares `Iterable.par_map` with equivalent parallel work or sequential `Iterable.map`.

| Mode | Workers | Reference | `par_map` | Ratio | Sequential speedup |
| --- | ---: | ---: | ---: | ---: | ---: |
| Parallel | 4 | 274.114 ms | 271.932 ms | 0.992x | 3.50x |
| Parallel | 12 | 113.984 ms | 107.529 ms | 0.943x | 8.86x |
| Deterministic | 1 | 952.967 ms | 948.985 ms | 0.996x | 1.00x |

The parallel reference uses hand-written procs with the same chunking policy.

The deterministic reference uses `Iterable.map`.

The acceptance limit is 1.08x for each ratio.

## Reified VM branching

Each result is the median of three pinned release processes.

Each process creates and completes 100 held runs.

| Method | Time | Branch divided by method |
| --- | ---: | ---: |
| Reuse one snapshot | 0.312 ms | 1.397x |
| Fresh capture and restore | 0.501 ms | 0.861x |
| `Run.branch()` | 0.429 ms | 1.000x |

`Run.branch()` is 14 percent faster than a fresh capture and restore.

A reused snapshot remains faster for repeated copies of one state.

## Host readiness observations

The former host used a ten-millisecond readiness quantum.

Raw terminal input could wait for two quanta. This path could add 20 milliseconds.

A mixed output and child wait could block for more than 15 seconds.

The shared completion queue removed all three conditions.

The execution boundary keeps resource registry updates on the coordinator.
