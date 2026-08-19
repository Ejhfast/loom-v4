# Week 10 Filesystem Benchmarks

This note records the first effect-focused benchmark. It measures the
filesystem against CPython, and it separates three costs that a single
number hides: the effect boundary, the input and output implementation
behind it, and the device.

`docs/notes/week9-bench.md` holds the language cases.
`docs/notes/week10-text-bench.md` holds the text cases.

## Host and build

- AMD Ryzen 9 9950X, release profile.
- CPython 3.13.12.

## How to run

```sh
nix-shell --run "cargo test --release -p lm-testkit --test bench \
  -- --ignored --nocapture --test-threads=1 bench_filesystem_operations"
nix-shell --run "python3 benchmarks/ops.py fs"
```

## Method

Each case reports the median of three runs of a case that itself
reports the median of nine rounds. Compilation, host construction, and
world construction stay outside the timed region.

Each Loom case runs under `CliHost`, the host `lm run` uses. Two cases
run a second time under `RecordingHost`, which defers a reply to a
later poll exactly as `CliHost` does but makes no call and starts no
thread. That pair separates the effect boundary from the call.

**A warm file measures the page cache and not the filesystem.** A case
that writes a file and reads it back never reaches the device. The
cold case calls `posix_fadvise` with `POSIX_FADV_DONTNEED` first, which
needs no root. The distinction changes the conclusion completely.

An effect benchmark also needs a quiet machine in a way the language
benchmarks do not. A worker thread wake depends on the scheduler, so a
busy host moves a disk case by a third and leaves an in-memory case
inside one percent.

## Per-operation cost

Nanoseconds per operation.

| Case | Loom, disk | Loom, memory | CPython |
| --- | ---: | ---: | ---: |
| `fs_open_close` | 9,670 | **1,048** | 1,418 |
| `fs_read_1k` | 5,694 | **574** | 270 |
| `fs_read_1k` buffered | — | — | 161 |
| `fs_read_64k` | 13,385 | — | 1,504 |
| `fs_read_file` | 14,989 | — | 1,740 |
| `fs_write_1k` | 4,876 | — | 1,566 |
| `fs_read_lines` | 1,154 | — | 26 |

## Throughput

Mebibytes per second over a 64 MiB file, and a 512 MiB cold read.

| Case | Loom | CPython |
| --- | ---: | ---: |
| read, 64 KiB chunks, warm | 3,401 | 16,693 |
| read, 1 MiB chunks, warm | 4,177 | 14,779 |
| write, 64 KiB chunks | 713 | 755 |
| **read, cold** | **888** | **898** |

## What the numbers say

**The effect boundary is not the cost.** One 1 KiB read costs 574 ns
against the in-memory host. That is the perform, the policy check, the
suspend, the completion, and the resume together.

`fs_open_close` states it most sharply. Loom opens and closes a file
through the boundary in 1,048 ns. CPython makes the same two system
calls in 1,418 ns. The effect machinery costs less than the calls it
replaces.

**The worker thread is the cost.** The difference between the two
hosts is the call plus the thread: 5,694 − 574 = 5,120 ns for one
read, of which CPython shows 270 ns is the call. Four to five
microseconds of each operation is the channel send, the thread wake,
and the completion poll.

**For a real file the runtime disappears.** A cold 512 MiB read runs
at 888 MiB/s in Loom and 898 MiB/s in CPython, a one percent
difference. Both wait on the device. The four-fold warm gap exists
only when the bytes are already in memory, and it never limits the
loading of a file.

**The weak spot is per-operation latency, not bandwidth.** A 1 KiB
read costs 3.5 to 5.7 microseconds against 0.27. That is invisible on
a sequential read of a large file and it dominates a loop of small
operations.

**Loom has no buffering layer.** CPython's buffered 1 KiB read costs
161 ns and makes no system call. `fs_read_lines` measures a buffered
line reader written in ordinary Loom code: 1,154 ns for each line
against 26 ns. Buffering works, and it moves the input and output cost
out of the way: 4.4 MB at 64 KiB per refill is 67 reads for 200,000
lines, about 0.17 percent of the run. What remains is allocation.

## Where the remaining cost sits

The reader costs 1,154 ns for each line. Removing the line slice
leaves 806 ns, so producing one line value costs 348 ns. The wrapper
and not the operation carries that cost:

| Loop | ns |
| --- | ---: |
| `b.at(0)`, native call, bare `Int` | 42 |
| `b.get(0)`, same call, Loom-built `Option` and `case` | 210 |
| `b.starts_with(p)`, native call, no allocation | 51 |
| `b.slice(0, 10)`, checks, native slice, `Result`, `case` | 274 |

The native slice costs about 13 ns. One fallible return costs about
168 ns: the allocation, and the `case` at the call site.

A core method can name the raw intrinsic and skip the wrapper. User
code cannot, because `intrinsic` needs core scope. So a buffered
reader belongs in a standard module, and a user-written one pays about
680 ns for each line that it has no way to avoid.

## What this says about the network work

TCP reads from a kernel buffer in memory. That is the warm regime,
where the gap appears and no device hides it, and a protocol loop
makes many small operations. The reassuring cold-read result does not
carry over.

Two questions belong in that design.

**Can a small operation complete on the calling thread?** The boundary
already costs less than a system call, and the handoff costs ten times
one.

**Where does buffering live?** A reader that fills once and serves
small reads from memory turns a round trip into a copy.
