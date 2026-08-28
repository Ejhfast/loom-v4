# Latest benchmark baseline

The measured tree is the `artifact-packages` worktree after revision `fbf8dbd`.

The tree compiles source modules against sparse dependencies.

The checker selects exact core support declarations.

All comparisons use `main` revision `8f7ba66` in the same session.

The measurements use release builds unless this file states a different build.

The host uses an AMD Ryzen 9 9950X processor.

The host has 16 physical cores and 32 logical processors.

Release runtime benchmarks use deterministic mode unless this file states another mode.

Core and operation benchmarks pin each process to logical processor zero.

The operating system selects processors for scheduler benchmark processes.

The bytecode version is 58.

The verifier version is 38.

The snapshot format version is 35.

## Core image

| Measurement | Result |
| --- | ---: |
| Classes | 300 |
| HIR functions | 597 |
| HIR types | 566 |
| Bytecode functions | 897 |
| Bytecode instructions | 18,899 |
| Decoded instruction width | 16 bytes |
| Core LMBC | 304,763 bytes |
| Core LMAR | 304,885 bytes |
| LMAR wrapper | 122 bytes |
| LMAR encoding | 0.127 ms |
| LMAR decoding and identity | 3.599 ms |
| Core checking | 2.519 ms |
| Core lowering | 1.018 ms |
| Core compilation | 3.947 ms |
| Core decoding | 0.452 ms |
| Core verification | 1.489 ms |
| Structural verification | 0.591 ms |
| Verification hash | 0.136 ms |
| Semantic identity | 2.468 ms |
| Core namespace publication | 3.959 ms |
| External core artifact load | 7.189 ms |
| Repeated artifact publication | 0.759 ms |
| Default interface witnesses | 11 of 3,600 possible entries |

LMBC stores compiler surface facts once.

LMAR adds 122 bytes of package structure.

LMAR does not store an `.lmi` type tree.

No compression or verification cache supplies these results.

### Core comparison

Each timing is the median of three pinned processes.

Each process reports one median from nine measured runs.

| Measurement | `main` | Current | Change |
| --- | ---: | ---: | ---: |
| Core LMBC | 274,657 bytes | 304,763 bytes | +11.0% |
| Core checking | 2.396 ms | 2.519 ms | +5.1% |
| Core lowering | 1.000 ms | 1.018 ms | +1.8% |
| Core compilation | 3.788 ms | 3.947 ms | +4.2% |
| Core decoding | 0.411 ms | 0.452 ms | +10.0% |
| Core verification | 1.411 ms | 1.489 ms | +5.5% |
| Structural verification | 0.516 ms | 0.591 ms | +14.5% |
| Verification hash | 0.137 ms | 0.136 ms | -0.7% |
| Semantic identity | 2.538 ms | 2.468 ms | -2.8% |

The LMBC growth stores exact source contract facts.

The function and class records keep compiler-only fields after execution fields.

This layout keeps the execution record prefix stable.

The external core load verifies one untrusted core artifact.

Normal program loads use the exact runtime core and do not repeat its verification.

## Thin program artifact

The program contains source `1`.

| Measurement | `main` | Current | Change |
| --- | ---: | ---: | ---: |
| Artifact bytes | 274,942 | 1,703 | -99.4% |
| Source compilation | 8.342 ms | 2.130 ms | -74.5% |
| Cold artifact load | 2.033 ms | 1.249 ms | -38.6% |
| Compilation and cold load | 10.375 ms | 3.379 ms | -67.4% |

The root unit contains one function and no class.

Artifact decoding takes 0.009 milliseconds.

Dependency collection takes 0.454 milliseconds.

Namespace publication takes 1.264 milliseconds.

The cold-load timing measures decoding and publication together.

## Command startup

Each cell gives the best result from two batches of twenty release executions.

The benchmark alternates the two revisions within each batch.

| Source | `main` | Current | Change |
| --- | ---: | ---: | ---: |
| `1` | 10.473 / 10.457 ms | 17.914 / 17.854 ms | +70.9% |
| `use std.json.Json` | 35.679 / 35.500 ms | 23.645 / 23.765 ms | -33.4% |
| `use std.http.Http` | 52.625 / 52.806 ms | 31.819 / 31.974 ms | -39.5% |

Tiny command startup still constructs, identifies, verifies, and publishes the runtime core.

Sparse dependency compilation removes repeated core work from standard modules.

## Execution gate

This comparison uses three pinned processes for each revision.

| Operation | `main` | Current | Change |
| --- | ---: | ---: | ---: |
| Direct call | 31.4 ns | 30.8 ns | -1.9% |
| Virtual call | 64.0 ns | 66.4 ns | +3.8% |
| List index | 51.6 ns | 44.2 ns | -14.3% |
| String interpolation | 200.0 ns | 201.0 ns | +0.5% |
| Interface default | 248.1 ns | 258.7 ns | +4.3% |
| List hash | 864.5 ns | 842.8 ns | -2.5% |
| List sort | 19,019.1 ns | 19,250.4 ns | +1.2% |
| Map hashable lookup | 218.8 ns | 214.8 ns | -1.8% |
| String builder | 40.9 ns | 42.0 ns | +2.7% |
| Text iteration | 78.1 ns | 76.7 ns | -1.8% |
| Large bytes decode | 911.6 ns | 933.0 ns | +2.3% |
| Byte buffer | 37.6 ns | 38.0 ns | +1.1% |
| Direct clock | 113.4 ns | 119.3 ns | +5.2% |

The mean operation ratio decreases by 0.1 percent.

The largest measured increase is 5.2 percent.

An adjacent comparison used revision `fbf8dbd` before this work unit.

The current work increases the mean operation ratio by 0.6 percent.

Admission and effect rows do not run inside these operation loops.

## Workspace suite

The warm debug suite uses Cargo's default concurrency and full coverage.

| Revision | Tests | Time |
| --- | ---: | ---: |
| `main` | 1,637 | 50.156 s |
| Current | 1,690 | 34.780 s |

The current tree adds 53 tests.

The current suite is 30.7 percent faster.

| Test binary | `main` | Current | Change |
| --- | ---: | ---: | ---: |
| Snapshot admission | 11.78 s | 5.10 s | -56.7% |
| Snapshot mutation fuzzing | 14.06 s | 7.49 s | -46.7% |
| Snapshot image rules | 0.17 s | 0.20 s | +0.03 s |
| Snapshot restore rules | 0.40 s | 0.31 s | -0.09 s |

Each test executable builds one process-local core `LinkUnit` when required.

This cost appears in compile-heavy test executables.

The test harness uses deterministic mode by default.

Parallel scheduler tests use up to four workers.

CLI integration tests use the parallel default.

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

## Reified VM lifecycle

Each result is the median of nine release executions after one warm-up.

The case runs the nine-queen multishot example.

| Reclamation policy | Time | Adaptive divided by policy |
| --- | ---: | ---: |
| Adaptive record threshold | 401.369 ms | 1.000x |
| Former 1,024-child trigger | 402.299 ms | 0.998x |

The adaptive threshold separates record reclamation from hard resource limits.

The acceptance limit is 1.20x.

## Reified VM branching

Each result is the median of nine release executions after one warm-up.

Each execution creates and completes 100 held runs.

| Method | Time | Branch divided by method |
| --- | ---: | ---: |
| Reuse one snapshot | 0.315 ms | 1.359x |
| Fresh capture and restore | 0.512 ms | 0.836x |
| `Run.branch()` | 0.428 ms | 1.000x |
| `Run.branch_answer()` | 0.343 ms | 1.248x |

`Run.branch()` is 16 percent faster than a fresh capture and restore.

`Run.branch_answer()` is 20 percent faster than `Run.branch()` plus a separate answer.

A reused snapshot remains faster for repeated copies of one state.

## Parallel multishot search

Each result is the median of nine release executions after one warm-up.

The search uses seven queens and four parallel workers.

| Method | Time | Relative result |
| --- | ---: | ---: |
| Direct recursive search | 0.128 ms | 1.000x |
| Deterministic multishot search | 2,047.484 ms | 15,998.220x overhead |
| Parallel multishot search | 1,393.175 ms | 1.470x deterministic speedup |

The multishot search creates 3,072 answered copies and processes 3,585 drive events.

Each selected event rebuilds and rearms the current one-shot wait frontier.

The previous duplicate scans also made each frontier validation quadratic.

`BTreeSet` validation removed those scans. One debug execution fell from 46.74 seconds to 16.99 seconds.

The checked five-queen example takes 0.13 seconds in the debug test profile.

World copying and repeated frontier registration still dominate this small search.

Direct recursive search is the correct implementation when each branch has little work.

Parallel multishot search helps when each copied world performs enough work between boundaries.

A future persistent wait collection can remove repeated full-frontier registration.

## Host readiness observations

The former host used a ten-millisecond readiness quantum.

Raw terminal input could wait for two quanta. This path could add 20 milliseconds.

A mixed output and child wait could block for more than 15 seconds.

The shared completion queue removed all three conditions.

The execution boundary keeps resource registry updates on the coordinator.
