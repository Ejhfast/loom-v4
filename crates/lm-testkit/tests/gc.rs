//! Collector gate tests on the production guest path: deep chains
//! without Rust-stack recursion, bounded object tables under churn,
//! and value preservation across many collections.

use lm_bytecode::artifact::Artifact;
use lm_testkit::{compile_text, publish_artifact};
use lm_vm::{Vm, VmConfig};

fn run_vm(source: &str, config: VmConfig) -> (String, lm_vm::HeapStats) {
    let artifact = compile_text("gc.lm", source).unwrap();
    run_artifact(&artifact, config)
}

fn run_artifact(artifact: &Artifact, config: VmConfig) -> (String, lm_vm::HeapStats) {
    let (arena, namespace) = publish_artifact(artifact).expect("the artifact publishes");
    let mut vm = Vm::new(arena, namespace, config);
    let outcome = vm.run();
    (vm.show_outcome(&outcome), vm.heap().stats())
}

const DEEP_CHAIN: &str = "class Node
  value: Int

  def init(mut self, value: Int)
    self.value = value
  end
end

class Link < Node
  next: Node

  def init(mut self, value: Int, next: Node)
    super.init(value)
    self.next = next
  end
end

head: Node = Node(0)
i = 0
while i < 100000
  head = Link(i, head)
  i = i + 1
end
head.freeze()
keep = head.value
head = Node(0)
j = 0
while j < 60000
  s = [j]
  j = j + 1
end
keep
";

#[test]
fn deep_chain_is_traced_and_frozen_without_rust_recursion() {
    // The chain is 100,000 links deep. The guest freezes it, drops it,
    // and then churns garbage under a cap that forces collections over
    // the dead chain. A small Rust stack proves the tracer and the
    // freezer are iterative.
    let artifact = compile_text("gc.lm", DEEP_CHAIN).unwrap();
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            let config = VmConfig {
                heap_bytes: 8 << 20,
                ..VmConfig::default()
            };
            run_artifact(&artifact, config)
        })
        .expect("thread starts");
    let (outcome, stats) = handle.join().expect("no Rust stack overflow");
    assert_eq!(outcome, "Done(99999)");
    assert!(stats.collections >= 1, "no collection ran: {stats:?}");
    // The dead 100,001-object chain was reclaimed: its slots moved to
    // the free list and the charged bytes dropped far below the chain
    // size.
    assert!(stats.free > 100_000, "chain leaked: {stats:?}");
    assert!(stats.used_bytes < 4 << 20, "chain bytes leaked: {stats:?}");
}

#[test]
fn object_table_stays_bounded_under_allocation_churn() {
    // 50,000 dropped lists recycle a bounded slot set through the free
    // list. An unbounded table would grow past 50,000 slots.
    let source = "i = 0\nwhile i < 50000\n  xs = [1, 2, 3]\n  i = i + 1\nend\ni\n";
    let config = VmConfig {
        heap_bytes: 64 * 1024,
        ..VmConfig::default()
    };
    let (outcome, stats) = run_vm(source, config);
    assert_eq!(outcome, "Done(50000)");
    assert!(
        stats.collections > 5,
        "expected many collections: {stats:?}"
    );
    assert!(stats.slots < 4096, "object table leaked slots: {stats:?}");
    assert!(stats.used_bytes <= 64 * 1024, "cap exceeded: {stats:?}");
}

#[test]
fn object_table_returns_to_baseline_after_a_collection() {
    // Direct heap-level baseline check on the same heap type the VM
    // uses. The count returns to the rooted baseline after a sweep.
    let mut heap = lm_vm::Heap::new(1 << 20);
    let keep = heap.alloc(lm_vm::Object::Str("keep".into()));
    let baseline = heap.live_count();
    for i in 0..1000 {
        heap.alloc(lm_vm::Object::Str(format!("garbage {i}").into()));
    }
    assert_eq!(heap.live_count(), baseline + 1000);
    lm_graph::collect(&mut heap, [keep]);
    assert_eq!(heap.live_count(), baseline);
}

#[test]
fn reachable_data_stays_correct_across_many_collections() {
    // The map stays live while list garbage churns under a small
    // cap. Every collection must keep the map graph intact. The
    // churn uses list literals, because string literals intern.
    let source = "counts: {String: Int} = {}\ncounts.put(\"red\", 0)\n\
                  counts.put(\"blue\", 0)\ni = 0\nwhile i < 20000\n  \
                  word = if i % 2 == 0\n    \"red\"\n  else\n    \"blue\"\n  end\n  \
                  junk = [i, i]\n  \
                  counts.put(word, counts.at(word) + 1)\n  i = i + 1\nend\ncounts\n";
    let config = VmConfig {
        heap_bytes: 32 * 1024,
        ..VmConfig::default()
    };
    let (outcome, stats) = run_vm(source, config);
    assert_eq!(outcome, "Done({\"red\": 10000, \"blue\": 10000})");
    assert!(stats.collections > 3, "expected collections: {stats:?}");
}

#[test]
fn closure_captures_survive_collections() {
    // The captured list is only reachable through the closure object
    // after the outer local rebinds.
    let source = "xs = [40, 2]\nf = { ||: Int xs.at(0) + xs.at(1) }\nxs = [0]\n\
                  i = 0\nwhile i < 20000\n  s = [i, i]\n  i = i + 1\nend\nf()\n";
    let config = VmConfig {
        heap_bytes: 16 * 1024,
        ..VmConfig::default()
    };
    let (outcome, stats) = run_vm(source, config);
    assert_eq!(outcome, "Done(42)");
    assert!(stats.collections > 3, "expected collections: {stats:?}");
}
