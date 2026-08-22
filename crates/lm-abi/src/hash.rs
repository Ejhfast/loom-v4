//! BLAKE3-256 for stable content identities.

/// Compute one BLAKE3-256 content hash.
pub fn hash256(input: &[u8]) -> [u8; 32] {
    *blake3::hash(input).as_bytes()
}

/// Render one BLAKE3-256 content hash as lowercase hexadecimal text.
pub fn hash256_hex(input: &[u8]) -> String {
    let digest = hash256(input);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut out, "{byte:02x}").expect("writing to a string cannot fail");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_vectors_match() {
        assert_eq!(
            hash256_hex(b""),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_eq!(
            hash256_hex(b"abc"),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
    }
}
