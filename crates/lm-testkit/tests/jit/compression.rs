use super::*;

const COMPRESSION_PROGRAM: &str = r#"
use std.compress.CompressionLevel
use std.compress.deflate_compress
use std.compress.deflate_decompress
use std.compress.gzip_compress
use std.compress.gzip_decompress

input = b"native compression data native compression data"
i = 0
total = 0
while i < 100
  gzip = gzip_compress(input, CompressionLevel.Balanced)
  deflate = deflate_compress(input, CompressionLevel.Fast)
  total = total + gzip_decompress(gzip, 1024).expect("gzip is valid").len()
  total = total + deflate_decompress(deflate, 1024).expect("deflate is valid").len()
  i = i + 1
end
total
"#;

#[test]
fn compression_operations_match_across_engines() {
    let artifact = lm_testkit::compile_text("jit-compression.lm", COMPRESSION_PROGRAM)
        .expect("the compression case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 1_000, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn decompression_matches_each_fuel_boundary() {
    let source = concat!(
        "use std.compress.CompressionLevel\n",
        "use std.compress.gzip_compress\n",
        "use std.compress.gzip_decompress\n",
        "input = b\"fuel boundary data\"\n",
        "encoded = gzip_compress(input, CompressionLevel.Fast)\n",
        "gzip_decompress(encoded, 1024).expect(\"valid\")\n",
    );
    let artifact = lm_testkit::compile_text("jit-compression-fuel.lm", source)
        .expect("the compression fuel case compiles");
    for fuel in 0..=80 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn compression_allocations_preserve_roots() {
    let artifact = lm_testkit::compile_text("jit-compression-roots.lm", COMPRESSION_PROGRAM)
        .expect("the compression root case compiles");
    let config = VmConfig {
        heap_bytes: 32 * 1024,
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert!(metrics.native_heap_allocations > 100, "{metrics:?}");
}
