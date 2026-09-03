//! Bounded compression for Loom.
//!
//! This crate owns the compression semantics used by every Loom engine.

use flate2::bufread::{GzEncoder, MultiGzDecoder, ZlibDecoder, ZlibEncoder};
use flate2::Compression;
use std::io::{self, Read};

const BUFFER_BYTES: usize = 8 * 1024;

/// One supported compression wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// The gzip wrapper from RFC 1952.
    Gzip,
    /// The zlib wrapper from RFC 1950.
    Zlib,
}

/// One bounded compression error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressError {
    /// The compression level is outside zero through nine.
    InvalidLevel,
    /// The allocator rejected a bounded reservation.
    Allocation,
    /// The backend rejected valid compression input.
    Backend,
}

/// One bounded decompression error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecompressError {
    /// The input is not one complete value of the selected format.
    InvalidData,
    /// The output exceeds the caller's byte limit.
    Limit,
    /// The allocator rejected a bounded reservation.
    Allocation,
}

/// Compress bytes with one deterministic wrapper and level.
pub fn compress(input: &[u8], format: Format, level: u32) -> Result<Vec<u8>, CompressError> {
    if level > 9 {
        return Err(CompressError::InvalidLevel);
    }
    let limit = compressed_size_limit(input.len()).ok_or(CompressError::Allocation)?;
    let level = Compression::new(level);
    match format {
        Format::Gzip => read_compressed(GzEncoder::new(input, level), limit),
        Format::Zlib => read_compressed(ZlibEncoder::new(input, level), limit),
    }
}

/// Decompress one complete value within an explicit byte limit.
pub fn decompress(
    input: &[u8],
    format: Format,
    max_output_bytes: usize,
) -> Result<Vec<u8>, DecompressError> {
    match format {
        Format::Gzip => {
            let mut decoder = MultiGzDecoder::new(input);
            read_decompressed(&mut decoder, max_output_bytes)
        }
        Format::Zlib => {
            let mut decoder = ZlibDecoder::new(input);
            let output = read_decompressed(&mut decoder, max_output_bytes)?;
            if !decoder.get_ref().is_empty() {
                return Err(DecompressError::InvalidData);
            }
            Ok(output)
        }
    }
}

fn compressed_size_limit(input_bytes: usize) -> Option<usize> {
    input_bytes
        .checked_add(input_bytes >> 12)?
        .checked_add(input_bytes >> 14)?
        .checked_add(input_bytes >> 25)?
        .checked_add(64)
}

fn read_compressed(
    mut reader: impl Read,
    max_output_bytes: usize,
) -> Result<Vec<u8>, CompressError> {
    read_bounded(&mut reader, max_output_bytes).map_err(|error| match error {
        ReadError::Limit | ReadError::Allocation => CompressError::Allocation,
        ReadError::Input => CompressError::Backend,
    })
}

fn read_decompressed(
    reader: &mut impl Read,
    max_output_bytes: usize,
) -> Result<Vec<u8>, DecompressError> {
    read_bounded(reader, max_output_bytes).map_err(|error| match error {
        ReadError::Input => DecompressError::InvalidData,
        ReadError::Limit => DecompressError::Limit,
        ReadError::Allocation => DecompressError::Allocation,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadError {
    Input,
    Limit,
    Allocation,
}

fn read_bounded(reader: &mut impl Read, max_output_bytes: usize) -> Result<Vec<u8>, ReadError> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; BUFFER_BYTES];
    loop {
        let remaining = max_output_bytes.saturating_sub(output.len());
        let read_limit = buffer.len().min(remaining.saturating_add(1));
        let count = reader
            .read(&mut buffer[..read_limit])
            .map_err(map_read_error)?;
        if count == 0 {
            return Ok(output);
        }
        if count > remaining {
            return Err(ReadError::Limit);
        }
        output
            .try_reserve_exact(count)
            .map_err(|_| ReadError::Allocation)?;
        output.extend_from_slice(&buffer[..count]);
    }
}

fn map_read_error(_: io::Error) -> ReadError {
    ReadError::Input
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &[u8] = b"Loom compression repeats. Loom compression repeats.";

    #[test]
    fn gzip_round_trip_is_deterministic() {
        let first = compress(TEXT, Format::Gzip, 6).expect("gzip compression succeeds");
        let second = compress(TEXT, Format::Gzip, 6).expect("gzip compression succeeds");
        assert_eq!(first, second);
        assert_eq!(
            decompress(&first, Format::Gzip, TEXT.len()).expect("gzip decompression succeeds"),
            TEXT
        );
    }

    #[test]
    fn zlib_round_trip_is_deterministic() {
        let first = compress(TEXT, Format::Zlib, 9).expect("zlib compression succeeds");
        let second = compress(TEXT, Format::Zlib, 9).expect("zlib compression succeeds");
        assert_eq!(first, second);
        assert_eq!(
            decompress(&first, Format::Zlib, TEXT.len()).expect("zlib decompression succeeds"),
            TEXT
        );
    }

    #[test]
    fn decompression_enforces_the_output_limit() {
        let compressed = compress(TEXT, Format::Gzip, 1).expect("gzip compression succeeds");
        assert_eq!(
            decompress(&compressed, Format::Gzip, TEXT.len() - 1),
            Err(DecompressError::Limit)
        );
    }

    #[test]
    fn decompression_rejects_invalid_and_trailing_data() {
        assert_eq!(
            decompress(b"not gzip", Format::Gzip, 1024),
            Err(DecompressError::InvalidData)
        );
        let mut compressed = compress(TEXT, Format::Zlib, 6).expect("zlib compression succeeds");
        compressed.push(0);
        assert_eq!(
            decompress(&compressed, Format::Zlib, 1024),
            Err(DecompressError::InvalidData)
        );
    }

    #[test]
    fn gzip_accepts_multiple_members() {
        let mut compressed = compress(b"one", Format::Gzip, 6).expect("gzip compression succeeds");
        compressed.extend(compress(b"two", Format::Gzip, 6).expect("gzip compression succeeds"));
        assert_eq!(
            decompress(&compressed, Format::Gzip, 6).expect("gzip decompression succeeds"),
            b"onetwo"
        );
    }

    #[test]
    fn compression_rejects_an_invalid_level() {
        assert_eq!(
            compress(TEXT, Format::Gzip, 10),
            Err(CompressError::InvalidLevel)
        );
    }
}
