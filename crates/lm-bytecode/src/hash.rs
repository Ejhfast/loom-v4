//! SHA-256 for content hashes.
//!
//! The implementation lives in `lm-abi`, the dependency-free manifest
//! crate. This module re-exports it under the established path.

pub use lm_abi::{sha256, sha256_hex};
