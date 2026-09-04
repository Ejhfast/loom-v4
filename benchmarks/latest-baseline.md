# Benchmark baseline

These measurements use an AMD Ryzen 9 9950X processor.

All measurements use release builds.

Most times are medians of nine measured runs after one warm run.

Message times are medians of five measured runs after one warm run.

Language operation costs subtract the empty-program baseline.

Parallel measurements let the operating system select processors.

## Format versions

| Format | Version |
| --- | ---: |
| LMBC | 73 |
| Compiler ABI | 57 |
| Verifier | 43 |
| Artifact container | 3 |
| Interface | 25 |
| Snapshot | 36 |

## Core image

| Measurement | Result |
| --- | ---: |
| Classes | 312 |
| HIR functions | 684 |
| HIR types | 596 |
| Bytecode functions | 996 |
| Bytecode instructions | 19,990 |
| Decoded instruction width | 16 bytes |
| Core LMBC | 329,802 bytes |
| Core artifact | 329,924 bytes |
| Core checking | 2.276 ms |
| Core lowering | 0.846 ms |
| Core compilation | 3.391 ms |
| Core decoding | 0.396 ms |
| Artifact encoding | 0.145 ms |
| Artifact decoding | 3.403 ms |
| Core verification | 2.206 ms |
| Structural verification | 0.615 ms |
| Verification hash | 0.152 ms |
| Semantic identity | 2.340 ms |
| Namespace publication | 2.809 ms |
| External core load | 6.278 ms |
| Repeated publication | less than 0.001 ms |
| Interface witnesses | 13 entries |

## Thin program artifact

The program contains the source expression `1`.

| Measurement | Result |
| --- | ---: |
| Artifact size | 1,731 bytes |
| Artifact units | 1 |
| Classes | 0 |
| Functions | 1 |
| Artifact decoding | 0.008 ms |
| Source compilation | 1.338 ms |
| Dependency collection | 0.008 ms |
| Namespace publication | 0.636 ms |
| Cold artifact load | 0.623 ms |

## Interpreter operations

| Operation | Iterations | Cost | Total |
| --- | ---: | ---: | ---: |
| Integer loop | 1,000,000 | 37.5 ns | 37.754 ms |
| Direct call | 1,000,000 | 30.7 ns | 30.920 ms |
| Virtual call | 1,000,000 | 70.0 ns | 70.202 ms |
| Field read and write | 1,000,000 | 73.3 ns | 73.508 ms |
| Closure call | 1,000,000 | 99.4 ns | 99.627 ms |
| Class initialization | 500,000 | 188.2 ns | 94.347 ms |
| List push | 500,000 | 49.2 ns | 24.847 ms |
| List index | 1,000,000 | 48.6 ns | 48.831 ms |
| Map insert | 200,000 | 90.8 ns | 18.403 ms |
| Map lookup | 1,000,000 | 67.4 ns | 67.620 ms |
| Map remove and insert | 200,000 | 146.2 ns | 29.474 ms |
| String interpolation | 200,000 | 187.4 ns | 37.719 ms |
| Mixed integer arithmetic | 1,000,000 | 61.4 ns | 61.629 ms |
| Integer bitwise operation | 1,000,000 | 35.6 ns | 35.874 ms |
| Float addition | 1,000,000 | 34.6 ns | 34.834 ms |
| 32-byte XOR | 20,000 | 96.7 ns | 2.169 ms |
| Conditional branch | 1,000,000 | 62.6 ns | 62.806 ms |
| Integer equality | 1,000,000 | 35.7 ns | 35.957 ms |
| Text equality | 1,000,000 | 44.0 ns | 44.236 ms |
| Generic equality | 1,000,000 | 103.2 ns | 103.394 ms |
| Interface default | 1,000,000 | 199.4 ns | 199.614 ms |
| List equality | 200,000 | 875.7 ns | 175.366 ms |
| List hash | 200,000 | 846.8 ns | 169.588 ms |
| Tuple hash | 200,000 | 375.5 ns | 75.328 ms |
| List sort | 20,000 | 21,656.1 ns | 433.357 ms |
| Recursive call | 1,000,000 | 34.2 ns | 34.439 ms |
| Inherited call | 1,000,000 | 71.9 ns | 72.166 ms |
| Closure capture | 1,000,000 | 110.7 ns | 110.942 ms |
| Generic call | 1,000,000 | 64.1 ns | 64.287 ms |
| Enum case | 1,000,000 | 190.3 ns | 190.583 ms |
| Option case | 1,000,000 | 177.3 ns | 177.523 ms |
| String map lookup | 500,000 | 66.5 ns | 33.507 ms |
| Bytes map lookup | 500,000 | 58.8 ns | 29.616 ms |
| Hashable map lookup | 500,000 | 222.8 ns | 111.656 ms |
| String builder | 500,000 | 50.7 ns | 25.602 ms |
| Text iteration | 600,000 | 88.0 ns | 53.038 ms |
| Text split piece | 320,000 | 36.0 ns | 11.758 ms |
| Text split call | 200,000 | 519.3 ns | 104.091 ms |
| Text trim | 500,000 | 103.8 ns | 52.119 ms |
| Bytes decode | 200,000 | 298.6 ns | 59.961 ms |
| Large bytes decode | 20,000 | 907.7 ns | 18.389 ms |
| Text comparison | 1,000,000 | 79.2 ns | 79.405 ms |
| Byte buffer | 500,000 | 41.2 ns | 20.813 ms |
| Scheduled class initialization | 500,000 | 189.8 ns | 94.906 ms |
| Scheduled integer loop | 1,000,000 | 38.0 ns | 37.981 ms |
| Direct clock effect | 1,000,000 | 149.9 ns | 149.905 ms |

## Parallel scheduler

| Workload | Tasks | Workers | Serial | Parallel | Speedup |
| --- | ---: | ---: | ---: | ---: | ---: |
| Integer loop | 2 | 2 | 47.515 ms | 23.313 ms | 2.038x |
| Integer loop | 4 | 4 | 90.164 ms | 23.563 ms | 3.826x |
| Native integer loop | 6 | 8 | 66.845 ms | 4.329 ms | 15.440x |

The native case reached six active execution leases.

### Message scheduling

The ratio is deterministic time divided by parallel time.

| Case | Messages | Workers | Deterministic | Parallel | Ratio |
| --- | ---: | ---: | ---: | ---: | ---: |
| Ping-pong | 4,003 | 4 | 4.266 ms | 4.333 ms | 0.985x |
| Stream | 500 | 4 | 0.223 ms | 0.226 ms | 0.986x |
| Independent pairs | 4,012 | 4 | 4.336 ms | 4.431 ms | 0.979x |
| Many senders | 800 | 4 | 0.408 ms | 0.418 ms | 0.976x |
| Allocated stream | 200 | 4 | 0.170 ms | 0.173 ms | 0.979x |

### Structured parallelism

The ratio is `par_map` time divided by reference time.

| Mode | Workers | Reference | `par_map` | Ratio |
| --- | ---: | ---: | ---: | ---: |
| Parallel | 4 | 253.294 ms | 242.549 ms | 0.958x |
| Parallel | 12 | 99.005 ms | 98.083 ms | 0.991x |
| Deterministic | 1 | 993.120 ms | 940.402 ms | 0.947x |

## Reproduction

Run each group inside the repository build environment:

```sh
nix-shell --run "cargo test --release -p lm-testkit --test bench bench_core_compilation -- --ignored --nocapture"
nix-shell --run "cargo test --release -p lm-testkit --test bench bench_program_artifact_linking -- --ignored --nocapture"
nix-shell --run "cargo test --release -p lm-testkit --test bench bench_language_operations -- --ignored --nocapture"
nix-shell --run "cargo test --release -p lm-testkit --test bench bench_parallel_cpu_scaling -- --ignored --nocapture"
nix-shell --run "cargo test --release -p lm-testkit --test bench bench_parallel_native_cpu_scaling -- --ignored --nocapture"
nix-shell --run "cargo test --release -p lm-testkit --test bench bench_parallel_messages -- --ignored --nocapture"
nix-shell --run "cargo test --release -p lm-testkit --test bench bench_parallel_par_map_queens -- --ignored --nocapture"
```
