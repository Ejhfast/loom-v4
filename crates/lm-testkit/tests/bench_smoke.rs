//! Micro-benchmark smoke checks for the week-2 hot paths.
//!
//! These are not real benchmarks. They time a fixed workload on the
//! production path, print the duration, and assert completion. Real
//! benchmark infrastructure stays deferred; see docs/notes/week2.md.

use lm_testkit::run_text;
use lm_vm::VmConfig;
use std::time::Instant;

fn timed(name: &str, source: &str, expected: &str) {
    let start = Instant::now();
    let outcome = run_text("bench.lm", source, VmConfig::default()).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(outcome, expected, "{name}");
    eprintln!("bench-smoke {name}: {elapsed:?}");
}

#[test]
fn list_push_smoke() {
    let source = "xs: [Int] = []\ni = 0\nwhile i < 100000\n  xs.push(i)\n  i = i + 1\nend\n\
                  xs.len()\n";
    timed("list_push_100k", source, "Done(100000)");
}

#[test]
fn field_and_virtual_call_smoke() {
    let source = "class Counter\n  value: Int = 0\n  def add(mut self, n: Int): Int\n    \
                  self.value = self.value + n\n    self.value\n  end\nend\n\
                  c = Counter()\ni = 0\nwhile i < 100000\n  c.add(1)\n  i = i + 1\nend\n\
                  c.value\n";
    timed("virtual_call_100k", source, "Done(100000)");
}

#[test]
fn map_put_smoke() {
    let source = "m: {Int: Int} = {}\ni = 0\nwhile i < 300\n  m.put(i, i)\n  i = i + 1\nend\n\
                  m.len()\n";
    timed("map_put_300", source, "Done(300)");
}

#[test]
fn allocation_and_collection_smoke() {
    let source = "i = 0\nwhile i < 100000\n  xs = [1, 2, 3, 4]\n  i = i + 1\nend\ni\n";
    let start = Instant::now();
    let config = VmConfig {
        heap_bytes: 256 * 1024,
        ..VmConfig::default()
    };
    let outcome = run_text("bench.lm", source, config).unwrap();
    eprintln!("bench-smoke alloc_gc_100k: {:?}", start.elapsed());
    assert_eq!(outcome, "Done(100000)");
}

/// Map insertions at two scales. The index keeps insertion near
/// linear; the printed timings document the shape, and the assert
/// covers completion only, because wall-clock ratios are flaky in CI.
#[test]
fn map_insertion_scaling_smoke() {
    for count in [4_000, 32_000] {
        let source = format!(
            "m: {{Int: Int}} = {{}}\ni = 0\nwhile i < {count}\n  m.put(i, i)\n  i = i + 1\nend\n\
             m.len()\n"
        );
        let start = Instant::now();
        let outcome = run_text("bench.lm", &source, VmConfig::default()).unwrap();
        eprintln!("bench-smoke map_insert_{count}: {:?}", start.elapsed());
        assert_eq!(outcome, format!("Done({count})"));
    }
}

/// A loop that loads one string literal many times must not allocate
/// one object per load: literals intern per machine. The live object
/// count after the run is the structural assert.
#[test]
fn literal_loop_allocation_smoke() {
    let source = "i = 0\ns = \"\"\nwhile i < 200000\n  s = \"hello\"\n  i = i + 1\nend\ni\n";
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source).unwrap();
    let loaded = lm_vm::load_bytes(&bytes).unwrap();
    let start = Instant::now();
    let mut vm = lm_vm::Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    eprintln!("bench-smoke literal_loop_200k: {:?}", start.elapsed());
    assert_eq!(vm.show_outcome(&outcome), "Done(200000)");
    let live = vm.heap().live_count();
    assert!(
        live < 16,
        "the literal loop leaked {live} objects; literals must intern"
    );
}

/// Many classes with distinct selectors must not cost a dense
/// classes-times-selectors dispatch table. The cell count is the
/// structural assert.
#[test]
fn many_class_dispatch_memory_smoke() {
    let classes = 300;
    let mut source = String::new();
    for i in 0..classes {
        source.push_str(&format!(
            "class C{i}\n  def m{i}(self): Int\n    {i}\n  end\nend\n"
        ));
    }
    source.push_str("C7().m7()\n");
    let start = Instant::now();
    let bytes = lm_testkit::compile_to_bytes("bench.lm", &source).unwrap();
    let loaded = lm_vm::load_bytes(&bytes).unwrap();
    eprintln!("bench-smoke many_class_load_300: {:?}", start.elapsed());
    let cells = loaded.dispatch_cells();
    let selectors = loaded.module().selectors.len();
    let dense = loaded.module().classes.len() * selectors;
    eprintln!("bench-smoke dispatch cells {cells} (dense equivalent {dense})");
    // Every class answers its own selector plus the shared core
    // selectors, so the cell count stays near the method count.
    assert!(
        cells < dense / 10,
        "the dispatch table is near dense: {cells} of {dense}"
    );
    let mut vm = lm_vm::Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(7)");
}

/// Time one effectful workload in a world with grants.
fn timed_world(name: &str, source: &str, allow: &[&str], expected: &str) {
    let start = Instant::now();
    let outcome = lm_testkit::run_allowed("bench.lm", source, allow).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(outcome, expected, "{name}");
    eprintln!("bench-smoke {name}: {elapsed:?}");
}

#[test]
fn perform_exact_pass_smoke() {
    let source = "def go(): Int with Clock.Now\n  i = 0\n  last = 0\n  while i < 20000\n    \
                  last = sys.clock.now()\n    i = i + 1\n  end\n  i\nend\ngo()\n";
    timed_world(
        "perform_exact_pass_20k",
        source,
        &["Clock.Now"],
        "Done(20000)",
    );
}

#[test]
fn perform_group_pass_smoke() {
    let source = "def go(): Int with Clock\n  i = 0\n  last = 0\n  while i < 20000\n    \
                  last = sys.clock.monotonic()\n    i = i + 1\n  end\n  i\nend\ngo()\n";
    timed_world("perform_group_pass_20k", source, &["Clock"], "Done(20000)");
}

/// The week-6 build path: identity, the linker, and the cached load.
/// The printed timings document the shape of the load path, where
/// identity now runs once per distinct module instead of once per
/// load.
#[test]
fn build_and_link_smoke() {
    use lm_compiler::{compile_module, link, CompileEnv, LinkEnv, LinkUnit};
    use lm_source::SourceFile;
    let library =
        "class Cell\n  value: Int = 0\n  def get(self): Int\n    self.value\n  end\nend\n\
                   def make(n: Int): Cell\n  c = Cell()\n  c\nend\n";
    let program = "use lib.cell\n\ndef run(): Int\n  c = cell.make(1)\n  c.get()\nend\n\nrun()\n";
    let start = Instant::now();
    let lib = compile_module(
        "lib.cell",
        &SourceFile::new("cell.lm", library.to_string()),
        &CompileEnv::new().freeze(),
        false,
    )
    .expect("compiles");
    let mut env = CompileEnv::new();
    env.bind_interface(lib.interface.clone()).expect("binds");
    env.bind_root("lib", "lib").expect("binds");
    let main = compile_module(
        "app.main",
        &SourceFile::new("main.lm", program.to_string()),
        &env.freeze(),
        true,
    )
    .expect("compiles");
    let compiled = start.elapsed();
    let start = Instant::now();
    let mut link_env = LinkEnv::new();
    for unit in [&lib, &main] {
        link_env
            .bind(LinkUnit {
                path: unit.path.clone(),
                module: unit.module.clone(),
                interface: unit.interface.clone(),
            })
            .expect("binds");
    }
    let linked = link("app.main", &link_env.freeze()).expect("links");
    let linked_time = start.elapsed();
    let start = Instant::now();
    let mut cache = lm_vm::VerifiedCache::new();
    for _ in 0..20 {
        lm_vm::load_bytes_cached(&linked.artifact, &mut cache).expect("loads");
    }
    let loads = start.elapsed();
    assert_eq!(cache.verifications, 1);
    eprintln!(
        "bench-smoke build_2_modules: {compiled:?} link: {linked_time:?} \
         20_cached_loads: {loads:?} bytes: {}",
        linked.artifact.len()
    );
}

#[test]
fn perform_block_smoke() {
    // Each child performs one blocked operation; the holder observes
    // the fault. This times the block path plus machine creation.
    let source = "def one(): String with Vm\n  \
                  vm = sys.vm.Vm().from_object(do || with Io.Print\n    sys.io.print(\"x\")\n  end, args: ())\n  \
                  case vm.run()\n  in Done(_) then \"done\"\n  in Fault(f) then f.code()\n  end\nend\n\
                  def go(): Int with Vm\n  i = 0\n  while i < 300\n    one()\n    i = i + 1\n  end\n  i\nend\ngo()\n";
    timed_world("perform_block_300", source, &["Vm"], "Done(300)");
}

#[test]
fn perform_mock_smoke() {
    let source = "def go(): Int with Vm\n  \
                  vm = sys.vm.Vm().from_object(do || with Clock.Now\n    \
                  i = 0\n    total = 0\n    while i < 5000\n      total = total + sys.clock.now()\n      i = i + 1\n    end\n    total\n  end, args: ())\n  \
                  vm.table().mock(Clock.Now, do ||: Int 1 end)\n  \
                  case vm.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\nend\ngo()\n";
    timed_world("perform_mock_5k", source, &["Vm"], "Done(5000)");
}

#[test]
fn drive_interception_smoke() {
    let source = "def go(): Int with Vm\n  \
                  vm = sys.vm.Vm().from_object(do || with Clock.Now\n    \
                  i = 0\n    total = 0\n    while i < 5000\n      total = total + sys.clock.now()\n      i = i + 1\n    end\n    total\n  end, args: ())\n  \
                  guard = 0\n  while guard < 100000\n    guard = guard + 1\n    \
                  case vm.drive()\n    in Asked(q)\n      \
                  case q.as_call(Clock.Now)\n      in Some(call) then vm.answer(call, 1)\n      in None then vm.dispatch(q)\n      end\n    \
                  in Done(v)\n      return v\n    in Fault(_)\n      return 0 - 1\n    end\n  end\n  0 - 2\nend\ngo()\n";
    timed_world("drive_interception_5k", source, &["Vm"], "Done(5000)");
}

#[test]
fn nested_vm_run_smoke() {
    let source = "def tower(n: Int): Int with Vm\n  if n <= 0\n    1\n  else\n    \
                  vm = sys.vm.Vm().from_object(do || with Vm\n      tower(n - 1)\n    end, args: ())\n    \
                  vm.table().pass(Vm)\n    \
                  case vm.run()\n    in Done(v) then v + 1\n    in Fault(_) then 0 - 1\n    end\n  end\nend\ntower(40)\n";
    timed_world("nested_vm_run_40", source, &["Vm"], "Done(41)");
}

#[test]
fn async_wait_smoke() {
    let source = "def go(): Int with Vm, Clock.Sleep\n  \
                  vm = sys.vm.Vm().from_object(do || with Clock.Sleep\n    \
                  i = 0\n    while i < 50\n      sys.clock.sleep(1)\n      i = i + 1\n    end\n    i\n  end, args: ())\n  \
                  vm.table().pass(Clock.Sleep)\n  \
                  case vm.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\nend\ngo()\n";
    timed_world("async_wait_50", source, &["Vm", "Clock.Sleep"], "Done(50)");
}

// ---------------------------------------------------------------
// Week-7 graph paths.
// ---------------------------------------------------------------

#[test]
fn deep_freeze_smoke() {
    // The freeze mode walks a 50,000-node chain and sets the bits
    // after the whole graph validates.
    let source = "xs: [[Int]] = []\ni = 0\nwhile i < 50000\n  xs.push([i])\n  i = i + 1\nend\n\
                  xs.freeze()\nxs.len()\n";
    timed("freeze_chain_50k", source, "Done(50000)");
}

#[test]
fn boundary_transfer_smoke() {
    // A 20,000-node frozen graph crosses one machine boundary.
    let source = "def go(): Int with Vm\n  \
                  vm = sys.vm.Vm().from_object(do ||: [[Int]]\n    \
                  xs: [[Int]] = []\n    i = 0\n    while i < 20000\n      xs.push([i])\n      i = i + 1\n    end\n    \
                  xs.freeze()\n  end, args: ())\n  \
                  case vm.run()\n  in Done(xs) then xs.len()\n  in Fault(_) then 0 - 1\n  end\nend\ngo()\n";
    timed_world("transfer_graph_20k", source, &["Vm"], "Done(20000)");
}

#[test]
fn digest_smoke() {
    // The digest walks and encodes a 20,000-node frozen graph once.
    // The second call reads the cache on the frozen root.
    let source = "xs: [[Int]] = []\ni = 0\nwhile i < 20000\n  xs.push([i])\n  i = i + 1\nend\n\
                  xs.freeze()\n\
                  d = xs.digest()\n\
                  same = 0\nj = 0\nwhile j < 1000\n  \
                  if d == xs.digest()\n    same = same + 1\n  else\n    same = same\n  end\n  \
                  j = j + 1\nend\nsame\n";
    timed("digest_graph_20k_plus_1k_cached", source, "Done(1000)");
}

#[test]
fn collection_after_the_graph_engine_smoke() {
    // The mark mode runs under a small cap, so the workload collects
    // many times over one reusable identity table.
    let source = "i = 0\ntotal = 0\nwhile i < 100000\n  junk = [i, i, i]\n  \
                  total = total + junk.len()\n  i = i + 1\nend\ntotal\n";
    let start = Instant::now();
    let config = VmConfig {
        heap_bytes: 256 * 1024,
        ..VmConfig::default()
    };
    let outcome = run_text("bench.lm", source, config).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(outcome, "Done(300000)");
    eprintln!("bench-smoke mark_sweep_100k_under_256k: {elapsed:?}");
}
