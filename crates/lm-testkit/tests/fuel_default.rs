//! The default instruction budget does not cap a root program.
//!
//! A program that serves forever is an ordinary program. The
//! specification declares `def serve(): Never` in section 7.2, so a
//! root program must not take an instruction cap it never asked for.
//! A caller that runs code it does not trust states a bound instead.

use lm_testkit::publish_artifact_bytes;
use lm_testkit::{compile_to_bytes, run_world};
use lm_vm::{NullHost, VmConfig, World, WorldLimits};

/// Both budgets default to the largest value: the budget of one
/// machine, and the budget every machine of one world shares.
///
/// The former default of one billion stopped a root program after a
/// few seconds of work. This case is the regression guard, and it
/// costs no execution: a case that retires a billion instructions
/// proves the same fact and slows the suite.
#[test]
fn both_default_budgets_are_the_largest_value() {
    assert_eq!(VmConfig::default().fuel, u64::MAX);
    assert_eq!(WorldLimits::default().fuel, u64::MAX);
}

/// A program runs under the default and no cap fires. The world still
/// counts what it retired.
#[test]
fn a_program_under_the_default_retires_with_no_cap() {
    let source = "i = 0\nwhile i < 2000\n  i = i + 1\nend\ni\n";
    let bytes = compile_to_bytes("fuel.lm", source).expect("the program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut world = World::new(arena, namespace, VmConfig::default(), Box::new(NullHost));
    let before = world.world_fuel();
    let outcome = world.run_root();
    let retired = before - world.world_fuel();
    assert_eq!(world.show_outcome(&outcome), "Done(2000)");
    assert!(retired > 2_000, "the world counted the work: {retired}");
}

/// A stated bound still stops a program that never ends.
#[test]
fn a_stated_bound_still_stops_an_endless_program() {
    let source = "i = 0\nloop do\n  i = i + 1\nend\n";
    let config = VmConfig {
        fuel: 10_000,
        ..VmConfig::default()
    };
    assert_eq!(
        run_world("endless.lm", source, &[], config).unwrap().0,
        "Fault(OutOfFuel)"
    );
}
