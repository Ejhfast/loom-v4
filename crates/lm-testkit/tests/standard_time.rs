//! Standard duration and clock values.

use lm_testkit::run_allowed;

fn run(source: &str, grants: &[&str]) -> String {
    run_allowed("standard-time.lm", source, grants).expect("the program runs")
}

#[test]
fn duration_units_are_checked() {
    let source = r#"
use std.time.Duration
use std.time.TimeError
use std.time.microseconds
use std.time.milliseconds
use std.time.nanoseconds
use std.time.seconds

def error_name(value: Result[Duration, TimeError]): String
  case value
  in Ok(duration) then display(duration)
  in Err(TimeError.Overflow) then "overflow"
  in Err(_) then "other"
  end
end

(
  display(nanoseconds(12)),
  display(microseconds(3).expect("valid microseconds")),
  display(milliseconds(-2).expect("valid milliseconds")),
  display(seconds(4).expect("valid seconds")),
  error_name(seconds(9223372037)),
  error_name(nanoseconds(9223372036854775807).checked_add(nanoseconds(1)))
)
"#;

    assert_eq!(
        run(source, &[]),
        "Done((\"12ns\", \"3000ns\", \"-2000000ns\", \"4000000000ns\", \"overflow\", \"overflow\"))"
    );
}

#[test]
fn clock_wrappers_preserve_effects_and_units() {
    let source = r#"
use std.time.monotonic
use std.time.now
use std.time.seconds
use std.time.sleep

def go(): (Int, Int, Int) with Clock.Now, Clock.Monotonic, Clock.Sleep
  wall = now()
  first = monotonic()
  sleep(seconds(0).expect("zero fits")).expect("zero sleep works")
  second = monotonic()
  elapsed = second.elapsed_since(first).expect("the clock advances")
  (wall.nanoseconds, elapsed.nanoseconds, second.nanoseconds)
end
go()
"#;

    assert_eq!(
        run(source, &["Clock.Now", "Clock.Monotonic", "Clock.Sleep"]),
        "Done((1001, 1, 2))"
    );
}

#[test]
fn sleep_rejects_negative_durations_without_an_effect() {
    let source = r#"
use std.time.TimeError
use std.time.nanoseconds
use std.time.sleep

def go(): String with Clock.Sleep
  case sleep(nanoseconds(-1))
  in Err(TimeError.NegativeDuration) then "negative"
  in _ then "wrong"
  end
end
go()
"#;

    assert_eq!(run(source, &["Clock.Sleep"]), "Done(\"negative\")");
}
