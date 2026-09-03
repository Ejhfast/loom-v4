//! Standard deterministic and host-provided randomness.

use lm_testkit::run_allowed;

fn run(source: &str, grants: &[&str]) -> String {
    run_allowed("standard-random.lm", source, grants).expect("the program runs")
}

#[test]
fn seeded_random_sequences_are_portable() {
    let source = r#"
use std.random.seeded

first = seeded(1)
second = seeded(1)
values = List[Int]()
index = 0
while index < 4
  left = first.next_bits()
  right = second.next_bits()
  assert(left == right)
  values.push(left)
  index = index + 1
end
(values, first.next_float() >= 0.0 and first.next_float() < 1.0)
"#;

    assert_eq!(
        run(source, &[]),
        "Done(([-7995527694508729151, -4689498862643123097, -534904783426661026, 8196980753821780235], true))"
    );
}

#[test]
fn seeded_ranges_cover_narrow_and_wide_intervals() {
    let source = r#"
use std.random.RandomError
use std.random.seeded

random = seeded(19)
narrow = random.int(-5, 8).expect("the narrow range is valid")
wide = random.int(-9223372036854775807 - 1, 9223372036854775807).expect("the wide range is valid")
invalid = case random.int(4, 4)
in Err(RandomError.InvalidRange) then true
in _ then false
end
chosen = random.choose([10, 20, 30]).expect("the list is not empty")
empty = random.choose(List[Int]()).is_none()
shuffled = random.shuffle([0, 1, 2, 3, 4])
(
  narrow >= -5 and narrow < 8,
  wide >= -9223372036854775807 - 1 and wide < 9223372036854775807,
  invalid,
  chosen == 10 or chosen == 20 or chosen == 30,
  empty,
  shuffled.len() == 5
)
"#;

    assert_eq!(
        run(source, &[]),
        "Done((true, true, true, true, true, true))"
    );
}

#[test]
fn host_random_helpers_validate_and_use_exact_effects() {
    let source = r#"
use std.random.RandomError
use std.random.boolean
use std.random.bytes
use std.random.int

def go(): (Bool, Bool, Bool, String) with Rand.Int, Entropy.Bytes
  invalid = case int(2, 2)
  in Err(RandomError.InvalidRange) then true
  in _ then false
  end
  wide = int(-9223372036854775807 - 1, 9223372036854775807).expect("the wide range is valid")
  coin = boolean()
  (
    invalid,
    coin == true or coin == false,
    wide >= -9223372036854775807 - 1 and wide < 9223372036854775807,
    bytes(4).expect("entropy works").hex()
  )
end
go()
"#;

    assert_eq!(
        run(source, &["Rand.Int", "Entropy.Bytes"]),
        "Done((true, true, true, \"2972bb04\"))"
    );
}
