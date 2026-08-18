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

#[cfg(test)]
mod tests {
    use super::SharedText;

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
}
