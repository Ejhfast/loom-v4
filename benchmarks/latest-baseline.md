# Latest benchmark baseline

The measured source revision is `1bb9678`.

The measurements use release builds unless this file states a different build.

The bytecode version is 55.

The verifier version is 36.

The snapshot format version is 30.

## Core image

| Measurement | Result |
| --- | ---: |
| Classes | 302 |
| HIR functions | 571 |
| Bytecode functions | 873 |
| Artifact size | 264,401 bytes |
| Core checking | 2.124 ms |
| Core lowering | 0.927 ms |
| Core compilation | 3.298 ms |
| Core decoding | 0.385 ms |
| Core verification | 1.264 ms |
| Structural verification | 0.454 ms |
| Verification hash | 0.128 ms |
| Semantic identity | 2.383 ms |
| Decoded loading | 1.444 ms |
| Core loading | 1.834 ms |

## Language operations

Each result is a nine-run median.

The runtime process used CPU 0.

| Operation | Result |
| --- | ---: |
| `int_loop` | 31.6 ns |
| `direct_call` | 30.6 ns |
| `string_interp` | 223.4 ns |
| `float_add` | 32.8 ns |
| `string_builder` | 43.0 ns |
| `byte_buffer` | 37.7 ns |
| `direct_clock` | 110.2 ns |

String measurements can vary with process layout.

## Workspace suite

The warm debug workspace suite completed in 43.364 seconds.

The suite used the existing worker count and full coverage.

The standard codec modules link only when source imports them.
