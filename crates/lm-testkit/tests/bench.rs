//! The benchmark suite.
//!
//! The benchmark groups run here. Every case is `#[ignore]`, so the
//! ordinary suite never pays for them. Run them with:
//!
//! ```text
//! nix-shell --run "cargo test --release -p lm-testkit --test bench \
//!   -- --ignored --nocapture"
//! ```
//!
//! Set `LOOM_BENCH_FILTER` to one case name for a focused run.
//!
//! Method. Each case compiles and loads once outside the timed
//! region, then times the run alone. The reported cost subtracts an
//! empty-program baseline, so it excludes machine construction. Every
//! case runs a warm-up round, then reports the median of the
//! remaining rounds. A workload returns a value the program consumes,
//! so no work is dead.
//!
//! The output is one tab-separated row per case, so a reader can join
//! it with the CPython table from `benchmarks/ops.py`.

use lm_vm::{Vm, VmConfig};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Rounds per case. One warm-up plus this many measured rounds.
const ROUNDS: usize = 9;

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort_unstable();
    values[values.len() / 2]
}

/// A large fuel budget: a benchmark must never stop on fuel.
fn config() -> VmConfig {
    VmConfig {
        fuel: 20_000_000_000,
        ..VmConfig::default()
    }
}

/// Time one program: compile and load once, then run it `ROUNDS + 1`
/// times and take the median run.
fn time_program(source: &str) -> Duration {
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let loaded = lm_vm::load_bytes(&bytes).expect("the benchmark artifact must load");
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let mut vm = Vm::new(&loaded, config());
        let outcome = vm.run();
        let elapsed = start.elapsed();
        assert!(
            matches!(outcome, lm_vm::Outcome::Done(_)),
            "the benchmark faulted: {}",
            vm.show_outcome(&outcome)
        );
        if round > 0 {
            runs.push(elapsed);
        }
    }
    median(runs)
}

/// The cost of machine construction and entry, with no workload.
fn baseline() -> Duration {
    time_program("0\n")
}

fn selected(name: &str) -> bool {
    let Ok(filter) = std::env::var("LOOM_BENCH_FILTER") else {
        return true;
    };
    filter.split(',').any(|item| item == name)
}

/// Report one case: the per-operation cost above the baseline.
fn report(name: &str, iterations: u64, source: &str, base: Duration) {
    if !selected(name) {
        return;
    }
    let total = time_program(source);
    let work = total.saturating_sub(base);
    let per = work.as_nanos() as f64 / iterations as f64;
    println!(
        "LOOM\t{name}\t{iterations}\t{:.1}\t{:.3}",
        per,
        total.as_secs_f64() * 1e3
    );
}

/// Report one case that runs inside a `World`.
///
/// The cases above build a bare `Vm`. Every tool builds a `World`,
/// and a `World` adds the aggregate ledgers and the activation loop.
/// A program with no proc runs one machine there, so this case
/// measures the path a plain `lm run` takes.
fn report_world(name: &str, iterations: u64, source: &str, expected: &str) {
    if !selected(name) {
        return;
    }
    let total = time_world(source, &[], config(), expected);
    let per = total.as_nanos() as f64 / iterations as f64;
    println!(
        "LOOM\t{name}\t{iterations}\t{:.1}\t{:.3}",
        per,
        total.as_secs_f64() * 1e3
    );
}

/// Time one proc program. Compile and load stay outside the timed region.
fn time_world(source: &str, grants: &[&str], config: VmConfig, expected: &str) -> Duration {
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let loaded = lm_vm::load_bytes(&bytes).expect("the benchmark artifact must load");
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let host = Rc::new(RefCell::new(lm_vm::RecordingHost::new(1)));
        let mut world = lm_vm::World::new(&loaded, config, Box::new(host));
        for grant in grants {
            world.allow(grant).expect("the benchmark grant must exist");
        }
        let outcome = lm_proc::run_world(&mut world);
        let elapsed = start.elapsed();
        assert_eq!(world.show_outcome(&outcome), expected);
        if round > 0 {
            runs.push(elapsed);
        }
    }
    median(runs)
}

// ---------------------------------------------------------------
// Group 1: the language operations.
// ---------------------------------------------------------------

#[test]
#[ignore]
fn bench_language_operations() {
    let base = baseline();
    println!("LOOM\tcase\titers\tns_per_op\ttotal_ms");
    println!(
        "LOOM\t_baseline\t1\t{:.1}\t{:.3}",
        base.as_nanos() as f64,
        base.as_secs_f64() * 1e3
    );

    // An integer while loop: the interpreter dispatch floor.
    report(
        "int_loop",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + i\n  i = i + 1\nend\ns\n",
        base,
    );

    // A direct call to a top-level function.
    report(
        "direct_call",
        1_000_000,
        "def add1(n: Int): Int\n  n + 1\nend\n\
         i = 0\ns = 0\nwhile i < 1000000\n  s = add1(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A virtual call through the dispatch row.
    report(
        "virtual_call",
        1_000_000,
        "class Adder\n  step: Int = 1\n  def bump(self, n: Int): Int\n    n + self.step\n  end\nend\n\
         a = Adder()\ni = 0\ns = 0\nwhile i < 1000000\n  s = a.bump(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A field read and a field write on a mutable receiver.
    report(
        "field_rw",
        1_000_000,
        "class Cell\n  v: Int = 0\n  def step(mut self)\n    self.v = self.v + 1\n  end\nend\n\
         c = Cell()\ni = 0\nwhile i < 1000000\n  c.step()\n  i = i + 1\nend\nc.v\n",
        base,
    );

    // Closure creation plus a call.
    report(
        "closure_call",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  f = { |x: Int|: Int x + 1 }\n  s = f(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // Object construction.
    report(
        "class_init",
        500_000,
        "class Point\n  x: Int = 0\n  y: Int = 0\n  def init(mut self, x: Int, y: Int)\n    \
         self.x = x\n    self.y = y\n  end\nend\n\
         i = 0\ns = 0\nwhile i < 500000\n  p = Point(i, i)\n  s = s + p.x\n  i = i + 1\nend\ns\n",
        base,
    );

    // List append.
    report(
        "list_push",
        500_000,
        "xs: [Int] = []\ni = 0\nwhile i < 500000\n  xs.push(i)\n  i = i + 1\nend\nxs.len()\n",
        base,
    );

    // List index on a built list.
    report(
        "list_index",
        1_000_000,
        "xs: [Int] = []\ni = 0\nwhile i < 1000\n  xs.push(i)\n  i = i + 1\nend\n\
         j = 0\ns = 0\nwhile j < 1000000\n  s = s + xs.at(j % 1000)\n  j = j + 1\nend\ns\n",
        base,
    );

    // Map insert with integer keys.
    report(
        "map_insert",
        200_000,
        "m: {Int: Int} = {}\ni = 0\nwhile i < 200000\n  m.put(i, i)\n  i = i + 1\nend\nm.len()\n",
        base,
    );

    // Map lookup on a built map.
    report(
        "map_lookup",
        1_000_000,
        "m: {Int: Int} = {}\ni = 0\nwhile i < 1000\n  m.put(i, i)\n  i = i + 1\nend\n\
         j = 0\ns = 0\nwhile j < 1000000\n  s = s + m.at(j % 1000)\n  j = j + 1\nend\ns\n",
        base,
    );

    // Each map removal leaves one tombstone. Reinsertion keeps the
    // live map size stable and exercises periodic compaction.
    report(
        "map_remove_reinsert",
        200_000,
        "m: {Int: Int} = {}\ni = 0\nwhile i < 1000\n  m.put(i, i)\n  i = i + 1\nend\n\
         j = 0\nwhile j < 200000\n  key = j % 1000\n  m.remove(key)\n  m.put(key, key)\n  j = j + 1\nend\nm.len()\n",
        base,
    );

    // String interpolation formats one integer into new short text.
    // Accumulation here would measure quadratic copying instead.
    report(
        "string_interp",
        200_000,
        "s = \"\"\ni = 0\nwhile i < 200000\n  s = \"v{i}\"\n  i = i + 1\nend\ns\n",
        base,
    );

    // Mixed integer arithmetic: multiply, divide, and modulo.
    report(
        "arith_mix",
        1_000_000,
        "i = 1\ns = 0\nwhile i < 1000001\n  s = s + i * 3 / 2 % 7\n  i = i + 1\nend\ns\n",
        base,
    );

    // One taken branch and one untaken branch per iteration.
    report(
        "branch",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  if i % 2 == 0\n    s = s + 1\n  else\n    s = s - 1\n  end\n  i = i + 1\nend\ns\n",
        base,
    );

    // Integer equality keeps its sealed instruction inside a hot loop.
    report(
        "int_eq",
        1_000_000,
        "i = 0\nsame = false\nwhile i < 1000000\n  same = i == i\n  i = i + 1\nend\nsame\n",
        base,
    );

    // Text equality keeps its native content instruction.
    report(
        "text_eq",
        1_000_000,
        "a = \"loom\"\nb = \"loom\"\ni = 0\nsame = false\nwhile i < 1000000\n  same = a == b\n  i = i + 1\nend\nsame\n",
        base,
    );

    // Generic equality measures one verified interface call.
    report(
        "partial_eq",
        1_000_000,
        "final class Token implements PartialEq\n  value: Int\n  def init(mut self, value: Int)\n    self.value = value\n  end\n  def __eq__(self, other: Token): Bool\n    self.value == other.value\n  end\nend\ndef same[T: PartialEq](a: T, b: T): Bool\n  a == b\nend\na = Token(7)\nb = Token(7)\ni = 0\nequal = false\nwhile i < 1000000\n  equal = same(a, b)\n  i = i + 1\nend\nequal\n",
        base,
    );

    // Conditional list equality compares all elements.
    report(
        "list_eq",
        200_000,
        "left = [1, 2, 3, 4, 5, 6, 7, 8]\nright = left.copy()\n\
         i = 0\nequal = false\nwhile i < 200000\n  equal = left == right\n  i = i + 1\nend\nequal\n",
        base,
    );

    // Conditional list hashing combines all elements.
    report(
        "list_hash",
        200_000,
        "values = [1, 2, 3, 4, 5, 6, 7, 8]\ni = 0\nhash = 0\n\
         while i < 200000\n  hash = hash_of(values)\n  i = i + 1\nend\nhash\n",
        base,
    );

    // Tuple hashing uses the ordinary conditional interface path.
    report(
        "tuple_hash",
        200_000,
        "value = (1, 2, 3, 4)\ni = 0\nhash = 0\nwhile i < 200000\n  \
         hash = hash_of(value)\n  i = i + 1\nend\nhash\n",
        base,
    );

    // Closure-free sorting copies and sorts sixteen integers.
    report(
        "list_sort",
        20_000,
        "source = [16, 7, 12, 3, 10, 1, 14, 5, 8, 15, 2, 11, 6, 13, 4, 9]\n\
         i = 0\nfirst = 0\nwhile i < 20000\n  values = source.copy()\n  values.sort()\n  first = values.at(0)\n  i = i + 1\nend\nfirst\n",
        base,
    );

    // Recursion: the call path with a growing activation stack.
    report(
        "recursion",
        1_000_000,
        "def down(n: Int): Int\n  if n <= 0\n    0\n  else\n    down(n - 1) + 1\n  end\nend\n\
         i = 0\ns = 0\nwhile i < 1000\n  s = s + down(1000)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A virtual call that resolves on an inherited method.
    report(
        "inherit_call",
        1_000_000,
        "class Base\n  step: Int = 1\n  def bump(self, n: Int): Int\n    n + self.step\n  end\nend\n\
         class Derived < Base\nend\n\
         d = Derived()\ni = 0\ns = 0\nwhile i < 1000000\n  s = d.bump(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A closure that captures a local, against the free closure above.
    report(
        "closure_capture",
        1_000_000,
        "k = 7\ni = 0\ns = 0\nwhile i < 1000000\n  f = { |x: Int|: Int x + k }\n  s = f(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A generic call: the type application path.
    report(
        "generic_call",
        1_000_000,
        "def pick[T](a: T, b: T): T\n  a\nend\n\
         i = 0\ns = 0\nwhile i < 1000000\n  s = pick(s + 1, 0)\n  i = i + 1\nend\ns\n",
        base,
    );

    // Enum construction plus a `case` dispatch over two arms.
    report(
        "enum_case",
        1_000_000,
        "enum Step\n  Up(v: Int)\n  Down(v: Int)\nend\n\
         i = 0\ns = 0\nwhile i < 1000000\n  e: Step = Up(1)\n  \
         s = s + case e\n  in Up(v) then v\n  in Down(v) then 0 - v\n  end\n  i = i + 1\nend\ns\n",
        base,
    );

    // The non-faulting list access: a native op that builds a core
    // `Option`, then a `case` over it.
    report(
        "option_case",
        1_000_000,
        "xs: [Int] = []\ni = 0\nwhile i < 1000\n  xs.push(i)\n  i = i + 1\nend\n\
         j = 0\ns = 0\nwhile j < 1000000\n  \
         s = s + case xs.get(j % 1000)\n  in Some(v) then v\n  in None then 0\n  end\n  j = j + 1\nend\ns\n",
        base,
    );

    // A map with string keys, against the integer-key cases above.
    report(
        "map_str_lookup",
        500_000,
        "m: {String: Int} = {}\ni = 0\nwhile i < 1000\n  m.put(\"k{i}\", i)\n  i = i + 1\nend\n\
         j = 0\ns = 0\nwhile j < 500000\n  s = s + m.at(\"k500\")\n  j = j + 1\nend\ns\n",
        base,
    );

    // A map with immutable byte keys uses the native byte hash path.
    report(
        "map_bytes_lookup",
        500_000,
        "key = Bytes(\"loom\")\nm: {Bytes: Int} = {}\nm.put(key, 7)\n\
         j = 0\ns = 0\nwhile j < 500000\n  s = s + m.at(key)\n  j = j + 1\nend\ns\n",
        base,
    );

    // A user key uses one hash call and one equality call per lookup.
    report(
        "map_hashable_lookup",
        500_000,
        "final class Key implements Hashable\n  value: Int\n  \
         def init(mut self, value: Int)\n    self.value = value\n  end\n  \
         def __eq__(self, other: Key): Bool\n    self.value == other.value\n  end\n  \
         def __hash__(self): Int\n    self.value\n  end\nend\n\
         key = Key(7).freeze()\nm = Map[Key, Int]()\nm.put(key, 9)\n\
         j = 0\ns = 0\nwhile j < 500000\n  s = s + m.at(key)\n  j = j + 1\nend\ns\n",
        base,
    );

    // The string builder uses the growable text path.
    report(
        "string_builder",
        500_000,
        "b = StringBuilder()\ni = 0\nwhile i < 500000\n  b.append(\"x\")\n  i = i + 1\nend\nb.build()\n",
        base,
    );

    // Scalar traversal uses one forward UTF-8 byte cursor.
    report(
        "text_each",
        600_000,
        "def ignore(value: Char): ()\n  ()\nend\n\
         text = \"aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫\"\n\
         i = 0\nwhile i < 10000\n  text.each(ignore)\n  i = i + 1\nend\ntext.len()\n",
        base,
    );

    // Split a document into fields. This case measures the design and
    // not the search: a Loom piece shares the source allocation, and
    // a CPython piece is a copy. The count is pieces, not iterations.
    report(
        "text_split",
        320_000,
        "row = \"alpha,beta,gamma,delta,epsilon,zeta,eta,theta,iota,kappa\"\n\
         total = 0\ni = 0\nwhile i < 32000\n  total = total + row.split(\",\").len()\n\
         \x20 i = i + 1\nend\ntotal\n",
        base,
    );

    // Split a line into a key and a value. This is the shape a
    // configuration or header parser writes, and it allocates one
    // Option and one tuple for each line.
    report(
        "text_split_once",
        200_000,
        "line = \"content-length: 4096\"\ntotal = 0\ni = 0\nwhile i < 200000\n\
         \x20 total = total + case line.split_once(\": \")\n\
         \x20 in Some((key, _)) then key.byte_len()\n  in None then 0\n  end\n\
         \x20 i = i + 1\nend\ntotal\n",
        base,
    );

    // Narrow one piece and keep it as a view. Loom copies nothing.
    report(
        "text_trim",
        500_000,
        "padded = \"   content-length   \"\ntotal = 0\ni = 0\nwhile i < 500000\n\
         \x20 total = total + padded.trim().byte_len()\n  i = i + 1\nend\ntotal\n",
        base,
    );

    // Decode bytes to text. Loom validates once and shares the
    // allocation. CPython allocates and copies.
    report(
        "bytes_decode",
        200_000,
        "b = ByteBuffer()\ni = 0\nwhile i < 512\n  b.append(97)\n  i = i + 1\nend\n\
         raw = b.finish()\ntotal = 0\nj = 0\nwhile j < 200000\n\
         \x20 total = total + case raw.utf8_view()\n  in Ok(text) then text.byte_len()\n\
         \x20 in Err(_) then 0\n  end\n  j = j + 1\nend\ntotal\n",
        base,
    );

    // The same decode over a large buffer. The Loom cost is one
    // validation plus one allocation and does not grow with the copy
    // CPython must make, so this pair locates the crossing point.
    report(
        "bytes_decode_large",
        20_000,
        "b = ByteBuffer()\ni = 0\nwhile i < 65536\n  b.append(97)\n  i = i + 1\nend\n\
         raw = b.finish()\ntotal = 0\nj = 0\nwhile j < 20000\n\
         \x20 total = total + case raw.utf8_view()\n  in Ok(text) then text.byte_len()\n\
         \x20 in Err(_) then 0\n  end\n  j = j + 1\nend\ntotal\n",
        base,
    );

    // Compare two strings. The ordering hooks reach one intrinsic.
    report(
        "text_compare",
        1_000_000,
        "a = \"content-length\"\nb = \"content-type\"\ntotal = 0\ni = 0\n\
         while i < 1000000\n  if a < b\n    total = total + 1\n  end\n  i = i + 1\nend\ntotal\n",
        base,
    );

    // The byte buffer.
    report(
        "byte_buffer",
        500_000,
        "b = ByteBuffer()\ni = 0\nwhile i < 500000\n  b.append(65)\n  i = i + 1\nend\nb.len()\n",
        base,
    );

    // The two cases below run the same workload inside a `World`.
    // The allocating case reports the heap ledger cost, and the
    // integer case reports the activation loop cost alone.
    report_world(
        "world_class_init",
        500_000,
        "class Point\n  x: Int = 0\n  y: Int = 0\n  def init(mut self, x: Int, y: Int)\n    \
         self.x = x\n    self.y = y\n  end\nend\n\
         i = 0\ns = 0\nwhile i < 500000\n  p = Point(i, i)\n  s = s + p.x\n  i = i + 1\nend\ns\n",
        "Done(124999750000)",
    );
    report_world(
        "world_int_loop",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + i\n  i = i + 1\nend\ns\n",
        "Done(499999500000)",
    );
}

#[test]
#[ignore]
fn bench_collection_operations() {
    let base = baseline();
    println!("LOOM\tcase\titers\tns_per_op\ttotal_ms");
    println!(
        "LOOM\t_baseline\t1\t{:.1}\t{:.3}",
        base.as_nanos() as f64,
        base.as_secs_f64() * 1e3
    );

    // Native list traversal creates no iterator or Option per element.
    report(
        "list_for",
        1_000_000,
        "xs: [Int] = []\ni = 0\nwhile i < 1000\n  xs.push(i)\n  i = i + 1\nend\n\
         rounds = 0\ns = 0\nwhile rounds < 1000\n  for value in xs\n    s = s + value\n  end\n\
           rounds = rounds + 1\nend\ns\n",
        base,
    );

    // A nonescaping callback avoids one closure object per call.
    report(
        "list_each",
        1_000_000,
        "class Total\n  value: Int = 0\n  def add(mut self, n: Int)\n    self.value = self.value + n\n  end\nend\n\
         xs: [Int] = []\ni = 0\nwhile i < 1000\n  xs.push(i)\n  i = i + 1\nend\n\
           total = Total()\nrounds = 0\nwhile rounds < 1000\n  xs.each() { |value: Int| total.add(value) }\n\
           rounds = rounds + 1\nend\ntotal.value\n",
        base,
    );

    // This eager pipeline applies three ordinary core algorithms.
    report(
        "list_pipeline",
        60_000,
        "xs: [Int] = []\ni = 0\nwhile i < 20000\n  xs.push(i)\n  i = i + 1\nend\n\
         mapped = xs.map[Int]() { |value: Int| value + 1 }\n\
         filtered = mapped.filter() { |value: Int| value % 2 == 0 }\n\
         filtered.fold[Int](0) { |sum: Int, value: Int| sum + value }\n",
        base,
    );

    // Map traversal passes the key and value without a tuple object.
    report(
        "map_each",
        1_000_000,
        "class Total\n  value: Int = 0\n  def add(mut self, key: Int, value: Int)\n    self.value = self.value + key + value\n  end\nend\n\
         table: {Int: Int} = {}\ni = 0\nwhile i < 1000\n  table.put(i, i)\n  i = i + 1\nend\n\
           total = Total()\nrounds = 0\nwhile rounds < 1000\n  table.each() { |key: Int, value: Int| total.add(key, value) }\n\
           rounds = rounds + 1\nend\ntotal.value\n",
        base,
    );
}

#[test]
#[ignore]
fn bench_proc_operations() {
    let source = "class Adder < Proc[Int]\n\
                  \x20 total: Int = 0\n\
                  \x20 def on_spawn(mut self): Int with Proc\n\
                  \x20   loop do\n\
                  \x20     case self.receive()\n\
                  \x20     in Msg(n)\n\
                  \x20       self.total = self.total + n\n\
                  \x20     in Closed\n\
                  \x20       return self.total\n\
                  \x20     end\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Adder.spawn()\n\
                  i = 0\n\
                  while i < 20000\n  h.send(1)\n  i = i + 1\nend\n\
                  h.close()\n\
                  case h.done()\n\
                  in Done(v)  then v\n\
                  in Fault(_) then -1\n\
                  end\n";
    let elapsed = time_world(source, &["Proc"], config(), "Done(20000)");
    println!(
        "LOOM\tproc_send_receive\t20000\t{:.1}\t{:.3}",
        elapsed.as_nanos() as f64 / 20_000.0,
        elapsed.as_secs_f64() * 1e3
    );
}

// ---------------------------------------------------------------
// Group 2: the type checker.
// ---------------------------------------------------------------

/// Generate a module of `n` small functions that call their
/// predecessor, plus a class with `n` methods.
fn checker_source(n: usize) -> String {
    let mut out = String::new();
    out.push_str("class Shape\n  size: Int = 1\n");
    for i in 0..n {
        out.push_str(&format!(
            "  def area{i}(self, k: Int): Int\n    self.size * k + {i}\n  end\n"
        ));
    }
    out.push_str("end\n");
    out.push_str("def f0(n: Int): Int\n  n + 1\nend\n");
    for i in 1..n {
        out.push_str(&format!(
            "def f{i}(n: Int): Int\n  f{} (n) + {i}\nend\n",
            i - 1
        ));
    }
    out.push_str(&format!("s = Shape()\nf{}(s.area0(1))\n", n - 1));
    out
}

/// `n` independent classes, each with two fields and two methods.
fn class_source(n: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        out.push_str(&format!(
            "class C{i}\n  a: Int = {i}\n  b: String = \"c{i}\"\n  \
             def sum(self, k: Int): Int\n    self.a + k\n  end\n  \
             def name(self): String\n    self.b\n  end\nend\n"
        ));
    }
    out.push_str("x = C0()\nx.sum(1)\n");
    out
}

/// One inheritance chain `n` deep. Every level overrides nothing, so
/// the checker resolves each method through the chain.
fn inherit_source(n: usize) -> String {
    let mut out =
        String::from("class L0\n  v: Int = 0\n  def get(self): Int\n    self.v\n  end\nend\n");
    for i in 1..n {
        out.push_str(&format!("class L{i} < L{}\nend\n", i - 1));
    }
    out.push_str(&format!("x = L{}()\nx.get()\n", n - 1));
    out
}

/// `n` generic functions, each instantiated at two types.
fn generic_source(n: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        out.push_str(&format!("def g{i}[T](a: T, b: T): T\n  a\nend\n"));
    }
    let mut body = String::from("s = 0\n");
    for i in 0..n {
        body.push_str(&format!("s = s + g{i}(1, 2)\n"));
        body.push_str(&format!("t{i} = g{i}(\"a\", \"b\")\n"));
    }
    out.push_str(&body);
    out.push_str("s\n");
    out
}

/// A chain of `n` assignments whose types flow through generic calls.
/// Each step must infer its type argument from the step before.
fn inference_source(n: usize) -> String {
    let mut out = String::from("def thru[T](x: T): T\n  x\nend\nv0 = 1\n");
    for i in 1..n {
        out.push_str(&format!("v{i} = thru(v{}) + 1\n", i - 1));
    }
    out.push_str(&format!("v{}\n", n - 1));
    out
}

/// One enum of `n` arms and one `case` that covers every arm.
fn enum_source(n: usize) -> String {
    let mut out = String::from("enum E\n");
    for i in 0..n {
        out.push_str(&format!("  A{i}(v: Int)\n"));
    }
    out.push_str("end\ndef pick(e: E): Int\n  case e\n");
    for i in 0..n {
        out.push_str(&format!("  in A{i}(v) then v + {i}\n"));
    }
    out.push_str("  end\nend\ne: E = A0(1)\npick(e)\n");
    out
}

/// One function whose body is `n` statements, against `n` functions.
fn wide_body_source(n: usize) -> String {
    let mut out = String::from("def big(): Int\n  s = 0\n");
    for i in 0..n {
        out.push_str(&format!("  s = s + {i}\n"));
    }
    out.push_str("  s\nend\nbig()\n");
    out
}

/// Generate `n` conformances and calls through one interface bound.
fn interface_source(n: usize) -> String {
    let mut out = String::from(
        "interface Measured\n  type Item\n  def measure(self): Int\nend\n\
         def read[T: Measured](value: T): Int\n  value.measure()\nend\n",
    );
    for i in 0..n {
        out.push_str(&format!(
            "final class C{i} implements Measured\n  type Item = Int\n  \
             def measure(self): Int\n    {i}\n  end\nend\n"
        ));
    }
    out.push_str("sum = 0\n");
    for i in 0..n {
        out.push_str(&format!("sum = sum + read(C{i}())\n"));
    }
    out.push_str("sum\n");
    out
}

/// One generated shape: a name, a source generator, and the sizes
/// the benchmarks run it at.
type Shape = (&'static str, fn(usize) -> String, Vec<usize>);

/// Every generated shape, for the checker and the verifier.
fn shapes() -> Vec<Shape> {
    vec![
        (
            "methods_and_chain",
            checker_source as fn(usize) -> String,
            vec![16, 64, 256, 1024],
        ),
        ("classes", class_source, vec![16, 64, 256]),
        ("inherit_chain", inherit_source, vec![16, 64, 256]),
        ("generics", generic_source, vec![16, 64, 256]),
        ("inference_chain", inference_source, vec![16, 64, 256]),
        ("enum_case_arms", enum_source, vec![16, 64, 256]),
        ("wide_body", wide_body_source, vec![64, 256, 1024]),
        ("interfaces", interface_source, vec![16, 64, 256]),
    ]
}

// ---------------------------------------------------------------
// Group 2: the bundled core image.
// ---------------------------------------------------------------

#[test]
#[ignore]
fn bench_core_compilation() {
    let mut compile_runs: Vec<Duration> = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let module = lm_hir::core_image();
        let elapsed = start.elapsed();
        std::hint::black_box(module.funcs.len());
        if round > 0 {
            compile_runs.push(elapsed);
        }
    }

    let module = lm_hir::core_image();
    let bytes = lm_bytecode::encode(&module);
    let mut load_runs: Vec<Duration> = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let loaded = lm_vm::load_bytes(&bytes).expect("the core image loads");
        let elapsed = start.elapsed();
        std::hint::black_box(loaded.dispatch_cells());
        if round > 0 {
            load_runs.push(elapsed);
        }
    }

    println!(
        "LOOM\tcore_compile\t{}\t{}\t{:.3}\tms",
        module.classes.len(),
        module.funcs.len(),
        median(compile_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tcore_load\t{}\t{}\t{:.3}\tms",
        bytes.len(),
        module.funcs.len(),
        median(load_runs).as_secs_f64() * 1e3
    );
}

#[test]
#[ignore]
fn bench_late_compilation() {
    use lm_compiler::{compile_module_with_options, CompileEnv, CompileOptions};
    use lm_source::SourceFile;

    let source = SourceFile::new("late-bench.lm", checker_source(256));
    let env = CompileEnv::new().freeze();
    let cases = [
        ("static_compile", CompileOptions::new()),
        ("late_compile", CompileOptions::new().late_definitions()),
    ];
    for (name, options) in cases {
        let mut runs = Vec::with_capacity(ROUNDS);
        for round in 0..=ROUNDS {
            let start = Instant::now();
            let compiled = compile_module_with_options("bench.late", &source, &env, true, &options)
                .expect("the late benchmark compiles");
            let elapsed = start.elapsed();
            std::hint::black_box(compiled.semantic_hash);
            if round > 0 {
                runs.push(elapsed);
            }
        }
        println!(
            "LOOM\t{name}\t256\t{:.3}\tms",
            median(runs).as_secs_f64() * 1e3
        );
    }
}

#[test]
#[ignore]
fn bench_public_syntax() {
    let mut source = String::from("value = 0\n");
    for _ in 1..5000 {
        source.push_str("value = value + 1\n");
    }

    let mut parse_runs = Vec::with_capacity(ROUNDS);
    let mut parsed = None;
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let result = lm_source::syntax::parse_public_syntax(&source);
        let elapsed = start.elapsed();
        assert_eq!(result.status, lm_source::syntax::ParseStatus::Complete);
        if round > 0 {
            parse_runs.push(elapsed);
        }
        parsed = Some(result);
    }
    let parsed = parsed.expect("one syntax parse completes");
    let view = lm_abi::syntax::SyntaxView::new(&parsed.records, source.len())
        .expect("the syntax records are valid");

    let part_source = "value = value + 1\n";
    let part = lm_source::syntax::parse_public_syntax(part_source);
    let part_view = lm_abi::syntax::SyntaxView::new(&part.records, part_source.len())
        .expect("the syntax records are valid");
    let part_root = part_view
        .record(part_view.root())
        .expect("the syntax root is valid");
    let statement = part_view.child(part_root, 0).expect("the statement exists");
    let parts: Vec<_> = (0..5000)
        .map(|_| lm_abi::syntax::SyntaxPart {
            source: part_source,
            records: &part.records,
            index: statement,
        })
        .collect();
    let mut construction_runs = Vec::with_capacity(ROUNDS);
    let mut built_count = 0u64;
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let built = lm_abi::syntax::build_syntax_node(lm_abi::syntax::KIND_MODULE, &parts)
            .expect("the syntax build completes");
        let elapsed = start.elapsed();
        let built_view = lm_abi::syntax::SyntaxView::new(&built.records, built.source.len())
            .expect("the built syntax records are valid");
        built_count = u64::from(built_view.item_count());
        std::hint::black_box(built);
        if round > 0 {
            construction_runs.push(elapsed);
        }
    }

    let mut traversal_runs = Vec::with_capacity(ROUNDS);
    let mut item_count = 0u64;
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let mut stack = vec![view.root()];
        let mut visited = 0u64;
        while let Some(index) = stack.pop() {
            let record = view.record(index).expect("the syntax item is valid");
            visited += 1;
            for offset in 0..record.child_len {
                stack.push(
                    view.child(record, offset)
                        .expect("the syntax child is valid"),
                );
            }
        }
        let elapsed = start.elapsed();
        std::hint::black_box(visited);
        item_count = visited;
        if round > 0 {
            traversal_runs.push(elapsed);
        }
    }

    let parse = median(parse_runs);
    let construction = median(construction_runs);
    let traversal = median(traversal_runs);
    println!(
        "LOOM\tsyntax_parse\t{}\t{:.1}\t{:.3}",
        item_count,
        parse.as_nanos() as f64 / item_count as f64,
        parse.as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tsyntax_construct\t{}\t{:.1}\t{:.3}",
        built_count,
        construction.as_nanos() as f64 / built_count as f64,
        construction.as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tsyntax_traverse\t{}\t{:.1}\t{:.3}",
        item_count,
        traversal.as_nanos() as f64 / item_count as f64,
        traversal.as_secs_f64() * 1e3
    );
}

#[test]
#[ignore]
fn bench_typechecking() {
    println!("LOOM\tshape\tn\tlines\tms\tlines_per_s");
    for (name, make, sizes) in shapes() {
        for n in sizes {
            let source = make(n);
            let lines = source.lines().count();
            let mut runs: Vec<Duration> = Vec::new();
            for round in 0..=ROUNDS {
                let start = Instant::now();
                let module = lm_testkit::compile_text("bench.lm", &source)
                    .unwrap_or_else(|e| panic!("the generated `{name}` must compile:\n{e}"));
                let elapsed = start.elapsed();
                std::hint::black_box(module.funcs.len());
                if round > 0 {
                    runs.push(elapsed);
                }
            }
            let ms = median(runs).as_secs_f64() * 1e3;
            println!(
                "LOOM\t{name}\t{n}\t{lines}\t{ms:.3}\t{:.0}",
                lines as f64 / (ms / 1e3)
            );
        }
    }
}

// ---------------------------------------------------------------
// Group 3: artifact verification.
// ---------------------------------------------------------------

#[test]
#[ignore]
fn bench_verification() {
    println!("LOOM\tcase\tbytes\tfuncs\tms\tmib_per_s");
    let mut cases: Vec<(String, String)> = vec![(
        "tiny".to_string(),
        "def f(n: Int): Int\n  n + 1\nend\nf(41)\n".to_string(),
    )];
    // Every generated shape at two sizes, so verification meets the
    // same variety the checker does.
    for (name, make, sizes) in shapes() {
        for n in [sizes[0], *sizes.last().expect("a shape has a size")] {
            cases.push((format!("{name}_{n}"), make(n)));
        }
    }
    for (name, source) in cases {
        let bytes = lm_testkit::compile_to_bytes("bench.lm", &source).expect("compiles");
        let module = lm_bytecode::decode(&bytes).expect("decodes");
        let mut runs: Vec<Duration> = Vec::new();
        for round in 0..=ROUNDS {
            let start = Instant::now();
            let result = lm_verify::verify_module(&module);
            let elapsed = start.elapsed();
            assert!(result.is_ok(), "{name} must verify");
            if round > 0 {
                runs.push(elapsed);
            }
        }
        let ms = median(runs).as_secs_f64() * 1e3;
        let mib = bytes.len() as f64 / (1024.0 * 1024.0);
        println!(
            "LOOM\tverify\t{}\t{}\t{ms:.4}\t{:.1}\t{name}",
            bytes.len(),
            module.funcs.len(),
            mib / (ms / 1e3)
        );
    }

    // The load path as a whole: decode, identity preflight, verify,
    // and the dispatch rows.
    println!("LOOM\tcase\tbytes\tms\tnote");
    for (name, source) in [
        (
            "load_tiny",
            "def f(n: Int): Int\n  n + 1\nend\nf(41)\n".to_string(),
        ),
        ("load_generated_256", checker_source(256)),
    ] {
        let bytes = lm_testkit::compile_to_bytes("bench.lm", &source).expect("compiles");
        let mut runs: Vec<Duration> = Vec::new();
        for round in 0..=ROUNDS {
            let start = Instant::now();
            let loaded = lm_vm::load_bytes(&bytes).expect("loads");
            let elapsed = start.elapsed();
            std::hint::black_box(loaded.dispatch_cells());
            if round > 0 {
                runs.push(elapsed);
            }
        }
        println!(
            "LOOM\t{name}\t{}\t{:.4}\tload_bytes",
            bytes.len(),
            median(runs).as_secs_f64() * 1e3
        );
    }
}

// ---------------------------------------------------------------
// Group 5: the filesystem effect.
//
// Every case runs under `CliHost`, the host `lm run` uses. A file
// operation there is asynchronous: the machine performs, the host
// sends the request to a worker thread, the thread makes the call,
// and the machine resumes on a later poll. CPython calls the same
// system calls directly.
//
// So a ratio here reads differently from the language table. It
// measures what the effect boundary costs above the call, not how
// fast the call is. Both sides run a warm page cache: the file is
// written, then a warm-up round reads it before any measured round.
// ---------------------------------------------------------------

/// The scratch directory of one filesystem case.
struct FsTree {
    root: std::path::PathBuf,
}

impl FsTree {
    fn new(label: &str) -> FsTree {
        let root = std::env::temp_dir().join(format!("lm-fs-bench-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the scratch directory is created");
        FsTree { root }
    }

    /// Write one file of `bytes` filler and return its path as text.
    fn file(&self, name: &str, bytes: usize) -> String {
        let path = self.root.join(name);
        std::fs::write(&path, vec![b'x'; bytes]).expect("the scratch file is written");
        path.display().to_string()
    }

    fn path(&self, name: &str) -> String {
        self.root.join(name).display().to_string()
    }
}

impl Drop for FsTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Time one filesystem program under the command-line host.
fn time_fs(source: &str, expected: &str) -> Duration {
    let bytes = lm_testkit::compile_to_bytes("fs-bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let loaded = lm_vm::load_bytes(&bytes).expect("the benchmark artifact must load");
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let host = Box::new(lm_host::CliHost::new(1));
        let mut world = lm_vm::World::new(&loaded, config(), host);
        world.allow("Fs").expect("the Fs grant exists");
        let start = Instant::now();
        let outcome = lm_proc::run_world(&mut world);
        let elapsed = start.elapsed();
        assert_eq!(
            world.show_outcome(&outcome),
            expected,
            "the case answered wrong"
        );
        if round > 0 {
            runs.push(elapsed);
        }
    }
    median(runs)
}

/// Time one filesystem program against the in-memory host.
///
/// `RecordingHost` defers a reply to a later poll exactly as
/// `CliHost` does, and it makes no system call and starts no worker
/// thread. The difference between the two hosts is therefore the cost
/// of the call and the thread, and what remains is the effect
/// boundary itself.
fn time_fs_memory(source: &str, file: &str, bytes: usize, expected: &str) -> Duration {
    let artifact = lm_testkit::compile_to_bytes("fs-bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let loaded = lm_vm::load_bytes(&artifact).expect("the benchmark artifact must load");
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let host = Rc::new(RefCell::new(lm_vm::RecordingHost::new(1)));
        host.borrow_mut().set_file(file, vec![b'x'; bytes]);
        let mut world = lm_vm::World::new(&loaded, config(), Box::new(host));
        world.allow("Fs").expect("the Fs grant exists");
        let start = Instant::now();
        let outcome = lm_proc::run_world(&mut world);
        let elapsed = start.elapsed();
        assert_eq!(
            world.show_outcome(&outcome),
            expected,
            "the case answered wrong"
        );
        if round > 0 {
            runs.push(elapsed);
        }
    }
    median(runs)
}

/// Report one case as throughput in mebibytes per second.
fn report_fs_throughput(name: &str, bytes: u64, source: &str, expected: &str) {
    let total = time_fs(source, expected);
    let mib = bytes as f64 / (1024.0 * 1024.0);
    println!(
        "LOOM\t{name}\t{bytes}\t{:.0}\t{:.3}",
        mib / total.as_secs_f64(),
        total.as_secs_f64() * 1e3
    );
}

/// Drop the page cache for one file. `posix_fadvise` needs no root.
///
/// A benchmark that writes a file and reads it back measures the page
/// cache and not the filesystem. The cold case evicts first, so the
/// device takes part.
fn evict_page_cache(path: &str) {
    let script = format!(
        "import os\nfd = os.open({path:?}, os.O_RDONLY)\n\
         os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)\nos.close(fd)\n"
    );
    let status = std::process::Command::new("python3")
        .arg("-c")
        .arg(script)
        .status()
        .expect("the eviction helper runs");
    assert!(status.success(), "the eviction helper failed");
}

/// Report one throughput case that evicts the page cache each round.
fn report_fs_cold(name: &str, bytes: u64, path: &str, source: &str, expected: &str) {
    let artifact = lm_testkit::compile_to_bytes("fs-bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let loaded = lm_vm::load_bytes(&artifact).expect("the benchmark artifact must load");
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=3 {
        evict_page_cache(path);
        let host = Box::new(lm_host::CliHost::new(1));
        let mut world = lm_vm::World::new(&loaded, config(), host);
        world.allow("Fs").expect("the Fs grant exists");
        let start = Instant::now();
        let outcome = lm_proc::run_world(&mut world);
        let elapsed = start.elapsed();
        assert_eq!(
            world.show_outcome(&outcome),
            expected,
            "the case answered wrong"
        );
        if round > 0 {
            runs.push(elapsed);
        }
    }
    let total = median(runs);
    println!(
        "LOOM\t{name}\t{bytes}\t{:.0}\t{:.3}",
        bytes as f64 / (1024.0 * 1024.0) / total.as_secs_f64(),
        total.as_secs_f64() * 1e3
    );
}

fn report_fs_memory(
    name: &str,
    iterations: u64,
    source: &str,
    file: &str,
    bytes: usize,
    expected: &str,
) {
    let total = time_fs_memory(source, file, bytes, expected);
    println!(
        "LOOM\t{name}\t{iterations}\t{:.0}\t{:.3}",
        total.as_nanos() as f64 / iterations as f64,
        total.as_secs_f64() * 1e3
    );
}

fn report_fs(name: &str, iterations: u64, source: &str, expected: &str) {
    let total = time_fs(source, expected);
    let per = total.as_nanos() as f64 / iterations as f64;
    println!(
        "LOOM\t{name}\t{iterations}\t{:.0}\t{:.3}",
        per,
        total.as_secs_f64() * 1e3
    );
}

/// The buffered line reader under test.
const READER: &str = r#"# A buffered line reader written in ordinary Loom code.
#
# The buffer is one Bytes value. A line is a slice of it, so a hit
# copies nothing and crosses no effect boundary. Only a refill
# performs `Fs.Read`, and the row says so.

class BufReader
  file: FileHandle
  buffer: Bytes
  eof: Bool

  def init(mut self, file: FileHandle)
    self.file = file
    self.buffer = "".bytes()
    self.eof = false
  end

  def read_line(mut self): Option[Bytes] with Fs.Read
    nl = "\n".bytes()
    out: Option[Bytes] = None
    going = true
    while going
      case self.buffer.find(nl)
      in Some(at)
        line = case self.buffer.slice(0, at) in Ok(b) then b in Err(_) then self.buffer end
        tail = self.buffer.len() - at - 1
        self.buffer = case self.buffer.slice(at + 1, tail) in Ok(b) then b in Err(_) then self.buffer end
        out = Some(line)
        going = false
      in None
        if self.eof
          if not self.buffer.is_empty()
            out = Some(self.buffer)
            self.buffer = "".bytes()
          end
          going = false
        else
          case self.file.read(65536)
          in Ok(chunk)
            if chunk.is_empty()
              self.eof = true
            else
              self.buffer = self.buffer + chunk
            end
          in Err(_)
            self.eof = true
          end
        end
      end
    end
    out
  end
end

def count_lines(path: String): Int with Fs.Open, Fs.Read, Fs.Close
  case sys.fs.open(path, ReadOnly)
  in Ok(f)
    r = BufReader(f)
    n = 0
    going = true
    while going
      case r.read_line()
      in Some(_)
        n = n + 1
      in None
        going = false
      end
    end
    f.close()
    n
  in Err(_) then -1
  end
end

"#;

#[test]
#[ignore]
fn bench_filesystem_operations() {
    println!("LOOM\tcase\titers\tns_per_op\ttotal_ms");
    let tree = FsTree::new("read");
    let data = tree.file("data.bin", 8 * 1024 * 1024);

    // The handle lifecycle alone: one open and one close.
    report_fs(
        "fs_open_close",
        2_000,
        &format!(
            "def go(): Int with Fs.Open, Fs.Close\n\
             \x20 n = 0\n  i = 0\n  while i < 2000\n\
             \x20   n = n + case sys.fs.open(\"{data}\", ReadOnly)\n\
             \x20   in Ok(f)\n      case f.close() in Ok(_) then 1 in Err(_) then 0 end\n\
             \x20   in Err(_) then 0\n    end\n    i = i + 1\n  end\n  n\nend\ngo()\n"
        ),
        "Done(2000)",
    );

    // One read of 1 KiB from an open handle. The file is large
    // enough that no read reaches its end.
    report_fs(
        "fs_read_1k",
        2_000,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{data}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 2000\n\
             \x20     n = n + case f.read(1024) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(2048000)",
    );

    // The same read at 64 KiB. The call does more work and the
    // boundary costs the same, so the ratio moves.
    report_fs(
        "fs_read_64k",
        100,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{data}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 100\n\
             \x20     n = n + case f.read(65536) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(6553600)",
    );

    // Read one whole small file: open, one read, close. This is the
    // shape a program writes most often.
    let small = tree.file("small.txt", 4096);
    report_fs(
        "fs_read_file",
        1_000,
        &format!(
            "def once(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{small}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = case f.read(8192) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20   f.close()\n    n\n  in Err(_) then 0\n  end\nend\n\
             def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 n = 0\n  i = 0\n  while i < 1000\n    n = n + once()\n    i = i + 1\n  end\n  n\nend\ngo()\n"
        ),
        "Done(4096000)",
    );

    // One write of 1 KiB to an open handle.
    let out = tree.path("out.bin");
    report_fs(
        "fs_write_1k",
        2_000,
        &format!(
            "def go(): Int with Fs.Open, Fs.Write, Fs.Close\n\
             \x20 case sys.fs.open(\"{out}\", CreateTruncate)\n\
             \x20 in Ok(f)\n    chunk = \"{}\".bytes()\n    n = 0\n    i = 0\n    while i < 2000\n\
             \x20     n = n + case f.write(chunk) in Ok(w) then w in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n",
            "y".repeat(1024)
        ),
        "Done(2048000)",
    );

    // The same 1 KiB read against the in-memory host. No system call
    // and no worker thread run, so this is the effect boundary alone.
    report_fs_memory(
        "fs_read_1k_memory",
        2_000,
        "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
         \x20 case sys.fs.open(\"mem.bin\", ReadOnly)\n\
         \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 2000\n\
         \x20     n = n + case f.read(1024) in Ok(b) then b.len() in Err(_) then 0 end\n\
         \x20     i = i + 1\n    end\n    f.close()\n    n\n\
         \x20 in Err(_) then 0\n  end\nend\ngo()\n",
        "mem.bin",
        8 * 1024 * 1024,
        "Done(2048000)",
    );

    // A buffered line reader written in ordinary Loom code. The
    // buffer is one Bytes value and a line is a slice of it, so a
    // buffer hit copies nothing and crosses no effect boundary. Only
    // a refill performs `Fs.Read`.
    let lines_path = tree.path("lines.txt");
    {
        let body = b"a short protocol line\n".repeat(200_000);
        std::fs::write(&lines_path, body).expect("the line file is written");
    }
    report_fs(
        "fs_read_lines",
        200_000,
        &format!("{}count_lines(\"{lines_path}\")\n", READER),
        "Done(200000)",
    );

    // The same reader with the line slice removed: one slice for
    // each line instead of two. The difference names what producing
    // one line value costs.
    report_fs(
        "fs_read_lines_advance",
        200_000,
        &format!(
            "{}count_lines(\"{lines_path}\")\n",
            READER.replace(
                "line = case self.buffer.slice(0, at) in Ok(b) then b in Err(_) then self.buffer end",
                "line = self.buffer"
            )
        ),
        "Done(200000)",
    );

    // Sequential throughput over a 64 MiB file, at two chunk sizes.
    // The unit is mebibytes per second, not nanoseconds.
    let big = tree.file("big.bin", 64 * 1024 * 1024);
    println!("LOOM\tcase\tbytes\tmib_per_s\ttotal_ms");
    report_fs_throughput(
        "fs_tput_read_64k",
        64 * 1024 * 1024,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{big}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 1024\n\
             \x20     n = n + case f.read(65536) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(67108864)",
    );
    report_fs_throughput(
        "fs_tput_read_1m",
        64 * 1024 * 1024,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{big}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 64\n\
             \x20     n = n + case f.read(1048576) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(67108864)",
    );
    let sink = tree.path("sink.bin");
    report_fs_throughput(
        "fs_tput_write_64k",
        64 * 1024 * 1024,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Write, Fs.Close\n\
             \x20 chunk = case sys.fs.open(\"{big}\", ReadOnly)\n\
             \x20 in Ok(src)\n    c = case src.read(65536) in Ok(b) then b in Err(_) then \"\".bytes() end\n\
             \x20   src.close()\n    c\n  in Err(_) then \"\".bytes()\n  end\n\
             \x20 case sys.fs.open(\"{sink}\", CreateTruncate)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 1024\n\
             \x20     n = n + case f.write(chunk) in Ok(w) then w in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(67108864)",
    );

    // The same read with the page cache evicted first. This is the
    // cost of loading a file, and the warm case above is the cost of
    // copying one out of memory.
    report_fs_cold(
        "fs_tput_read_cold",
        64 * 1024 * 1024,
        &big,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{big}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 64\n\
             \x20     n = n + case f.read(1048576) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(67108864)",
    );

    // The handle lifecycle against the in-memory host.
    report_fs_memory(
        "fs_open_close_memory",
        2_000,
        "def go(): Int with Fs.Open, Fs.Close\n\
         \x20 n = 0\n  i = 0\n  while i < 2000\n\
         \x20   n = n + case sys.fs.open(\"mem.bin\", ReadOnly)\n\
         \x20   in Ok(f)\n      case f.close() in Ok(_) then 1 in Err(_) then 0 end\n\
         \x20   in Err(_) then 0\n    end\n    i = i + 1\n  end\n  n\nend\ngo()\n",
        "mem.bin",
        4096,
        "Done(2000)",
    );
}
