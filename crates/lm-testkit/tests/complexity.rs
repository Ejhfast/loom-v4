//! Checker complexity gates: pathological inputs must complete in
//! bounded time without exponential search. Each case asserts a
//! generous wall-clock bound that an exponential algorithm would
//! break by orders of magnitude.

use lm_testkit::{compile_text, run_text};
use lm_vm::VmConfig;
use std::time::{Duration, Instant};

const BOUND: Duration = Duration::from_secs(30);

fn timed<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    // The deep-nesting cases sit near 100 source levels. The
    // supported guarantee is a standard 8 MiB stack (week-2 note);
    // the compile pipeline grew with the week-4 surfaces, so the
    // gate runs on the guaranteed stack instead of the smaller
    // default test-thread stack.
    let start = Instant::now();
    let out = std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(f)
        .expect("thread starts")
        .join()
        .expect("no stack overflow");
    let elapsed = start.elapsed();
    assert!(
        elapsed < BOUND,
        "{what} took {elapsed:?}, over the {BOUND:?} bound"
    );
    out
}

#[test]
fn large_enum_match_is_linear() {
    // One enum with 300 arms and one full case over it.
    let n = 300;
    let mut source = String::from("enum Big\n");
    for i in 0..n {
        source.push_str(&format!("  A{i}(v: Int)\n"));
    }
    source.push_str("end\ndef pick(b: Big): Int\n  case b\n");
    for i in 0..n {
        source.push_str(&format!("  in A{i}(v) then v + {i}\n"));
    }
    source.push_str("  end\nend\npick(A7(100))\n");
    let out = timed("large enum match", move || {
        run_text("big.lm", &source, VmConfig::default()).unwrap()
    });
    assert_eq!(out, "Done(107)");
}

#[test]
fn deep_generic_nesting_is_bounded() {
    // A deeply nested generic application inside the parser guard.
    let depth = 60;
    let ty = format!("{}Int{}", "Box[".repeat(depth), "]".repeat(depth));
    let mut build = "Box(1)".to_string();
    for _ in 1..depth {
        build = format!("Box({build})");
    }
    let source = format!(
        "class Box[T]\n  v: T\n  def init(mut self, v: T)\n    self.v = v\n  end\nend\n\
         x: {ty} = {build}\n0\n"
    );
    let out = timed("deep generic nesting", move || {
        run_text("deep.lm", &source, VmConfig::default()).unwrap()
    });
    assert_eq!(out, "Done(0)");
}

#[test]
fn wide_branch_joins_are_bounded() {
    // A 200-branch elsif chain that joins two subclasses.
    let n = 200;
    let mut source = String::from(
        "class Animal\nend\nclass Dog < Animal\nend\nclass Cat < Animal\nend\n\
         def pick(n: Int): Animal\n  if n == 0\n    Dog()\n",
    );
    for i in 1..n {
        source.push_str(&format!("  elsif n == {i}\n    Cat()\n"));
    }
    source.push_str("  else\n    Dog()\n  end\nend\npick(3) is Cat\n");
    let out = timed("wide joins", move || {
        run_text("wide.lm", &source, VmConfig::default()).unwrap()
    });
    assert_eq!(out, "Done(true)");
}

#[test]
fn many_generic_instantiations_are_bounded() {
    // 200 call sites with distinct instantiations share one body.
    let n = 200;
    let mut source = String::from(
        "class Box[T]\n  v: T\n  def init(mut self, v: T)\n    self.v = v\n  end\nend\n\
         def id[T](x: T): T\n  x\nend\ntotal = 0\n",
    );
    let mut nested_ty = String::from("Int");
    let mut nested_build = String::from("1");
    for i in 0..n {
        nested_ty = format!("Box[{nested_ty}]");
        nested_build = format!("Box({nested_build})");
        source.push_str(&format!("y{i} = id[{nested_ty}]({nested_build})\n"));
        source.push_str("total = total + 1\n");
        // Keep the nesting shallow per statement.
        if nested_ty.len() > 400 {
            nested_ty = String::from("Int");
            nested_build = String::from("1");
        }
    }
    source.push_str("total\n");
    let out = timed("many instantiations", move || {
        run_text("insts.lm", &source, VmConfig::default()).unwrap()
    });
    assert_eq!(out, format!("Done({n})"));
}

#[test]
fn nested_pattern_analysis_stays_inside_its_budget() {
    // A diagonal nested-option matrix with 12 levels stays linear in
    // this shape; the analysis budget also keeps hostile shapes from
    // running without bound.
    let depth = 12;
    let mut source = String::from("def probe(o: ");
    let mut ty = String::from("Int");
    for _ in 0..depth {
        ty = format!("Option[{ty}]");
    }
    source.push_str(&ty);
    source.push_str("): Int\n  case o\n");
    for level in 0..depth {
        let mut pat = String::from("None");
        for _ in 0..level {
            pat = format!("Some({pat})");
        }
        source.push_str(&format!("  in {pat} then {level}\n"));
    }
    let mut full = String::from("Some(n)");
    for _ in 1..depth {
        full = format!("Some({full})");
    }
    source.push_str(&format!("  in {full} then n\n  end\nend\n"));
    let mut build = String::from("None");
    for _ in 0..3 {
        build = format!("Some({build})");
    }
    source.push_str(&format!("probe({build})\n"));
    let out = timed("nested pattern analysis", move || {
        run_text("nested.lm", &source, VmConfig::default()).unwrap()
    });
    assert_eq!(out, "Done(3)");
}

#[test]
fn compile_twice_is_deterministic_for_week3_surfaces() {
    for example in ["examples/03-types/expr.lm", "examples/03-types/generics.lm"] {
        let source = std::fs::read_to_string(lm_testkit::repo_root().join(example)).unwrap();
        let a = lm_bytecode::encode(&compile_text(example, &source).unwrap());
        let b = lm_bytecode::encode(&compile_text(example, &source).unwrap());
        assert_eq!(a, b, "bytecode bytes differ for {example}");
    }
}
