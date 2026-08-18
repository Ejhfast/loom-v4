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

/// Report one case that runs inside a `World`.
///
/// The cases above build a bare `Vm`. Every tool builds a `World`,
/// and a `World` adds the aggregate ledgers and the activation loop.
/// A program with no proc runs one machine there, so this case
/// measures the path a plain `lm run` takes.
fn report_world(name: &str, iterations: u64, source: &str, expected: &str) {
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
        "k = 7\ni = 0\ns = 0\nwhile i < 1000000\n  f = do |x: Int|: Int x + k end\n  s = f(s)\n  i = i + 1\nend\ns\n",
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

    // The string builder: the growable path the String methods will
    // use once specification 24.6 lands.
    report(
        "string_builder",
        500_000,
        "b = StringBuilder()\ni = 0\nwhile i < 500000\n  b.append(\"x\")\n  i = i + 1\nend\nb.build()\n",
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
                  in Fault(_) then 0 - 1\n\
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
    ]
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
