//! Gate tests: deep guest recursion without Rust-stack growth, fuel
//! exhaustion, heap-cap behavior with the collector, and deterministic
//! output.

use lm_testkit::{compile_to_bytes, run_text};
use lm_vm::{Vm, VmConfig};

const DEEP_RECURSION: &str = "def down(n: Int): Int
  if n <= 0
    0
  else
    down(n - 1)
  end
end

down(100000)
";

/// A small Rust stack for the gate. A recursive interpreter would
/// overflow this stack in a debug build long before 100,000 frames.
const SMALL_RUST_STACK: usize = 256 * 1024;

fn run_on_small_stack(source: &'static str, config: VmConfig) -> String {
    let bytes = compile_to_bytes("gate.lm", source).expect("compiles");
    let (arena, namespace) = lm_testkit::publish_artifact_bytes(&bytes).expect("loads");
    std::thread::Builder::new()
        .stack_size(SMALL_RUST_STACK)
        .spawn(move || {
            let mut vm = Vm::new(arena, namespace, config);
            let outcome = vm.run();
            vm.show_outcome(&outcome)
        })
        .expect("thread starts")
        .join()
        .expect("no Rust stack overflow and no panic")
}

#[test]
fn deep_recursion_completes_without_rust_stack_growth() {
    let config = VmConfig {
        max_frames: 200_000,
        ..VmConfig::default()
    };
    assert_eq!(run_on_small_stack(DEEP_RECURSION, config), "Done(0)");
}

#[test]
fn deep_recursion_faults_with_stack_limit_under_a_low_frame_cap() {
    let config = VmConfig {
        max_frames: 50_000,
        ..VmConfig::default()
    };
    assert_eq!(
        run_on_small_stack(DEEP_RECURSION, config),
        "Fault(StackLimit)"
    );
}

#[test]
fn infinite_loop_faults_with_out_of_fuel() {
    let source = "i = 0\nwhile true\n  i = i + 1\n  i = i - 1\nend\n";
    let config = VmConfig {
        fuel: 10_000,
        ..VmConfig::default()
    };
    assert_eq!(
        run_text("fuel.lm", source, config).unwrap(),
        "Fault(OutOfFuel)"
    );
}

#[test]
fn unreachable_garbage_does_not_fault_under_a_small_cap() {
    // Each pass of the loop drops the previous string. The collector
    // reclaims the garbage, so a tiny cap completes.
    let source = "i = 0\nh = \"start\"\nwhile i < 100000\n  h = \"chunk\"\n  \
                  i = i + 1\nend\ni\n";
    let config = VmConfig {
        heap_bytes: 1024,
        ..VmConfig::default()
    };
    assert_eq!(run_text("heap.lm", source, config).unwrap(), "Done(100000)");
}

#[test]
fn live_data_past_the_cap_faults_with_heap_limit() {
    // The list keeps every string alive, so collection frees nothing.
    let source = "xs: [String] = []\ni = 0\nwhile i < 100000\n  xs.push(\"chunk\")\n  \
                  i = i + 1\nend\nxs.len()\n";
    let config = VmConfig {
        heap_bytes: 4096,
        ..VmConfig::default()
    };
    assert_eq!(
        run_text("heap.lm", source, config).unwrap(),
        "Fault(HeapLimit)"
    );
}

#[test]
fn short_lived_data_collects_before_the_hard_heap_limit() {
    let source = "i = 0\ntext = \"\"\nwhile i < 100000\n  text = \"value #{i}\"\n  \
                  i = i + 1\nend\ntext.len()\n";
    let bytes = compile_to_bytes("heap-pace.lm", source).expect("the source compiles");
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the artifact loads");
    let mut world = lm_vm::World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(lm_vm::RecordingHost::new(1)),
    );
    let outcome = world.run_root();
    assert_eq!(world.show_outcome(&outcome), "Done(11)");
    assert!(world.execution_metrics().collections > 0);
    assert!(world.heap_of(0).stats().used_bytes < 8 << 20);
}

#[test]
fn compilation_is_deterministic() {
    for example in [
        "examples/01-basics/factorial.lm",
        "examples/02-objects/counter.lm",
        "examples/02-objects/counts.lm",
        "examples/02-objects/closures.lm",
    ] {
        let source = std::fs::read_to_string(lm_testkit::repo_root().join(example)).unwrap();
        let a = compile_to_bytes(example, &source).unwrap();
        let b = compile_to_bytes(example, &source).unwrap();
        assert_eq!(a, b, "bytecode bytes differ for {example}");
        let run_a = run_text(example, &source, VmConfig::default()).unwrap();
        let run_b = run_text(example, &source, VmConfig::default()).unwrap();
        assert_eq!(run_a, run_b);
    }
}

#[test]
fn diagnostics_are_deterministic() {
    let source = "x = 1\nx = \"two\"\n";
    let a = lm_testkit::compile_module_text("t.lm", source).unwrap_err();
    let b = lm_testkit::compile_module_text("t.lm", source).unwrap_err();
    assert_eq!(a, b);
    assert_eq!(
        a,
        "error[E1004]: expected Int, found String\n  --> t.lm:2:5\n"
    );
}
