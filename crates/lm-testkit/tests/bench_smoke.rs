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
