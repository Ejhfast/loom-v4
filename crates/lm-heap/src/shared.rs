//! Shared storage for immutable native values.

use std::collections::TryReserveError;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::{Arc, OnceLock};

/// Immutable UTF-8 text with shared storage and a cached lookup hash.
pub struct SharedText {
    storage: Arc<String>,
    start: usize,
    len: usize,
    lookup_hash: OnceLock<u64>,
}

impl SharedText {
    /// Make an empty text value.
    pub fn new() -> SharedText {
        SharedText::default()
    }

    /// Get the visible text.
    pub fn as_str(&self) -> &str {
        &self.storage[self.start..self.start + self.len]
    }

    /// Get the visible byte length.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Test whether the visible text is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Get the visible Unicode scalar count.
    pub fn char_count(&self) -> usize {
        self.as_str().chars().count()
    }

    /// Make a shared byte range.
    ///
    /// Both positions must be UTF-8 boundaries in the visible text.
    pub fn slice(&self, start: usize, end: usize) -> Option<SharedText> {
        if start > end || end > self.len {
            return None;
        }
        let text = self.as_str();
        if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            return None;
        }
        Some(SharedText {
            storage: self.storage.clone(),
            start: self.start + start,
            len: end - start,
            lookup_hash: OnceLock::new(),
        })
    }

    /// Join two values in new storage.
    pub fn try_concat(&self, other: &SharedText) -> Result<SharedText, TryReserveError> {
        let mut text = String::new();
        text.try_reserve_exact(self.len.saturating_add(other.len))?;
        text.push_str(self.as_str());
        text.push_str(other.as_str());
        Ok(SharedText::from(text))
    }

    /// Get the cached hash for map lookup.
    pub fn lookup_hash(&self) -> u64 {
        *self.lookup_hash.get_or_init(|| {
            let mut state = std::collections::hash_map::DefaultHasher::new();
            self.as_str().hash(&mut state);
            state.finish()
        })
    }

    /// Test whether this value shares its backing allocation with another value.
    pub fn shares_storage(&self, other: &SharedText) -> bool {
        Arc::ptr_eq(&self.storage, &other.storage)
    }

    /// Test whether this value has computed its lookup hash.
    pub fn has_cached_hash(&self) -> bool {
        self.lookup_hash.get().is_some()
    }
}

impl Default for SharedText {
    fn default() -> SharedText {
        SharedText::from(String::new())
    }
}

impl Clone for SharedText {
    fn clone(&self) -> SharedText {
        let lookup_hash = OnceLock::new();
        if let Some(hash) = self.lookup_hash.get() {
            let _ = lookup_hash.set(*hash);
        }
        SharedText {
            storage: self.storage.clone(),
            start: self.start,
            len: self.len,
            lookup_hash,
        }
    }
}

impl From<String> for SharedText {
    fn from(text: String) -> SharedText {
        let len = text.len();
        SharedText {
            storage: Arc::new(text),
            start: 0,
            len,
            lookup_hash: OnceLock::new(),
        }
    }
}

impl From<&str> for SharedText {
    fn from(text: &str) -> SharedText {
        SharedText::from(text.to_owned())
    }
}

impl AsRef<str> for SharedText {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Deref for SharedText {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Debug for SharedText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(f)
    }
}

impl fmt::Display for SharedText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq for SharedText {
    fn eq(&self, other: &SharedText) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for SharedText {}

impl Hash for SharedText {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

/// Immutable binary data with shared storage and a cached lookup hash.
pub struct SharedBytes {
    storage: Arc<Vec<u8>>,
    start: usize,
    len: usize,
    lookup_hash: OnceLock<u64>,
}

impl SharedBytes {
    /// Make an empty byte value.
    pub fn new() -> SharedBytes {
        SharedBytes::default()
    }

    /// Get the visible bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.storage[self.start..self.start + self.len]
    }

    /// Get the visible byte length.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Test whether the visible bytes are empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Make a shared byte range.
    pub fn slice(&self, start: usize, end: usize) -> Option<SharedBytes> {
        if start > end || end > self.len {
            return None;
        }
        Some(SharedBytes {
            storage: self.storage.clone(),
            start: self.start + start,
            len: end - start,
            lookup_hash: OnceLock::new(),
        })
    }

    /// Join two values in new storage.
    pub fn try_concat(&self, other: &SharedBytes) -> Result<SharedBytes, TryReserveError> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(self.len.saturating_add(other.len))?;
        bytes.extend_from_slice(self.as_slice());
        bytes.extend_from_slice(other.as_slice());
        Ok(SharedBytes::from(bytes))
    }

    /// Get the cached hash for map lookup.
    pub fn lookup_hash(&self) -> u64 {
        *self.lookup_hash.get_or_init(|| {
            let mut state = std::collections::hash_map::DefaultHasher::new();
            self.as_slice().hash(&mut state);
            state.finish()
        })
    }

    /// Test whether this value shares its backing allocation with another value.
    pub fn shares_storage(&self, other: &SharedBytes) -> bool {
        Arc::ptr_eq(&self.storage, &other.storage)
    }

    /// Test whether this value has computed its lookup hash.
    pub fn has_cached_hash(&self) -> bool {
        self.lookup_hash.get().is_some()
    }
}

impl Default for SharedBytes {
    fn default() -> SharedBytes {
        SharedBytes::from(Vec::new())
    }
}

impl Clone for SharedBytes {
    fn clone(&self) -> SharedBytes {
        let lookup_hash = OnceLock::new();
        if let Some(hash) = self.lookup_hash.get() {
            let _ = lookup_hash.set(*hash);
        }
        SharedBytes {
            storage: self.storage.clone(),
            start: self.start,
            len: self.len,
            lookup_hash,
        }
    }
}

impl From<Vec<u8>> for SharedBytes {
    fn from(bytes: Vec<u8>) -> SharedBytes {
        let len = bytes.len();
        SharedBytes {
            storage: Arc::new(bytes),
            start: 0,
            len,
            lookup_hash: OnceLock::new(),
        }
    }
}

impl From<&[u8]> for SharedBytes {
    fn from(bytes: &[u8]) -> SharedBytes {
        SharedBytes::from(bytes.to_vec())
    }
}

impl<const N: usize> From<&[u8; N]> for SharedBytes {
    fn from(bytes: &[u8; N]) -> SharedBytes {
        SharedBytes::from(bytes.as_slice())
    }
}

impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Deref for SharedBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl fmt::Debug for SharedBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl PartialEq for SharedBytes {
    fn eq(&self, other: &SharedBytes) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for SharedBytes {}

impl Hash for SharedBytes {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::{SharedBytes, SharedText};

    #[test]
    fn slices_share_storage_and_keep_utf8_boundaries() {
        let text = SharedText::from("aéz");
        let slice = text.slice(1, 3).expect("the range is valid");
        assert_eq!(slice.as_str(), "é");
        assert!(text.shares_storage(&slice));
        assert!(text.slice(1, 2).is_none());
    }

    #[test]
    fn clones_keep_the_cached_lookup_hash() {
        let text = SharedText::from("key");
        assert!(!text.has_cached_hash());
        let hash = text.lookup_hash();
        let clone = text.clone();
        assert_eq!(clone.lookup_hash(), hash);
        assert!(clone.has_cached_hash());
        assert!(text.shares_storage(&clone));
    }

    #[test]
    fn byte_slices_share_storage_and_keep_binary_data() {
        let bytes = SharedBytes::from(vec![0, 0xff, 2, 3]);
        let slice = bytes.slice(1, 3).expect("the range is valid");
        assert_eq!(slice.as_slice(), &[0xff, 2]);
        assert!(bytes.shares_storage(&slice));
        assert_eq!(slice.lookup_hash(), slice.clone().lookup_hash());
    }
}
