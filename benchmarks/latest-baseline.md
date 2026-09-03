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
| LMBC | 69 |
| Compiler ABI | 56 |
| Verifier | 42 |
| Artifact container | 3 |
| Interface | 24 |
| Snapshot | 36 |

## Core image

| Measurement | Result |
| --- | ---: |
| Classes | 309 |
| HIR functions | 684 |
| HIR types | 593 |
| Bytecode functions | 993 |
| Bytecode instructions | 19,951 |
| Decoded instruction width | 16 bytes |
| Core LMBC | 328,413 bytes |
| Core artifact | 328,535 bytes |
| Core checking | 2.337 ms |
| Core lowering | 0.866 ms |
| Core compilation | 3.469 ms |
| Core decoding | 0.409 ms |
| Artifact encoding | 0.153 ms |
| Artifact decoding | 3.839 ms |
| Core verification | 1.387 ms |
| Structural verification | 0.588 ms |
| Verification hash | 0.158 ms |
| Semantic identity | 2.213 ms |
| Namespace publication | 2.018 ms |
| External core load | 5.512 ms |
| Repeated publication | less than 0.001 ms |
| Interface witnesses | 13 entries |

## Thin program artifact

The program contains the source expression `1`.

| Measurement | Result |
| --- | ---: |
| Artifact size | 1,703 bytes |
| Artifact units | 1 |
| Classes | 0 |
| Functions | 1 |
| Artifact decoding | 0.008 ms |
| Source compilation | 1.163 ms |
| Dependency collection | 0.008 ms |
| Namespace publication | 0.584 ms |
| Cold artifact load | 0.571 ms |

## Interpreter operations

| Operation | Iterations | Cost | Total |
| --- | ---: | ---: | ---: |
| Integer loop | 1,000,000 | 37.0 ns | 37.262 ms |
| Direct call | 1,000,000 | 30.3 ns | 30.608 ms |
| Virtual call | 1,000,000 | 67.2 ns | 67.488 ms |
| Field read and write | 1,000,000 | 70.3 ns | 70.611 ms |
| Closure call | 1,000,000 | 97.1 ns | 97.392 ms |
| Class initialization | 500,000 | 202.6 ns | 101.540 ms |
| List push | 500,000 | 40.6 ns | 20.560 ms |
| List index | 1,000,000 | 48.2 ns | 48.430 ms |
| Map insert | 200,000 | 88.8 ns | 18.032 ms |
| Map lookup | 1,000,000 | 59.0 ns | 59.295 ms |
| Map remove and insert | 200,000 | 141.1 ns | 28.474 ms |
| String interpolation | 200,000 | 186.5 ns | 37.571 ms |
| Mixed integer arithmetic | 1,000,000 | 44.5 ns | 44.727 ms |
| Integer bitwise operation | 1,000,000 | 37.7 ns | 37.927 ms |
| Float addition | 1,000,000 | 35.5 ns | 35.732 ms |
| 32-byte XOR | 20,000 | 97.1 ns | 2.205 ms |
| Conditional branch | 1,000,000 | 53.1 ns | 53.384 ms |
| Integer equality | 1,000,000 | 31.7 ns | 31.950 ms |
| Text equality | 1,000,000 | 42.0 ns | 42.268 ms |
| Generic equality | 1,000,000 | 99.1 ns | 99.368 ms |
| Interface default | 1,000,000 | 208.7 ns | 208.972 ms |
| List equality | 200,000 | 859.1 ns | 172.081 ms |
| List hash | 200,000 | 844.0 ns | 169.054 ms |
| Tuple hash | 200,000 | 375.6 ns | 75.373 ms |
| List sort | 20,000 | 20,718.4 ns | 414.629 ms |
| Recursive call | 1,000,000 | 34.7 ns | 34.966 ms |
| Inherited call | 1,000,000 | 67.1 ns | 67.371 ms |
| Closure capture | 1,000,000 | 112.3 ns | 112.544 ms |
| Generic call | 1,000,000 | 52.7 ns | 52.993 ms |
| Enum case | 1,000,000 | 188.1 ns | 188.376 ms |
| Option case | 1,000,000 | 171.6 ns | 171.889 ms |
| String map lookup | 500,000 | 63.1 ns | 31.804 ms |
| Bytes map lookup | 500,000 | 53.0 ns | 26.750 ms |
| Hashable map lookup | 500,000 | 220.6 ns | 110.586 ms |
| String builder | 500,000 | 44.5 ns | 22.517 ms |
| Text iteration | 600,000 | 86.2 ns | 51.977 ms |
| Text split piece | 320,000 | 38.1 ns | 12.469 ms |
| Text split call | 200,000 | 532.4 ns | 106.734 ms |
| Text trim | 500,000 | 104.4 ns | 52.451 ms |
| Bytes decode | 200,000 | 313.8 ns | 63.026 ms |
| Large bytes decode | 20,000 | 961.5 ns | 19.493 ms |
| Text comparison | 1,000,000 | 69.7 ns | 69.978 ms |
| Byte buffer | 500,000 | 42.6 ns | 21.574 ms |
| Scheduled class initialization | 500,000 | 207.3 ns | 103.634 ms |
| Scheduled integer loop | 1,000,000 | 37.1 ns | 37.072 ms |
| Direct clock effect | 1,000,000 | 141.9 ns | 141.939 ms |

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
