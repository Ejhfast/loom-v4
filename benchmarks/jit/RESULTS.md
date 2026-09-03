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
| scalar_nodiv | 207.802 | 8.104 | 8.138 | 25.64x | 100.00% | 179.42 | 22.13x |
| top_level_loop | 225.939 | 13.907 | 13.913 | 16.25x | 100.00% | 351.73 | 25.29x |
| image_luma | 373.099 | 20.223 | 20.055 | 18.45x | 99.97% | 285.66 | 14.13x |
| matmul | 46.670 | 1.879 | 1.876 | 24.84x | 100.00% | 22.18 | 11.80x |
| sort_search | 83.645 | 5.327 | 5.328 | 15.70x | 100.00% | 52.93 | 9.94x |
| graph_bfs | 60.016 | 3.701 | 3.713 | 16.22x | 100.00% | 13.90 | 3.76x |
| particles | 46.100 | 4.339 | 4.291 | 10.62x | 100.00% | 13.67 | 3.15x |
| pipeline_style | 103.547 | 15.347 | 15.372 | 6.75x | 100.00% | 43.09 | 2.81x |
| expr_interpreter | 24.472 | 3.534 | 3.512 | 6.93x | 100.00% | 6.26 | 1.77x |
| many_functions | 292.015 | 89.034 | 93.788 | 3.28x | 100.00% | 425.51 | 4.78x |
| wordcount | 15.631 | 3.749 | 3.746 | 4.17x | 100.00% | 5.63 | 1.50x |
| json_parse_large | 77.047 | 18.028 | 17.574 | 4.27x | 100.00% | 22.64 | 1.26x |
| json_pipeline | 111.507 | 28.749 | 29.397 | 3.88x | 100.00% | 28.17 | 0.98x |
| csv_report | 10.910 | 4.521 | 4.589 | 2.41x | 100.00% | 4.15 | 0.92x |
| gcx_churn_low | 71.401 | 1.212 | 1.214 | 58.92x | 100.00% | 38.86 | 32.06x |
| gcx_churn_high | 92.316 | 27.469 | 27.399 | 3.36x | 100.00% | 54.20 | 1.97x |
| gcx_retained | 98.194 | 27.650 | 27.661 | 3.55x | 100.00% | 60.62 | 2.19x |
| gcx_alloc_burst | 33.932 | 6.811 | 6.749 | 4.98x | 100.00% | 23.15 | 3.40x |

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
