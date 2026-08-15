//! Week-1 gate tests: deep guest recursion without Rust-stack growth,
//! fuel exhaustion, heap-cap faults, and deterministic output.

use lm_testkit::{compile_to_bytes, run_text};
use lm_vm::VmConfig;

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
    std::thread::Builder::new()
        .stack_size(SMALL_RUST_STACK)
        .spawn(move || run_text("gate.lm", source, config).expect("compiles"))
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
    let source = "i = 0\nwhile true\n  i = i + 1\n  i = i - 1\nend\ni\n";
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
fn string_allocation_faults_with_heap_limit_under_a_small_cap() {
    let source = "i = 0\nh = \"start\"\nwhile i < 100000\n  h = \"chunk\"\n  \
                  i = i + 1\nend\ni\n";
    let config = VmConfig {
        heap_bytes: 1024,
        ..VmConfig::default()
    };
    assert_eq!(
        run_text("heap.lm", source, config).unwrap(),
        "Fault(HeapLimit)"
    );
}

#[test]
fn compilation_is_deterministic() {
    let source =
        std::fs::read_to_string(lm_testkit::repo_root().join("examples/01-basics/factorial.lm"))
            .unwrap();
    let a = compile_to_bytes("factorial.lm", &source).unwrap();
    let b = compile_to_bytes("factorial.lm", &source).unwrap();
    assert_eq!(a, b, "bytecode bytes differ between compilations");
    let run_a = run_text("factorial.lm", &source, VmConfig::default()).unwrap();
    let run_b = run_text("factorial.lm", &source, VmConfig::default()).unwrap();
    assert_eq!(run_a, run_b);
}

#[test]
fn diagnostics_are_deterministic() {
    let source = "x = 1\nx = \"two\"\n";
    let a = lm_testkit::compile_text("t.lm", source).unwrap_err();
    let b = lm_testkit::compile_text("t.lm", source).unwrap_err();
    assert_eq!(a, b);
    assert_eq!(
        a,
        "error[E1004]: expected Int, found String\n  --> t.lm:2:5\n"
    );
}
