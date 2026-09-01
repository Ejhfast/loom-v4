"""CPython ports of the Loom real-program suite.

Same algorithms, same LCG, same sizes as the programs directory.
Run: nix-shell --run "python3 benchmarks/jit/ports.py [case ...]"
"""

import time
from functools import reduce

ROUNDS = 5


def bench(name, fn):
    runs = []
    sink = 0
    for round_index in range(ROUNDS + 1):
        start = time.perf_counter_ns()
        sink += fn()
        elapsed = time.perf_counter_ns() - start
        if round_index > 0:
            runs.append(elapsed)
    runs.sort()
    print("PY\t%s\t%.2f\t%d" % (name, runs[len(runs) // 2] / 1e6, sink))


def lcg(seed):
    return (seed * 1103515245 + 12345) % 2147483648


def top_level_loop():
    seed = 1
    total = 0
    for _ in range(3000000):
        seed = (seed * 1103515245 + 12345) % 2147483648
        if seed % 3 == 0:
            total += seed % 97
        else:
            total -= seed % 13
    return total + seed


def matmul():
    n = 56
    a = []
    b = []
    seed = 12345
    for _ in range(n * n):
        seed = lcg(seed)
        a.append((seed % 1000) / 1000.0)
        seed = lcg(seed)
        b.append((seed % 1000) / 1000.0)
    total = 0.0
    for _ in range(3):
        out = []
        for row in range(n):
            base = row * n
            for col in range(n):
                s = 0.0
                for k in range(n):
                    s += a[base + k] * b[k * n + col]
                out.append(s)
        total += out[0] + out[n * n - 1] + out[n + 7]
    return int(total)


def quicksort(values, low, high):
    if low < high:
        pivot = values[(low + high) // 2]
        i = low
        j = high
        while i <= j:
            while values[i] < pivot:
                i += 1
            while values[j] > pivot:
                j -= 1
            if i <= j:
                values[i], values[j] = values[j], values[i]
                i += 1
                j -= 1
        quicksort(values, low, j)
        quicksort(values, i, high)


def sort_search():
    count = 30000
    values = []
    seed = 424242
    for _ in range(count):
        seed = lcg(seed)
        values.append(seed % 1000000)
    quicksort(values, 0, count - 1)
    found = 0
    seed = 777
    for _ in range(count):
        seed = lcg(seed)
        target = seed % 1000000
        low = 0
        high = count - 1
        while low <= high:
            mid = (low + high) // 2
            probe = values[mid]
            if probe == target:
                found += 1
                break
            if probe < target:
                low = mid + 1
            else:
                high = mid - 1
    return values[0] + values[count - 1] + found


VOCAB = [
    "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta",
    "iota", "kappa", "lambda", "mu", "nu", "xi", "omicron", "pi",
    "rho", "sigma", "tau", "upsilon", "phi", "chi", "psi", "omega",
    "red", "orange", "yellow", "green", "blue", "indigo", "violet", "gray",
    "one", "two", "three", "four", "five", "six", "seven", "eight",
]


def wordcount():
    parts = []
    seed = 99
    for _ in range(20000):
        seed = lcg(seed)
        parts.append(VOCAB[seed % 40])
    doc = " ".join(parts) + " "
    total_words = 0
    best = 0
    for _ in range(2):
        counts = {}
        for word in doc.split(" "):
            if word:
                counts[word] = counts.get(word, 0) + 1
                total_words += 1
        best = max(counts.values())
    return total_words + best


def _pure_json():
    """Return the pure-Python json implementation.

    CPython accelerates the json module with the C `_json` extension.
    The Loom side runs std.json as Loom code, so the fair comparison
    bypasses the accelerator and uses the Python implementation that
    the standard library ships for decoding and encoding.
    """
    import json.decoder as decoder
    import json.encoder as encoder
    import json.scanner as scanner

    decoder.scanstring = decoder.py_scanstring
    scanner.make_scanner = scanner.py_make_scanner
    encoder.c_make_encoder = None
    loads = decoder.JSONDecoder().decode
    dumps = encoder.JSONEncoder(separators=(",", ":")).encode
    return loads, dumps


PURE_LOADS, PURE_DUMPS = _pure_json()


def json_pipeline():
    records = []
    seed = 31337
    for i in range(150):
        seed = lcg(seed)
        score = seed % 1000
        records.append(
            '{"id":%d,"name":"user%d","score":%d,"active":%s}'
            % (i, i, score, "true" if score % 2 == 0 else "false")
        )
    source = "[" + ",".join(records) + "]"
    total = 0
    length = 0
    for _ in range(30):
        document = PURE_LOADS(source)
        for item in document:
            if item["score"] >= 500.0:
                total += 1
        length += len(PURE_DUMPS(document))
    return total + length


def tokenize(text):
    tokens = []
    i = 0
    n = len(text)
    while i < n:
        c = text[i]
        if c == " ":
            i += 1
        elif c == "+":
            tokens.append(("+", 0))
            i += 1
        elif c == "-":
            tokens.append(("-", 0))
            i += 1
        elif c == "*":
            tokens.append(("*", 0))
            i += 1
        elif c == "/":
            tokens.append(("/", 0))
            i += 1
        else:
            value = 0
            while i < n and text[i].isdigit():
                value = value * 10 + (ord(text[i]) - 48)
                i += 1
            tokens.append(("n", value))
    tokens.append(("end", 0))
    return tokens


class Parser:
    __slots__ = ("tokens", "position")

    def __init__(self, tokens):
        self.tokens = tokens
        self.position = 0

    def peek(self):
        return self.tokens[self.position]

    def advance(self):
        token = self.tokens[self.position]
        self.position += 1
        return token

    def factor(self):
        kind, value = self.advance()
        if kind == "n":
            return ("lit", value, None)
        return ("lit", 0, None)

    def term(self):
        left = self.factor()
        while True:
            kind, _ = self.peek()
            if kind == "*":
                self.advance()
                left = ("*", left, self.factor())
            elif kind == "/":
                self.advance()
                left = ("/", left, self.factor())
            else:
                return left

    def expr(self):
        left = self.term()
        while True:
            kind, _ = self.peek()
            if kind == "+":
                self.advance()
                left = ("+", left, self.term())
            elif kind == "-":
                self.advance()
                left = ("-", left, self.term())
            else:
                return left


def evaluate(node):
    kind = node[0]
    if kind == "lit":
        return node[1]
    left = evaluate(node[1])
    right = evaluate(node[2])
    if kind == "+":
        return left + right
    if kind == "-":
        return left - right
    if kind == "*":
        return left * right
    return int(left / right)


def expr_interpreter():
    seed = 2024
    total = 0
    for _ in range(150):
        parts = []
        seed = lcg(seed)
        parts.append(str(1 + seed % 9))
        for _ in range(14):
            seed = lcg(seed)
            op = seed % 4
            parts.append([" + ", " - ", " * ", " / "][op])
            seed = lcg(seed)
            parts.append(str(1 + seed % 9))
        text = "".join(parts)
        tree = Parser(tokenize(text)).expr()
        for _ in range(30):
            total += evaluate(tree)
    return total


class Particle:
    __slots__ = ("x", "y", "vx", "vy")

    def __init__(self, x, y, vx, vy):
        self.x = x
        self.y = y
        self.vx = vx
        self.vy = vy

    def step(self, gravity, bounds):
        self.vy += gravity
        self.x += self.vx
        self.y += self.vy
        if self.x < 0.0 or self.x > bounds:
            self.vx = -self.vx
        if self.y < 0.0 or self.y > bounds:
            self.vy = -self.vy

    def energy(self):
        return self.vx * self.vx + self.vy * self.vy


def particles():
    plist = []
    seed = 5150
    for _ in range(400):
        seed = lcg(seed)
        x = float(seed % 1000)
        seed = lcg(seed)
        y = float(seed % 1000)
        seed = lcg(seed)
        vx = float(seed % 19) - 9.0
        seed = lcg(seed)
        vy = float(seed % 19) - 9.0
        plist.append(Particle(x, y, vx, vy))
    observed = 0.0
    for tick in range(400):
        for p in plist:
            p.step(0.5, 1000.0)
        if tick % 100 == 0:
            for p in plist:
                observed += p.energy()
    return int(observed)


def graph_bfs():
    side = 100
    count = side * side
    adjacency = []
    for node in range(count):
        row = node // side
        col = node % side
        edges = []
        if row > 0:
            edges.append(node - side)
        if row < side - 1:
            edges.append(node + side)
        if col > 0:
            edges.append(node - 1)
        if col < side - 1:
            edges.append(node + 1)
        adjacency.append(edges)
    total = 0
    for rep in range(10):
        distance = [-1] * count
        start = rep * 37 % count
        queue = [start]
        distance[start] = 0
        head = 0
        while head < len(queue):
            current = queue[head]
            head += 1
            for neighbor in adjacency[current]:
                if distance[neighbor] < 0:
                    distance[neighbor] = distance[current] + 1
                    queue.append(neighbor)
        total += sum(distance)
    return total


def csv_report():
    regions = ["north", "south", "east", "west", "center"]
    products = ["ore", "grain", "wood", "cloth", "tools", "salt"]
    rows = []
    seed = 8080
    for _ in range(3000):
        seed = lcg(seed)
        region = regions[seed % 5]
        seed = lcg(seed)
        product = products[seed % 6]
        seed = lcg(seed)
        units = 1 + seed % 40
        seed = lcg(seed)
        price = 50 + seed % 950
        rows.append("%s,%s,%d,%d" % (region, product, units, price))
    csv = "\n".join(rows) + "\n"
    grand_total = 0
    report_len = 0
    for _ in range(3):
        revenue = {}
        for line in csv.split("\n"):
            if line:
                fields = line.split(",")
                units = int(fields[2])
                price = int(fields[3])
                key = fields[0]
                revenue[key] = revenue.get(key, 0) + units * price
        out = []
        grand_total = 0
        for key, value in revenue.items():
            grand_total += value
            out.append("%s: %d\n" % (key, value))
        report_len += len("".join(out))
    return grand_total + report_len


def pipeline_style():
    values = []
    seed = 60601
    for _ in range(50000):
        seed = lcg(seed)
        values.append(seed % 100000)
    total = 0
    for _ in range(10):
        evens = list(filter(lambda v: v % 2 == 0, values))
        scaled = list(map(lambda v: v * 3 % 1000, evens))
        total = (total + reduce(lambda a, b: (a + b) % 1000003, scaled, 0)) % 1000003
    return total


class Node:
    __slots__ = ("a", "b")

    def __init__(self, a, b):
        self.a = a
        self.b = b


def gcx_churn_low():
    total = 0
    for i in range(400000):
        node = Node(i, i + 1)
        total += node.a
    return total


def gcx_churn_high():
    keep = [Node(i, i * 2) for i in range(100000)]
    total = 0
    for i in range(400000):
        node = Node(i, i + 1)
        total += node.a
    return total + len(keep)


def gcx_alloc_burst():
    nodes = [Node(i, i + 1) for i in range(150000)]
    total = 0
    for node in nodes:
        total += node.b
    return total


def scalar_nodiv():
    seed = 1
    total = 0
    for _ in range(3000000):
        seed = (seed * 31 + 7) & 1048575
        if seed > 524288:
            total += seed
        else:
            total -= 1
    return total + seed


def json_parse_large():
    records = []
    seed = 31337
    for i in range(150):
        seed = lcg(seed)
        score = seed % 1000
        records.append(
            '{"id":%d,"name":"user%d","score":%d,"active":%s}'
            % (i, i, score, "true" if score % 2 == 0 else "false")
        )
    source = "[" + ",".join(records) + "]"
    total = 0
    for _ in range(30):
        document = PURE_LOADS(source)
        total += len(document)
    return total


def gcx_retained():
    peak = [Node(i, i * 2) for i in range(150000)]
    peak = []
    total = 0
    for i in range(400000):
        node = Node(i, i + 1)
        total += node.a
    return total + len(peak)


def many_functions():
    src = []
    for i in range(300):
        src.append(
            "def helper%d(value):\n    return value * %d + %d"
            % (i, (i % 9) + 1, i % 7)
        )
    namespace = {}
    exec("\n".join(src), namespace)
    helpers = [namespace["helper%d" % i] for i in range(300)]
    total = 0
    for i in range(25000):
        for h in helpers:
            total = (total + h(i)) % 1000003
    return total



def luma(r, g, b):
    return (r * 299 + g * 587 + b * 114) // 1000


def clamp8(value):
    if value < 0:
        return 0
    if value > 255:
        return 255
    return value


def image_luma():
    reds = []
    greens = []
    blues = []
    seed = 7
    for _ in range(65536):
        seed = (seed * 1103515245 + 12345) & 2147483647
        reds.append(seed & 255)
        greens.append((seed // 256) & 255)
        blues.append((seed // 65536) & 255)
    bright = 0
    total = 0
    for _ in range(30):
        for i in range(65536):
            level = clamp8(luma(reds[i], greens[i], blues[i]) + 16)
            if level > 128:
                bright += 1
            total = (total + level) & 268435455
    return bright + total


CASES = {
    "scalar_nodiv": scalar_nodiv,
    "image_luma": image_luma,
    "top_level_loop": top_level_loop,
    "matmul": matmul,
    "sort_search": sort_search,
    "wordcount": wordcount,
    "json_pipeline": json_pipeline,
    "expr_interpreter": expr_interpreter,
    "particles": particles,
    "graph_bfs": graph_bfs,
    "csv_report": csv_report,
    "json_parse_large": json_parse_large,
    "many_functions": many_functions,
    "pipeline_style": pipeline_style,
    "gcx_churn_low": gcx_churn_low,
    "gcx_churn_high": gcx_churn_high,
    "gcx_retained": gcx_retained,
    "gcx_alloc_burst": gcx_alloc_burst,
}

if __name__ == "__main__":
    import sys

    names = sys.argv[1:] or list(CASES)
    for name in names:
        bench(name, CASES[name])
