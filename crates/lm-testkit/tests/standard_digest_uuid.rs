//! Standard digest and UUID values.

use lm_testkit::run_allowed;

fn run(source: &str, grants: &[&str]) -> String {
    run_allowed("standard-digest-uuid.lm", source, grants).expect("the program runs")
}

#[test]
fn digest_functions_match_standard_vectors() {
    let source = r#"
use std.digest.crc32
use std.digest.md5_hex
use std.digest.sha256
use std.digest.sha256_hex

(
  sha256(b"").hex(),
  sha256_hex(b"abc"),
  md5_hex(b"abc"),
  crc32(b"123456789")
)
"#;

    assert_eq!(
        run(source, &[]),
        concat!(
            "Done((\"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934c",
            "a495991b7852b855\", ",
            "\"ba7816bf8f01cfea414140de5dae2223b00361a396177a9c",
            "b410ff61f20015ad\", \"900150983cd24fb0d6963f7d28e17f72\", ",
            "3421780262))"
        )
    );
}

#[test]
fn uuid_text_and_versions_follow_rfc_9562() {
    let source = r#"
use std.time.Timestamp
use std.uuid.UuidError
use std.uuid.nil
use std.uuid.parse
use std.uuid.v4_from
use std.uuid.v7_from

invalid = case parse("550e8400-e29b-41d4-a716-44665544000z")
in Err(UuidError.InvalidSyntax) then true
in _ then false
end
parsed = parse("550E8400-E29B-41D4-A716-446655440000").expect("the UUID is valid")
four = v4_from(b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00").expect("the random input fits")
seven = v7_from(Timestamp(0), b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00").expect("the UUID inputs fit")
(
  display(parsed),
  parsed.version(),
  display(parsed.variant()),
  invalid,
  display(four),
  four.version(),
  display(seven),
  seven.version(),
  nil().is_nil()
)
"#;

    assert_eq!(
        run(source, &[]),
        concat!(
            "Done((\"550e8400-e29b-41d4-a716-446655440000\", 4, ",
            "\"RFC 4122\", true, \"00000000-0000-4000-8000-000000000000\", 4, ",
            "\"00000000-0000-7000-8000-000000000000\", 7, true))"
        )
    );
}

#[test]
fn uuid_generators_use_explicit_host_effects() {
    let source = r#"
use std.uuid.v4
use std.uuid.v7

def make(): (Int, String, Int, String) with Clock.Now, Entropy.Bytes
  four = v4().expect("entropy works")
  seven = v7().expect("the clock and entropy work")
  (four.version(), display(four.variant()), seven.version(), display(seven.variant()))
end
make()
"#;

    assert_eq!(
        run(source, &["Clock.Now", "Entropy.Bytes"]),
        "Done((4, \"RFC 4122\", 7, \"RFC 4122\"))"
    );
}
