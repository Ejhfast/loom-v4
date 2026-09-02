# JIT benchmark results

This table compares the three execution engine modes on real programs.
It also compares Loom against CPython on the same workloads.

## Method

- The tree is branch `jit-compilation-heap-abi` at commit `41880de`.
- The host uses an AMD Ryzen 9 9950X processor.
- The Loom columns come from the warm in-process suite on the scheduled path.
- Warm means the native code set is stable before the measured rounds.
- The CPython column is the median of five warm rounds of `ports.py`.
- The CPython version is 3.13.
- The JSON cases bypass the C `_json` accelerator.
- They run the pure-Python implementation that the standard library ships.
- The word-count and CSV programs pass borrowed `Text` keys to their maps.
- `run.sh` reports end-to-end process times instead. Those include compilation.

## Modes

- `interpreter` executes bytecode directly. This is the CLI default.
- `auto` samples hot code and compiles it with the productivity policy.
- `native` compiles every candidate. It is a correctness and test mode.

## Results

| program | interp ms | auto ms | native ms | auto gain | CPython ms | auto vs CPython |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| scalar_nodiv | 258.9 | 8.1 | 8.1 | 32.1x | 177.5 | 21.9x |
| top_level_loop | 254.6 | 13.9 | 13.9 | 18.3x | 354.9 | 25.5x |
| image_luma | 401.9 | 20.2 | 20.1 | 19.9x | 272.9 | 13.5x |
| matmul | 51.9 | 1.9 | 1.9 | 27.7x | 22.0 | 11.6x |
| sort_search | 84.0 | 5.3 | 5.2 | 16.0x | 52.1 | 9.8x |
| graph_bfs | 61.4 | 3.9 | 3.9 | 15.9x | 13.0 | 3.3x |
| particles | 45.3 | 4.1 | 4.1 | 10.9x | 13.6 | 3.3x |
| pipeline_style | 115.8 | 16.1 | 15.8 | 7.2x | 42.5 | 2.6x |
| expr_interpreter | 25.0 | 3.6 | 3.6 | 7.0x | 6.4 | 1.8x |
| many_functions | 303.9 | 94.2 | 91.4 | 3.2x | 421.9 | 4.5x |
| wordcount | 15.7 | 3.7 | 3.7 | 4.3x | 5.6 | 1.52x |
| json_parse_large | 76.5 | 19.3 | 19.4 | 4.0x | 22.5 | 1.16x |
| json_pipeline | 113.2 | 31.0 | 30.7 | 3.7x | 27.9 | 0.90x |
| csv_report | 11.3 | 4.7 | 4.6 | 2.4x | 4.1 | 0.87x |
| gcx_churn_low | 72.9 | 1.2 | 1.2 | 60.1x | 39.1 | 32.6x |
| gcx_churn_high | 93.0 | 31.6 | 31.6 | 2.9x | 53.5 | 1.7x |
| gcx_retained | 101.4 | 31.5 | 31.4 | 3.2x | 61.0 | 1.9x |
| gcx_alloc_burst | 33.9 | 6.9 | 6.8 | 4.9x | 22.3 | 3.2x |

Ratios move with interpreter-side timing noise.
Compare absolute Auto milliseconds for small deltas.
Native matches Auto on every row within noise.

The `auto` geo-mean gain over the interpreter is 8.8x on the core ten rows.
Loom `auto` leads CPython 3.3x geo-mean on the same ten rows.
The four library rows lead CPython 1.08x geo-mean together.
Word count and JSON parsing lead CPython directly.
The CSV and JSON pipeline rows sit within 13 percent of CPython.

## Notes

- The JSON rows compare `std.json` against the pure-Python `json` library.
- Both sides run language-level code. A wrapped C library is not the subject.
- The pure-Python lexer still uses compiled regular expressions.
- The wordcount and csv rows compare against C string paths.
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
