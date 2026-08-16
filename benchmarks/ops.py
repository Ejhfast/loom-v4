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

    print(f"# CPython {sys.version.split()[0]}")
    print("CPY\tcase\titers\tns_per_op\ttotal_ms")
    for name, fn, iterations in CASES:
        per_op, total_ms = measure(fn, iterations)
        print(f"CPY\t{name}\t{iterations}\t{per_op:.1f}\t{total_ms:.3f}")


if __name__ == "__main__":
    main()
