# JIT benchmark results

This table compares the three execution engine modes on real programs.
It also compares Loom against CPython on the same workloads.

## Method

- The tree is branch `jit-compilation-heap-abi` at commit `311b8c5`.
- The host uses an AMD Ryzen 9 9950X processor.
- The Loom columns come from the warm in-process suite on the scheduled path.
- Warm means the native code set is stable before the measured rounds.
- The CPython column is the median of five warm rounds of `ports.py`.
- The CPython version is 3.13.
- `run.sh` reports end-to-end process times instead. Those include compilation.

## Modes

- `interpreter` executes bytecode directly. This is the CLI default.
- `auto` samples hot code and compiles it with the productivity policy.
- `native` compiles every candidate. It is a correctness and test mode.

## Results

| program | interp ms | auto ms | native ms | auto gain | CPython ms | auto vs CPython |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| scalar_nodiv | 230.2 | 8.1 | 8.1 | 28.4x | 176.7 | 21.8x |
| image_luma | 398.0 | 20.5 | 20.5 | 19.4x | 275.9 | 13.5x |
| top_level_loop | 259.0 | 13.9 | 14.0 | 18.6x | 346.5 | 24.9x |
| matmul | 45.0 | 1.9 | 1.9 | 24.0x | 22.1 | 11.6x |
| sort_search | 86.2 | 5.2 | 5.2 | 16.4x | 52.3 | 10.1x |
| graph_bfs | 63.0 | 3.8 | 3.8 | 16.5x | 13.3 | 3.5x |
| particles | 44.8 | 4.1 | 4.1 | 10.9x | 13.7 | 3.3x |
| pipeline_style | 108.6 | 15.6 | 15.2 | 7.0x | 45.8 | 2.9x |
| expr_interpreter | 24.5 | 3.6 | 3.6 | 6.8x | 6.3 | 1.7x |
| many_functions | 328.1 | 92.7 | 92.9 | 3.5x | 421.8 | 4.6x |
| json_pipeline | 127.6 | 47.6 | 46.1 | 2.7x | 2.6 | 0.06x |
| json_parse_large | 86.3 | 33.9 | 33.0 | 2.5x | 1.3 | 0.04x |
| wordcount | 21.0 | 9.2 | 9.2 | 2.3x | 5.6 | 0.60x |
| csv_report | 12.9 | 7.4 | 7.4 | 1.7x | 4.1 | 0.55x |
| gcx_churn_low | 72.3 | 1.2 | 1.2 | 59.2x | 38.7 | 32.3x |
| gcx_churn_high | 92.5 | 32.3 | 32.7 | 2.9x | 53.8 | 1.7x |
| gcx_retained | 99.5 | 34.4 | 34.5 | 2.9x | 62.3 | 1.8x |
| gcx_alloc_burst | 34.4 | 7.1 | 7.1 | 4.9x | 23.7 | 3.3x |

The `auto` geo-mean gain over the interpreter is 7.6x on the core ten rows.
Loom `auto` leads CPython 2.2x geo-mean on the same ten rows.
Native matches auto on every row within noise.

## Notes

- The JSON rows compare `std.json` in Loom against the C `json` module.
- The wordcount and csv rows also compare against C string paths.
- Every row runs identically in all three modes. The suite checks this.
- Effect and parallel benchmarks live in `crates/lm-testkit`.

## Reproduction

Run the end-to-end harness:

```sh
nix-shell --run "bash benchmarks/jit/run.sh"
```

Run the CPython side alone:

```sh
nix-shell --run "python3 benchmarks/jit/ports.py"
```

Run one program in one mode:

```sh
nix-shell --run "cargo run --release -p lm-cli -- run --engine auto --show-result benchmarks/jit/programs/image_luma.lm"
```
