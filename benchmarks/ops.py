"""CPython side of the language-operation benchmarks.

Every case runs the same workload as the Loom case of the same name in
`crates/lm-testkit/tests/bench.rs`. CPython is a frame of reference,
not a target: it says whether a Loom number is reasonable for an
interpreter.

Method. Each case runs one warm-up round and then nine measured
rounds, and reports the median. The timer wraps the workload alone, so
interpreter start-up stays outside. A workload returns a value the
case consumes, so no work is dead.

Run it through the build shell:

    nix-shell --run "python3 benchmarks/ops.py"

The output is one tab-separated row per case, in the same shape as the
Loom table.
"""

import time

ROUNDS = 9


def measure(fn, iterations):
    """Median wall time of `fn`, in nanoseconds per iteration."""
    runs = []
    for round_index in range(ROUNDS + 1):
        start = time.perf_counter_ns()
        result = fn()
        elapsed = time.perf_counter_ns() - start
        assert result is not None
        if round_index > 0:
            runs.append(elapsed)
    runs.sort()
    total = runs[len(runs) // 2]
    return total / iterations, total / 1e6


def int_loop():
    i = 0
    s = 0
    while i < 1000000:
        s = s + i
        i = i + 1
    return s


def add1(n):
    return n + 1


def direct_call():
    i = 0
    s = 0
    while i < 1000000:
        s = add1(s)
        i = i + 1
    return s


class Adder:
    def __init__(self):
        self.step = 1

    def bump(self, n):
        return n + self.step


def virtual_call():
    a = Adder()
    i = 0
    s = 0
    while i < 1000000:
        s = a.bump(s)
        i = i + 1
    return s


class Cell:
    def __init__(self):
        self.v = 0

    def step(self):
        self.v = self.v + 1


def field_rw():
    c = Cell()
    i = 0
    while i < 1000000:
        c.step()
        i = i + 1
    return c.v


def closure_call():
    i = 0
    s = 0
    while i < 1000000:
        f = lambda x: x + 1  # noqa: E731 - the Loom case builds a closure too
        s = f(s)
        i = i + 1
    return s


class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y


def class_init():
    i = 0
    s = 0
    while i < 500000:
        p = Point(i, i)
        s = s + p.x
        i = i + 1
    return s


def list_push():
    xs = []
    i = 0
    while i < 500000:
        xs.append(i)
        i = i + 1
    return len(xs)


def list_index():
    xs = []
    i = 0
    while i < 1000:
        xs.append(i)
        i = i + 1
    j = 0
    s = 0
    while j < 1000000:
        s = s + xs[j % 1000]
        j = j + 1
    return s


def map_insert():
    m = {}
    i = 0
    while i < 200000:
        m[i] = i
        i = i + 1
    return len(m)


def map_lookup():
    m = {}
    i = 0
    while i < 1000:
        m[i] = i
        i = i + 1
    j = 0
    s = 0
    while j < 1000000:
        s = s + m[j % 1000]
        j = j + 1
    return s


def string_interp():
    s = ""
    i = 0
    while i < 200000:
        s = f"v{i}"
        i = i + 1
    return s


CASES = [
    ("int_loop", int_loop, 1000000),
    ("direct_call", direct_call, 1000000),
    ("virtual_call", virtual_call, 1000000),
    ("field_rw", field_rw, 1000000),
    ("closure_call", closure_call, 1000000),
    ("class_init", class_init, 500000),
    ("list_push", list_push, 500000),
    ("list_index", list_index, 1000000),
    ("map_insert", map_insert, 200000),
    ("map_lookup", map_lookup, 1000000),
    ("string_interp", string_interp, 200000),
]


def main():
    import sys

    # The recursion case descends 1000 frames, which meets the default
    # limit exactly.
    sys.setrecursionlimit(20000)
    print(f"# CPython {sys.version.split()[0]}")
    print("CPY\tcase\titers\tns_per_op\ttotal_ms")
    for name, fn, iterations in CASES:
        per_op, total_ms = measure(fn, iterations)
        print(f"CPY\t{name}\t{iterations}\t{per_op:.1f}\t{total_ms:.3f}")


# ---------------------------------------------------------------
# The cases added in the second pass.
#
# Three have no exact CPython analogue, and the table marks them:
# `generic_call` runs a plain function, because CPython erases type
# arguments; `enum_case` runs a `match` over a tagged class; and
# `option_case` runs the `None` check that stands in for `Option`.
# Read those three ratios as indicative, not as a like-for-like.
# ---------------------------------------------------------------


def arith_mix():
    i = 1
    s = 0
    while i < 1000001:
        s = s + i * 3 // 2 % 7
        i = i + 1
    return s


def branch():
    i = 0
    s = 0
    while i < 1000000:
        if i % 2 == 0:
            s = s + 1
        else:
            s = s - 1
        i = i + 1
    return s


def down(n):
    if n <= 0:
        return 0
    return down(n - 1) + 1


def recursion():
    i = 0
    s = 0
    while i < 1000:
        s = s + down(1000)
        i = i + 1
    return s


class Base:
    def __init__(self):
        self.step = 1

    def bump(self, n):
        return n + self.step


class Derived(Base):
    pass


def inherit_call():
    d = Derived()
    i = 0
    s = 0
    while i < 1000000:
        s = d.bump(s)
        i = i + 1
    return s


def closure_capture():
    k = 7
    i = 0
    s = 0
    while i < 1000000:
        f = lambda x: x + k  # noqa: E731 - the Loom case builds a closure too
        s = f(s)
        i = i + 1
    return s


def pick(a, b):
    return a


def generic_call():
    i = 0
    s = 0
    while i < 1000000:
        s = pick(s + 1, 0)
        i = i + 1
    return s


class Up:
    __match_args__ = ("v",)

    def __init__(self, v):
        self.v = v


class Down:
    __match_args__ = ("v",)

    def __init__(self, v):
        self.v = v


def enum_case():
    i = 0
    s = 0
    while i < 1000000:
        e = Up(1)
        match e:
            case Up(v):
                s = s + v
            case Down(v):
                s = s - v
        i = i + 1
    return s


def option_case():
    xs = []
    i = 0
    while i < 1000:
        xs.append(i)
        i = i + 1
    j = 0
    s = 0
    while j < 1000000:
        v = xs[j % 1000] if (j % 1000) < len(xs) else None
        if v is not None:
            s = s + v
        j = j + 1
    return s


def map_str_lookup():
    m = {}
    i = 0
    while i < 1000:
        m[f"k{i}"] = i
        i = i + 1
    j = 0
    s = 0
    while j < 500000:
        s = s + m["k500"]
        j = j + 1
    return s


def string_builder():
    parts = []
    i = 0
    while i < 500000:
        parts.append("x")
        i = i + 1
    return "".join(parts)


def byte_buffer():
    b = bytearray()
    i = 0
    while i < 500000:
        b.append(65)
        i = i + 1
    return len(b)


def text_each():
    text = "aé猫" * 20
    total = 0
    i = 0
    while i < 10000:
        for scalar in text:
            total = total + 1
        i = i + 1
    return total


def text_split():
    row = "alpha,beta,gamma,delta,epsilon,zeta,eta,theta,iota,kappa"
    total = 0
    i = 0
    while i < 32000:
        total = total + len(row.split(","))
        i = i + 1
    return total


def text_split_once():
    line = "content-length: 4096"
    total = 0
    i = 0
    while i < 200000:
        head, sep, _ = line.partition(": ")
        total = total + (len(head) if sep else 0)
        i = i + 1
    return total


def text_trim():
    padded = "   content-length   "
    total = 0
    i = 0
    while i < 500000:
        total = total + len(padded.strip())
        i = i + 1
    return total


def bytes_decode():
    raw = b"a" * 512
    total = 0
    j = 0
    while j < 200000:
        total = total + len(raw.decode("utf-8"))
        j = j + 1
    return total


def bytes_decode_large():
    raw = b"a" * 65536
    total = 0
    j = 0
    while j < 20000:
        total = total + len(raw.decode("utf-8"))
        j = j + 1
    return total


def text_compare():
    a = "content-length"
    b = "content-type"
    total = 0
    i = 0
    while i < 1000000:
        if a < b:
            total = total + 1
        i = i + 1
    return total


CASES.extend(
    [
        ("arith_mix", arith_mix, 1000000),
        ("branch", branch, 1000000),
        ("recursion", recursion, 1000000),
        ("inherit_call", inherit_call, 1000000),
        ("closure_capture", closure_capture, 1000000),
        ("generic_call", generic_call, 1000000),
        ("enum_case", enum_case, 1000000),
        ("option_case", option_case, 1000000),
        ("map_str_lookup", map_str_lookup, 500000),
        ("string_builder", string_builder, 500000),
        ("byte_buffer", byte_buffer, 500000),
        ("text_each", text_each, 600000),
        ("text_split", text_split, 320000),
        ("text_split_once", text_split_once, 200000),
        ("text_trim", text_trim, 500000),
        ("bytes_decode", bytes_decode, 200000),
        ("bytes_decode_large", bytes_decode_large, 20000),
        ("text_compare", text_compare, 1000000),
    ]
)




# ---------------------------------------------------------------
# The filesystem cases.
#
# Every case runs the same workload as the Loom case of the same name
# in `bench_filesystem_operations`. A Loom file operation crosses an
# effect boundary and completes on a worker thread; CPython calls the
# system call directly, so the ratio measures the boundary.
#
# The read cases run twice. The unbuffered pair matches Loom call for
# call: one system call per read. The buffered pair is what a CPython
# program writes by default, where a small read is a copy out of an
# internal buffer and no system call happens at all.
# ---------------------------------------------------------------

import os
import tempfile

FS_DIR = os.path.join(tempfile.gettempdir(), f"lm-fs-bench-cpy-{os.getpid()}")
FS_DATA = os.path.join(FS_DIR, "data.bin")
FS_SMALL = os.path.join(FS_DIR, "small.txt")
FS_OUT = os.path.join(FS_DIR, "out.bin")


def fs_setup():
    os.makedirs(FS_DIR, exist_ok=True)
    with open(FS_DATA, "wb") as f:
        f.write(b"x" * (8 * 1024 * 1024))
    with open(FS_SMALL, "wb") as f:
        f.write(b"x" * 4096)


def fs_open_close():
    total = 0
    for _ in range(2000):
        f = open(FS_DATA, "rb", buffering=0)
        f.close()
        total = total + 1
    return total


def fs_read_1k():
    with open(FS_DATA, "rb", buffering=0) as f:
        total = 0
        for _ in range(2000):
            total = total + len(f.read(1024))
    return total


def fs_read_1k_buffered():
    with open(FS_DATA, "rb") as f:
        total = 0
        for _ in range(2000):
            total = total + len(f.read(1024))
    return total


def fs_read_64k():
    with open(FS_DATA, "rb", buffering=0) as f:
        total = 0
        for _ in range(100):
            total = total + len(f.read(65536))
    return total


def fs_read_file():
    total = 0
    for _ in range(1000):
        f = open(FS_SMALL, "rb", buffering=0)
        total = total + len(f.read(8192))
        f.close()
    return total


def fs_write_1k():
    chunk = b"y" * 1024
    with open(FS_OUT, "wb", buffering=0) as f:
        total = 0
        for _ in range(2000):
            total = total + f.write(chunk)
    return total


FS_CASES = [
    ("fs_open_close", fs_open_close, 2000),
    ("fs_read_1k", fs_read_1k, 2000),
    ("fs_read_1k_buffered", fs_read_1k_buffered, 2000),
    ("fs_read_64k", fs_read_64k, 100),
    ("fs_read_file", fs_read_file, 1000),
    ("fs_write_1k", fs_write_1k, 2000),
]


def fs_main():
    """Run the filesystem cases. Setup stays outside the timer."""
    import shutil
    import sys

    fs_setup()
    try:
        print(f"# CPython {sys.version.split()[0]} filesystem")
        print("CPY\tcase\titers\tns_per_op\ttotal_ms")
        for name, fn, iterations in FS_CASES:
            per_op, total_ms = measure(fn, iterations)
            print(f"CPY\t{name}\t{iterations}\t{per_op:.0f}\t{total_ms:.3f}")
    finally:
        shutil.rmtree(FS_DIR, ignore_errors=True)


if __name__ == "__main__":
    import sys

    if len(sys.argv) > 1 and sys.argv[1] == "fs":
        fs_main()
    else:
        main()
