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

#[test]
fn calendar_validation_uses_the_proleptic_gregorian_calendar() {
    let source = r#"
use std.time.TimeError
use std.time.Date
use std.time.date
use std.time.days_in_month
use std.time.is_leap_year

def valid(value: Result[Date, TimeError]): Bool
  case value
  in Ok(_) then true
  in Err(_) then false
  end
end

(
  is_leap_year(2000),
  is_leap_year(1900),
  days_in_month(2024, 2).expect("February is valid"),
  valid(date(2024, 2, 29)),
  valid(date(2023, 2, 29)),
  display(date(1, 1, 1).expect("the first year is valid"))
)
"#;

    assert_eq!(
        run(source, &[]),
        "Done((true, false, 29, true, false, \"0001-01-01\"))"
    );
}

#[test]
fn rfc3339_parsing_and_formatting_preserve_instants() {
    let source = r#"
use std.time.TimeError
use std.time.format_rfc3339
use std.time.parse_rfc3339

def parse_nanos(text: Text): Int
  parse_rfc3339(text).expect("valid RFC 3339").timestamp().expect("the date fits").nanoseconds
end

def invalid(text: Text): Bool
  case parse_rfc3339(text)
  in Err(TimeError.InvalidRfc3339) then true
  in _ then false
  end
end

first = parse_rfc3339("2000-02-29T12:34:56.123400000+05:30").expect("valid leap date")
(
  parse_nanos("1970-01-01T00:00:00Z"),
  parse_nanos("1969-12-31T23:59:59.5Z"),
  parse_nanos("1970-01-01T01:00:00+01:00"),
  format_rfc3339(first).expect("the value formats"),
  invalid("2023-02-29T00:00:00Z"),
  invalid("2024-01-01T00:00:00.1234567890Z"),
  invalid("2024-01-01T00:00:60Z")
)
"#;

    assert_eq!(
        run(source, &[]),
        "Done((0, -500000000, 0, \"2000-02-29T12:34:56.1234+05:30\", true, true, true))"
    );
}

#[test]
fn timestamp_conversion_handles_negative_epoch_values() {
    let source = r#"
use std.time.Timestamp
use std.time.UtcOffset
use std.time.format_rfc3339
use std.time.from_timestamp

(
  format_rfc3339(from_timestamp(Timestamp(-1), UtcOffset(0)).expect("the timestamp fits")).expect("the value formats"),
  format_rfc3339(from_timestamp(Timestamp(0), UtcOffset(-28800)).expect("the timestamp fits")).expect("the value formats")
)
"#;

    assert_eq!(
        run(source, &[]),
        "Done((\"1969-12-31T23:59:59.999999999Z\", \"1969-12-31T16:00:00-08:00\"))"
    );
}
