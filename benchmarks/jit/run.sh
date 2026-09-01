#!/usr/bin/env bash
# Run each JIT benchmark program in the three engine modes.
#
# Usage: nix-shell --run "bash benchmarks/jit/run.sh [program ...]"
#
# The times are end-to-end process times. They include compilation.
# The RESULTS.md table uses the warm in-process suite instead.
# Set RUNS to change the repetition count. The row keeps the best run.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
root="$here/../.."
runs="${RUNS:-3}"
(cd "$root" && cargo build --release -p lm-cli --quiet) || exit 1
lm="$root/target/release/lm"
programs=("$@")
if [ ${#programs[@]} -eq 0 ]; then
  for f in "$here"/programs/*.lm; do
    programs+=("$(basename "$f" .lm)")
  done
fi
printf 'program\tinterpreter_ms\tauto_ms\tnative_ms\tpython_ms\n'
for name in "${programs[@]}"; do
  row="$name"
  for mode in interpreter auto native; do
    best=""
    for _ in $(seq "$runs"); do
      start=$(date +%s%N)
      if ! "$lm" run --engine "$mode" "$here/programs/$name.lm" >/dev/null 2>&1; then
        best="fail"
        break
      fi
      end=$(date +%s%N)
      ms=$(((end - start) / 1000000))
      if [ -z "$best" ] || [ "$ms" -lt "$best" ]; then
        best="$ms"
      fi
    done
    row="$row\t$best"
  done
  py="$(python3 "$here/ports.py" "$name" 2>/dev/null | awk -F'\t' '{print $3}')"
  row="$row\t${py:-n/a}"
  # shellcheck disable=SC2059
  printf "$row\n"
done
