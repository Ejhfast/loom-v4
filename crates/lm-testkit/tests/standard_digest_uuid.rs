//! Standard digest and UUID values.

use lm_testkit::run_allowed;
use sha2::{Digest, Sha256};
use std::fmt::Write;

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
fn digest_states_support_chunks_copies_and_reset() {
    let source = r#"
use std.digest.Crc32
use std.digest.Md5
use std.digest.Sha256
use std.digest.crc32
use std.digest.md5
use std.digest.sha256

sha = Sha256()
sha.update(b"a")
sha_copy = sha.copy()
first_sha = sha.finish()
sha.update(b"bc")
sha_copy.update(b"x")
sha_result = sha.finish()
sha_again = sha.finish()
sha.reset()
sha.update(b"abc")

md = Md5()
md.update(b"a")
md_copy = md.copy()
md.update(b"bc")
md_copy.update(b"x")
md.reset()
md.update(b"abc")

crc = Crc32()
crc.update(b"1")
crc_copy = crc.copy()
crc.update(b"23456789")
crc_copy.update(b"x")
crc.reset()
crc.update(b"123456789")

(
  first_sha == sha256(b"a"),
  sha_result == sha256(b"abc"),
  sha_again == sha_result,
  sha_copy.finish() == sha256(b"ax"),
  sha.finish() == sha256(b"abc"),
  md.finish() == md5(b"abc"),
  md_copy.finish() == md5(b"ax"),
  crc.finish() == crc32(b"123456789"),
  crc_copy.finish() == crc32(b"1x")
)
"#;

    assert_eq!(
        run(source, &[]),
        "Done((true, true, true, true, true, true, true, true, true))"
    );
}

fn byte_literal(bytes: &[u8]) -> String {
    let mut result = String::from("b\"");
    for byte in bytes {
        write!(result, "\\x{byte:02x}").expect("a byte escape fits");
    }
    result.push('"');
    result
}

fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(result, "{byte:02x}").expect("a hex byte fits");
    }
    result
}

#[test]
fn digest_algorithms_match_the_reference_at_block_boundaries() {
    for length in [
        0, 1, 7, 55, 56, 57, 63, 64, 65, 119, 120, 121, 127, 128, 129, 1024,
    ] {
        let bytes: Vec<u8> = (0..length)
            .map(|index| ((index * 131 + length * 17) & 255) as u8)
            .collect();
        let source = format!(
            concat!(
                "use std.digest.Crc32\n",
                "use std.digest.Md5\n",
                "use std.digest.Sha256\n",
                "use std.digest.crc32\n",
                "use std.digest.md5_hex\n",
                "use std.digest.sha256_hex\n",
                "value = {}\n",
                "sha = Sha256()\nmd = Md5()\ncrc = Crc32()\n",
                "offset = 0\n",
                "while offset < value.len()\n",
                "  count = ((offset * 7 + 3) % 71) + 1\n",
                "  if count > value.len() - offset\n",
                "    count = value.len() - offset\n",
                "  end\n",
                "  part = value.slice(offset, count).expect(\"the test range is valid\")\n",
                "  sha.update(part)\nmd.update(part)\ncrc.update(part)\n",
                "  offset = offset + count\n",
                "end\n",
                "(sha.finish().hex(), md.finish().hex(), crc.finish(), ",
                "sha256_hex(value), md5_hex(value), crc32(value))\n",
            ),
            byte_literal(&bytes)
        );
        let expected = format!(
            "Done((\"{sha}\", \"{md5}\", {crc}, \"{sha}\", \"{md5}\", {crc}))",
            sha = hex(&Sha256::digest(&bytes)),
            md5 = hex(&md5::Md5::digest(&bytes)),
            crc = crc32fast::hash(&bytes)
        );
        assert_eq!(run(&source, &[]), expected, "length {length}");
    }
}

#[test]
fn uuid_text_and_versions_follow_rfc_9562() {
    let source = r#"
use std.time.Timestamp
use std.uuid.UuidError
use std.uuid.from_bytes
use std.uuid.nil
use std.uuid.parse
use std.uuid.v4_from
use std.uuid.v7_from

invalid = case parse("550e8400-e29b-41d4-a716-44665544000z")
in Err(UuidError.InvalidSyntax) then true
in _ then false
end
parsed = parse("550E8400-E29B-41D4-A716-446655440000").expect("the UUID is valid")
raw = from_bytes(b"\x55\x0e\x84\x00\xe2\x9b\x41\xd4\xa7\x16\x44\x66\x55\x44\x00\x00").expect("the UUID bytes are valid")
invalid_bytes = case from_bytes(b"short")
in Err(UuidError.InvalidByteLength) then true
in _ then false
end
four = v4_from(b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00").expect("the random input fits")
seven = v7_from(Timestamp(0), b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00").expect("the UUID inputs fit")
(
  display(parsed),
  parsed.version(),
  display(parsed.variant()),
  invalid,
  display(raw),
  invalid_bytes,
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
            "\"RFC 4122\", true, \"550e8400-e29b-41d4-a716-446655440000\", true, ",
            "\"00000000-0000-4000-8000-000000000000\", 4, ",
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
