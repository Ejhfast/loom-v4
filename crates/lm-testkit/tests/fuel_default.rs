//! The default instruction budget is unbounded.
//!
//! A program that serves forever is an ordinary program. The
//! specification declares `def serve(): Never` in section 7.2, so a
//! root program must not take an instruction cap it never asked for.
//! A caller that runs code it does not trust states a bound instead.

use lm_testkit::run_world;
use lm_vm::{VmConfig, WorldLimits};

/// The former default stopped a program after one billion
/// instructions. This loop retires more than that and finishes.
#[test]
fn a_program_runs_past_the_former_default_budget() {
    let source = "i = 0
total = 0
while i < 400000000
  total = total + 1
  i = i + 1
end
total
";
    assert_eq!(
        run_world("long.lm", source, &[], VmConfig::default())
            .unwrap()
            .0,
        "Done(400000000)"
    );
}

/// A stated bound still stops a program that never ends.
#[test]
fn a_stated_bound_still_stops_an_endless_program() {
    let source = "i = 0
while true
  i = i + 1
end
";
    let config = VmConfig {
        fuel: 10_000,
        ..VmConfig::default()
    };
    assert_eq!(
        run_world("endless.lm", source, &[], config).unwrap().0,
        "Fault(OutOfFuel)"
    );
}

/// Both budgets default to unbounded: the one machine budget and the
/// one every machine of a world shares.
#[test]
fn both_default_budgets_are_unbounded() {
    assert_eq!(VmConfig::default().fuel, u64::MAX);
    assert_eq!(WorldLimits::default().fuel, u64::MAX);
}
