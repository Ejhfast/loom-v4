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
                  last = sys.clock.Now()\n    i = i + 1\n  end\n  i\nend\ngo()\n";
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
                  last = sys.clock.Monotonic()\n    i = i + 1\n  end\n  i\nend\ngo()\n";
    timed_world("perform_group_pass_20k", source, &["Clock"], "Done(20000)");
}

#[test]
fn perform_block_smoke() {
    // Each child performs one blocked operation; the holder observes
    // the fault. This times the block path plus machine creation.
    let source = "def one(): String with Vm\n  \
                  vm = sys.vm.Vm().from_object(do || with Io.Print\n    sys.io.Print(\"x\")\n  end, args: ())\n  \
                  case vm.run()\n  in Done(_) then \"done\"\n  in Fault(f) then f.code()\n  end\nend\n\
                  def go(): Int with Vm\n  i = 0\n  while i < 300\n    one()\n    i = i + 1\n  end\n  i\nend\ngo()\n";
    timed_world("perform_block_300", source, &["Vm"], "Done(300)");
}

#[test]
fn perform_mock_smoke() {
    let source = "def go(): Int with Vm\n  \
                  vm = sys.vm.Vm().from_object(do || with Clock.Now\n    \
                  i = 0\n    total = 0\n    while i < 5000\n      total = total + sys.clock.Now()\n      i = i + 1\n    end\n    total\n  end, args: ())\n  \
                  vm.table().mock(Clock.Now, do ||: Int 1 end)\n  \
                  case vm.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\nend\ngo()\n";
    timed_world("perform_mock_5k", source, &["Vm"], "Done(5000)");
}

#[test]
fn drive_interception_smoke() {
    let source = "def go(): Int with Vm\n  \
                  vm = sys.vm.Vm().from_object(do || with Clock.Now\n    \
                  i = 0\n    total = 0\n    while i < 5000\n      total = total + sys.clock.Now()\n      i = i + 1\n    end\n    total\n  end, args: ())\n  \
                  guard = 0\n  while guard < 100000\n    guard = guard + 1\n    \
                  case vm.drive()\n    in Asked(q)\n      \
                  case q.as_call(sys.clock.Now)\n      in Some(call) then vm.answer(call, 1)\n      in None then vm.dispatch(q)\n      end\n    \
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
                  i = 0\n    while i < 50\n      sys.clock.Sleep(1)\n      i = i + 1\n    end\n    i\n  end, args: ())\n  \
                  vm.table().pass(Clock.Sleep)\n  \
                  case vm.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\nend\ngo()\n";
    timed_world("async_wait_50", source, &["Vm", "Clock.Sleep"], "Done(50)");
}
