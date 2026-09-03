# JIT benchmark results

These measurements compare all execution engine modes on complete programs.

They also compare Loom with equivalent CPython programs.

## Method

- The host uses an AMD Ryzen 9 9950X processor.
- The Loom values use release builds and the scheduled execution path.
- Each warm value is the median of nine stable runs.
- One engine and namespace serve every warm run.
- The CPython value is the median of five warm runs.
- The CPython version is 3.13.12.
- The JSON cases use the pure-Python implementation.
- The JSON cases do not use the C `_json` accelerator.

## Engine modes

- `interpreter` executes verified bytecode.
- `auto` samples execution and compiles productive code.
- `native` compiles each eligible function for differential tests.

## Warm results

The Loom columns show milliseconds.

`Auto versus Python` is CPython time divided by Loom Auto time.

| Program | Interpreter | Auto | Native | Auto gain | Coverage | CPython | Auto versus Python |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| scalar_nodiv | 262.423 | 8.102 | 8.113 | 32.39x | 100.00% | 179.42 | 22.15x |
| top_level_loop | 273.527 | 13.930 | 13.924 | 19.64x | 100.00% | 351.73 | 25.25x |
| image_luma | 400.598 | 20.214 | 20.153 | 19.82x | 99.97% | 285.66 | 14.13x |
| matmul | 44.999 | 1.891 | 1.891 | 23.80x | 100.00% | 22.18 | 11.73x |
| sort_search | 89.810 | 5.329 | 5.300 | 16.85x | 100.00% | 52.93 | 9.93x |
| graph_bfs | 62.622 | 3.652 | 3.666 | 17.15x | 100.00% | 13.90 | 3.81x |
| particles | 44.499 | 4.500 | 4.457 | 9.89x | 100.00% | 13.67 | 3.04x |
| pipeline_style | 108.497 | 16.237 | 16.114 | 6.68x | 100.00% | 43.09 | 2.65x |
| expr_interpreter | 24.480 | 3.539 | 3.537 | 6.92x | 100.00% | 6.26 | 1.77x |
| many_functions | 307.974 | 93.422 | 93.523 | 3.30x | 100.00% | 425.51 | 4.55x |
| wordcount | 15.912 | 3.744 | 3.732 | 4.25x | 100.00% | 5.63 | 1.50x |
| json_parse_large | 75.387 | 18.081 | 18.576 | 4.17x | 100.00% | 22.64 | 1.25x |
| json_pipeline | 111.716 | 28.520 | 28.312 | 3.92x | 100.00% | 28.17 | 0.99x |
| csv_report | 10.852 | 4.397 | 4.394 | 2.47x | 100.00% | 4.15 | 0.94x |
| digest | 9.993 | 0.390 | 0.420 | 25.62x | 99.81% | — | — |
| gcx_churn_low | 69.609 | 1.215 | 1.213 | 57.29x | 100.00% | 38.86 | 31.98x |
| gcx_churn_high | 89.279 | 26.850 | 26.915 | 3.33x | 100.00% | 54.20 | 2.02x |
| gcx_retained | 96.127 | 27.684 | 27.737 | 3.47x | 100.00% | 60.62 | 2.19x |
| gcx_alloc_burst | 33.874 | 6.474 | 6.491 | 5.23x | 100.00% | 23.15 | 3.58x |

All executed bytecode instructions have a dedicated JIT treatment.

The coverage value is the native instruction share in Auto mode.

The image case has scheduled environment exits outside native regions.

## Cold results

Cold Auto time includes native compilation.

| Program | Interpreter | Auto | Auto gain | Regions | Code bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| Integer loop | 39.454 ms | 1.562 ms | 25.25x | 1 | 1,705 |
| JSON parse | 44.727 ms | 258.269 ms | 0.17x | 26 | 815,795 |
| 295 hot functions | 13.992 ms | 356.825 ms | 0.04x | 295 | 1,180,682 |

The warm 295-function program runs in 3.440 ms under Auto.

Its warm interpreter time is 13.361 ms.

## Reproduction

Run the warm Loom suite:

```sh
nix-shell --run "cargo test --release -p lm-testkit --test bench bench_jit_application_programs -- --ignored --nocapture"
```

Run the cold Loom suite:

```sh
nix-shell --run "cargo test --release -p lm-testkit --test bench bench_jit_cold_start_and_cache_pressure -- --ignored --nocapture"
```

Run the probe suite:

```sh
nix-shell --run "cargo test --release -p lm-testkit --test bench bench_jit_probe_programs -- --ignored --nocapture"
```

Run the process harness:

```sh
nix-shell --run "bash benchmarks/jit/run.sh"
```

Run the CPython programs:

```sh
nix-shell --run "python3 benchmarks/jit/ports.py"
```
