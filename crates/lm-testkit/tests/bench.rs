//! The benchmark suite.
//!
//! Three groups run here: the language operations, the type checker,
//! and artifact verification. Every case is `#[ignore]`, so the
//! ordinary suite never pays for them. Run them with:
//!
//! ```text
//! nix-shell --run "cargo test --release -p lm-testkit --test bench \
//!   -- --ignored --nocapture"
//! ```
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

/// Report one case: the per-operation cost above the baseline.
fn report(name: &str, iterations: u64, source: &str, base: Duration) {
    let total = time_program(source);
    let work = total.saturating_sub(base);
    let per = work.as_nanos() as f64 / iterations as f64;
    println!(
        "LOOM\t{name}\t{iterations}\t{:.1}\t{:.3}",
        per,
        total.as_secs_f64() * 1e3
    );
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
        "i = 0\ns = 0\nwhile i < 1000000\n  f = do |x: Int|: Int x + 1 end\n  s = f(s)\n  i = i + 1\nend\ns\n",
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

    // String interpolation: format one integer into a fresh short
    // string. The String method surface of specification 24.6 is not
    // implemented yet, so interpolation is the one string workload
    // available. Accumulating instead would measure quadratic copying.
    report(
        "string_interp",
        200_000,
        "s = \"\"\ni = 0\nwhile i < 200000\n  s = \"v{i}\"\n  i = i + 1\nend\ns\n",
        base,
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

#[test]
#[ignore]
fn bench_typechecking() {
    println!("LOOM\tcase\tdefs\tlines\tms\tlines_per_s");
    for n in [16usize, 64, 256, 1024] {
        let source = checker_source(n);
        let lines = source.lines().count();
        let mut runs: Vec<Duration> = Vec::new();
        for round in 0..=ROUNDS {
            let start = Instant::now();
            let module = lm_testkit::compile_text("bench.lm", &source)
                .unwrap_or_else(|e| panic!("the generated source must compile:\n{e}"));
            let elapsed = start.elapsed();
            std::hint::black_box(module.funcs.len());
            if round > 0 {
                runs.push(elapsed);
            }
        }
        let ms = median(runs).as_secs_f64() * 1e3;
        println!(
            "LOOM\tcheck_and_lower\t{n}\t{lines}\t{ms:.3}\t{:.0}",
            lines as f64 / (ms / 1e3)
        );
    }
}

// ---------------------------------------------------------------
// Group 3: artifact verification.
// ---------------------------------------------------------------

#[test]
#[ignore]
fn bench_verification() {
    println!("LOOM\tcase\tbytes\tfuncs\tms\tmib_per_s");
    let cases: Vec<(&str, String)> = vec![
        (
            "tiny",
            "def f(n: Int): Int\n  n + 1\nend\nf(41)\n".to_string(),
        ),
        (
            "class_small",
            "class Counter\n  value: Int = 0\n  \
          def add(mut self, n: Int): Int\n    self.value = self.value + n\n    self.value\n  end\n\
          end\nc = Counter()\nc.add(1)\n"
                .to_string(),
        ),
        ("generated_64", checker_source(64)),
        ("generated_256", checker_source(256)),
        ("generated_1024", checker_source(1024)),
    ];
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
