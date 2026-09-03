//! Standard compression behavior.

use lm_testkit::run_text;
use lm_vm::VmConfig;

fn run(source: &str) -> String {
    run_text("standard-compress.lm", source, VmConfig::default())
        .expect("the compression program runs")
}

#[test]
fn gzip_and_deflate_round_trip_binary_data() {
    let source = r#"
use std.compress.CompressionLevel
use std.compress.deflate_compress
use std.compress.deflate_decompress
use std.compress.gzip_compress
use std.compress.gzip_decompress

input = b"loom\x00loom\xffloom\x00loom\xff"
gzip = gzip_compress(input, CompressionLevel.Balanced)
deflate = deflate_compress(input, CompressionLevel.Best)
(
  gzip.at(0),
  gzip.at(1),
  gzip_decompress(gzip, 1024).expect("gzip is valid") == input,
  deflate_decompress(deflate, 1024).expect("deflate is valid") == input
)
"#;

    assert_eq!(run(source), "Done((31, 139, true, true))");
}

#[test]
fn decompression_rejects_invalid_data_and_large_output() {
    let source = r#"
use std.compress.CompressionError
use std.compress.CompressionLevel
use std.compress.gzip_compress
use std.compress.gzip_decompress

invalid = case gzip_decompress(b"not gzip", 100)
in Err(CompressionError.InvalidData) then true
in _ then false
end
encoded = gzip_compress(b"abcdefgh", CompressionLevel.Fast)
limited = case gzip_decompress(encoded, 7)
in Err(CompressionError.LimitExceeded) then true
in _ then false
end
bad_limit = case gzip_decompress(encoded, -1)
in Err(CompressionError.LimitExceeded) then true
in _ then false
end
(invalid, limited, bad_limit)
"#;

    assert_eq!(run(source), "Done((true, true, true))");
}
