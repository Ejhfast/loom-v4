//! Standard base64 and JSON codecs.

use lm_testkit::run_text;
use lm_vm::VmConfig;

fn run(source: &str) -> String {
    run_text("standard-codecs.lm", source, VmConfig::default()).expect("the program runs")
}

#[test]
fn bytes_decode_hex_with_checked_errors() {
    let source = r#"
def error_name(value: Result[Bytes, HexError]): String
  case value
  in Ok(bytes) then bytes.hex()
  in Err(HexError.OddLength) then "odd"
  in Err(HexError.InvalidDigit(index)) then "digit #{index}"
  end
end

(
  Bytes.from_hex("00ff10").expect("valid hex").hex(),
  Bytes.from_hex("A0b1").expect("mixed hex").hex(),
  error_name(Bytes.from_hex("abc")),
  error_name(Bytes.from_hex("0x"))
)
"#;

    assert_eq!(
        run(source),
        "Done((\"00ff10\", \"a0b1\", \"odd\", \"digit 1\"))"
    );
}

#[test]
fn base64_matches_the_rfc_vectors_and_rejects_bad_text() {
    let source = r#"
use std.base64.Base64Error
use std.base64.decode
use std.base64.encode

def error_name(value: Result[Bytes, Base64Error]): String
  case value
  in Ok(bytes) then bytes.hex()
  in Err(Base64Error.InvalidLength) then "length"
  in Err(Base64Error.InvalidByte(index)) then "byte #{index}"
  in Err(Base64Error.InvalidPadding) then "padding"
  end
end

(
  encode(b""),
  encode(b"f"),
  encode(b"fo"),
  encode(b"foo"),
  encode(b"foobar"),
  decode("AP8B").expect("binary data").hex(),
  error_name(decode("A")),
  error_name(decode("AA?=")),
  error_name(decode("AB=="))
)
"#;

    assert_eq!(
        run(source),
        "Done((\"\", \"Zg==\", \"Zm8=\", \"Zm9v\", \"Zm9vYmFy\", \"00ff01\", \"length\", \"byte 2\", \"padding\"))"
    );
}

#[test]
fn json_round_trips_values_and_unicode_escapes() {
    let source = r#"
use std.json.Json
use std.json.JsonError
use std.json.parse
use std.json.stringify

def go(): (String, String, String, String)
  source = "{\"name\":\"loom\",\"values\":[1,2.5,true,null],\"face\":\"\\ud83d\\ude00\"}"
  value = parse(source).expect("valid JSON")
  rendered = stringify(value).expect("finite JSON")
  name = case value
  in Json.Object(fields)
    case fields.get("name")
    in Some(Json.Text(text)) then text
    in _ then "missing"
    end
  in _ then "not object"
  end
  face = case value
  in Json.Object(fields)
    case fields.get("face")
    in Some(Json.Text(text)) then text
    in _ then "missing"
    end
  in _ then "not object"
  end
  invalid = case parse("[1,]")
  in Err(JsonError.Invalid(_, _)) then "invalid"
  in _ then "accepted"
  end
  (rendered, name, face, invalid)
end

go()
"#;

    assert_eq!(
        run(source),
        "Done((\"{\\\"name\\\":\\\"loom\\\",\\\"values\\\":[1,2.5,true,null],\\\"face\\\":\\\"😀\\\"}\", \"loom\", \"😀\", \"invalid\"))"
    );
}

#[test]
fn json_rejects_non_finite_numbers_and_excessive_depth() {
    let nested = format!("{}null{}", "[".repeat(129), "]".repeat(129));
    let source = format!(
        r#"
use std.json.Json
use std.json.JsonError
use std.json.parse
use std.json.stringify

depth = case parse("{nested}")
in Err(JsonError.LimitExceeded(_)) then "depth"
in _ then "accepted"
end
infinite = Float.from_bits(9218868437227405312)
number = case stringify(Json.Number(infinite))
in Err(JsonError.NonFiniteNumber) then "finite"
in _ then "accepted"
end
(depth, number)
"#
    );

    assert_eq!(run(&source), "Done((\"depth\", \"finite\"))");
}

#[test]
fn json_pins_duplicate_keys_escapes_and_strict_numbers() {
    let source = r#"
use std.json.Json
use std.json.JsonError
use std.json.parse
use std.json.stringify

duplicate = case parse("{\"key\":1,\"key\":2}").expect("valid JSON")
in Json.Object(fields)
  case fields.get("key")
  in Some(Json.Number(value)) then value == 2.0
  in _ then false
  end
in _ then false
end
surrogate = case parse("\"\\ud800\"")
in Err(JsonError.Invalid(_, _)) then true
in _ then false
end
leading_zero = case parse("01")
in Err(JsonError.Invalid(_, _)) then true
in _ then false
end
escaped = stringify(Json.Text("line\n\t\"")).expect("finite JSON")
unicode = case parse("\"café\"").expect("valid UTF-8 JSON")
in Json.Text(text) then text
in _ then "wrong value"
end
(duplicate, surrogate, leading_zero, escaped, unicode)
"#;

    assert_eq!(
        run(source),
        "Done((true, true, true, \"\\\"line\\\\n\\\\t\\\\\\\"\\\"\", \"café\"))"
    );
}
