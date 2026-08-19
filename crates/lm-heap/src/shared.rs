//! Shared byte storage for immutable text and binary values.

use std::collections::hash_map::RandomState;
use std::collections::TryReserveError;
use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher};
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

const STRING_RETENTION_FLOOR: usize = 4096;
const SCALAR_INDEX_STRIDE: usize = 64;
const MIN_BYTE_BUFFER_CAPACITY: usize = 8;
const UTF8_UNKNOWN: u8 = 0;
const UTF8_VALID: u8 = 1;
const UTF8_INVALID: u8 = 2;

/// Hash one internal lookup key with the process key.
pub fn process_lookup_hash<T: Hash>(value: T) -> u64 {
    static HASHER: OnceLock<RandomState> = OnceLock::new();
    HASHER.get_or_init(RandomState::new).hash_one(value)
}

fn amortized_growth(length: usize, capacity: usize, additional: usize) -> usize {
    let required = length.saturating_add(additional);
    if required <= capacity {
        return 0;
    }
    required
        .max(capacity.saturating_mul(2))
        .max(MIN_BYTE_BUFFER_CAPACITY)
        .saturating_sub(capacity)
}

/// One immutable byte allocation shared by text and binary spans.
#[derive(Debug)]
struct ByteAllocation {
    data: Vec<u8>,
}

impl ByteAllocation {
    fn new(data: Vec<u8>) -> ByteAllocation {
        ByteAllocation { data }
    }

    fn retained_capacity(&self) -> usize {
        self.data.capacity()
    }
}

/// One visible range inside a shared byte allocation.
#[derive(Debug, Clone)]
struct ByteSpan {
    storage: Arc<ByteAllocation>,
    start: usize,
    len: usize,
}

impl ByteSpan {
    fn from_vec(data: Vec<u8>) -> ByteSpan {
        let len = data.len();
        ByteSpan {
            storage: Arc::new(ByteAllocation::new(data)),
            start: 0,
            len,
        }
    }

    fn as_slice(&self) -> &[u8] {
        &self.storage.data[self.start..self.start + self.len]
    }

    fn slice(&self, start: usize, end: usize) -> Option<ByteSpan> {
        if start > end || end > self.len {
            return None;
        }
        Some(ByteSpan {
            storage: self.storage.clone(),
            start: self.start + start,
            len: end - start,
        })
    }

    fn retained_capacity(&self) -> usize {
        self.storage.retained_capacity()
    }

    fn shares_storage(&self, other: &ByteSpan) -> bool {
        Arc::ptr_eq(&self.storage, &other.storage)
    }
}

/// UTF-8 metadata for one validated span.
#[derive(Debug)]
struct TextRoot {
    span: ByteSpan,
    scalar_count: usize,
    ascii: bool,
    /// Byte positions for scalar 0, 64, 128, and later positions.
    scalar_index: OnceLock<Vec<usize>>,
}

impl TextRoot {
    fn from_valid_span(span: ByteSpan) -> TextRoot {
        // The caller validated this complete span.
        let text = unsafe { std::str::from_utf8_unchecked(span.as_slice()) };
        let ascii = text.is_ascii();
        let scalar_count = if ascii {
            text.len()
        } else {
            text.chars().count()
        };
        TextRoot::from_valid_parts(span, scalar_count, ascii)
    }

    fn from_valid_parts(span: ByteSpan, scalar_count: usize, ascii: bool) -> TextRoot {
        TextRoot {
            span,
            scalar_count,
            ascii,
            scalar_index: OnceLock::new(),
        }
    }

    fn as_str(&self) -> &str {
        // Construction validates this complete span.
        unsafe { std::str::from_utf8_unchecked(self.span.as_slice()) }
    }

    fn index(&self) -> &[usize] {
        self.scalar_index.get_or_init(|| {
            let mut index = Vec::new();
            index.push(0);
            if self.ascii {
                return index;
            }
            for (scalar, (byte, _)) in self.as_str().char_indices().enumerate() {
                if scalar != 0 && scalar % SCALAR_INDEX_STRIDE == 0 {
                    index.push(byte);
                }
            }
            index
        })
    }

    fn byte_of_scalar(&self, scalar: usize) -> Option<usize> {
        if scalar > self.scalar_count {
            return None;
        }
        if scalar == self.scalar_count {
            return Some(self.span.len);
        }
        if self.ascii {
            return Some(scalar);
        }
        let block = scalar / SCALAR_INDEX_STRIDE;
        let block_scalar = block * SCALAR_INDEX_STRIDE;
        let block_byte = *self.index().get(block)?;
        if block_scalar == scalar {
            return Some(block_byte);
        }
        self.as_str()[block_byte..]
            .char_indices()
            .nth(scalar - block_scalar)
            .map(|(byte, _)| block_byte + byte)
    }

    fn scalar_of_byte(&self, byte: usize) -> Option<usize> {
        if byte > self.span.len || !self.as_str().is_char_boundary(byte) {
            return None;
        }
        if self.ascii {
            return Some(byte);
        }
        let index = self.index();
        let block = match index.binary_search(&byte) {
            Ok(block) => return Some(block * SCALAR_INDEX_STRIDE),
            Err(next) => next.saturating_sub(1),
        };
        let block_byte = *index.get(block)?;
        let within = self.as_str()[block_byte..byte].chars().count();
        Some(block * SCALAR_INDEX_STRIDE + within)
    }
}

/// Immutable UTF-8 text with shared storage and cached metadata.
pub struct SharedText {
    root: Arc<TextRoot>,
    byte_start: usize,
    byte_len: usize,
    scalar_start: usize,
    scalar_len: usize,
    lookup_hash: AtomicU64,
}

impl SharedText {
    /// Make an empty text value.
    pub fn new() -> SharedText {
        SharedText::default()
    }

    fn from_valid_span(span: ByteSpan) -> SharedText {
        SharedText::from_root(Arc::new(TextRoot::from_valid_span(span)))
    }

    fn from_root(root: Arc<TextRoot>) -> SharedText {
        SharedText {
            byte_start: 0,
            byte_len: root.span.len,
            scalar_start: 0,
            scalar_len: root.scalar_count,
            root,
            lookup_hash: AtomicU64::new(0),
        }
    }

    fn retention_limit(visible_len: usize) -> usize {
        STRING_RETENTION_FLOOR.max(visible_len.saturating_mul(2))
    }

    /// Make bounded text from an owned UTF-8 buffer.
    pub fn try_from_string(text: String) -> Result<SharedText, TryReserveError> {
        let ascii = text.is_ascii();
        let scalar_count = if ascii {
            text.len()
        } else {
            text.chars().count()
        };
        SharedText::try_from_string_parts(text, scalar_count, ascii)
    }

    /// Make bounded text from an owned buffer and known metadata.
    pub fn try_from_string_parts(
        text: String,
        scalar_count: usize,
        ascii: bool,
    ) -> Result<SharedText, TryReserveError> {
        debug_assert_eq!(text.chars().count(), scalar_count);
        debug_assert_eq!(text.is_ascii(), ascii);
        let mut bytes = text.into_bytes();
        let limit = SharedText::retention_limit(bytes.len());
        if bytes.capacity() > limit {
            let mut compact = Vec::new();
            compact.try_reserve_exact(bytes.len())?;
            compact.extend_from_slice(&bytes);
            bytes = compact;
        }
        let root = TextRoot::from_valid_parts(ByteSpan::from_vec(bytes), scalar_count, ascii);
        Ok(SharedText::from_root(Arc::new(root)))
    }

    /// Copy UTF-8 text into bounded storage.
    pub fn try_from_str(text: &str) -> Result<SharedText, TryReserveError> {
        let ascii = text.is_ascii();
        let scalar_count = if ascii {
            text.len()
        } else {
            text.chars().count()
        };
        SharedText::try_from_str_parts(text, scalar_count, ascii)
    }

    /// Copy UTF-8 text and retain its known metadata.
    pub fn try_from_str_parts(
        text: &str,
        scalar_count: usize,
        ascii: bool,
    ) -> Result<SharedText, TryReserveError> {
        let mut buffer = String::new();
        buffer.try_reserve_exact(text.len())?;
        buffer.push_str(text);
        SharedText::try_from_string_parts(buffer, scalar_count, ascii)
    }

    fn needs_compaction(&self) -> bool {
        self.retained_capacity() > Self::retention_limit(self.byte_len)
    }

    /// Get the visible text.
    pub fn as_str(&self) -> &str {
        let start = self.byte_start;
        let end = start + self.byte_len;
        &self.root.as_str()[start..end]
    }

    /// Get the visible byte length.
    pub fn len(&self) -> usize {
        self.byte_len
    }

    /// Test whether the visible text is empty.
    pub fn is_empty(&self) -> bool {
        self.byte_len == 0
    }

    /// Get the visible Unicode scalar count.
    pub fn char_count(&self) -> usize {
        self.scalar_len
    }

    /// Test whether all visible text bytes are ASCII.
    pub fn is_ascii(&self) -> bool {
        self.root.ascii || self.as_str().is_ascii()
    }

    /// Get the retained byte allocation capacity.
    pub fn retained_capacity(&self) -> usize {
        self.root.span.retained_capacity()
    }

    pub(crate) fn allocation_key(&self) -> usize {
        Arc::as_ptr(&self.root.span.storage) as usize
    }

    /// Test the durable String backing limit.
    pub fn has_bounded_retention(&self) -> bool {
        !self.needs_compaction()
    }

    /// Make a shared byte range.
    ///
    /// Both positions must be UTF-8 boundaries in the visible text.
    pub fn slice(&self, start: usize, end: usize) -> Option<SharedText> {
        if start > end || end > self.byte_len {
            return None;
        }
        let text = self.as_str();
        if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            return None;
        }
        let root_start = self.byte_start.checked_add(start)?;
        let root_end = self.byte_start.checked_add(end)?;
        let scalar_start = self.root.scalar_of_byte(root_start)?;
        let scalar_end = self.root.scalar_of_byte(root_end)?;
        Some(SharedText {
            root: self.root.clone(),
            byte_start: root_start,
            byte_len: end - start,
            scalar_start,
            scalar_len: scalar_end - scalar_start,
            lookup_hash: AtomicU64::new(0),
        })
    }

    /// Make a shared range from Unicode scalar positions.
    pub fn scalar_slice(&self, start: usize, length: usize) -> Option<SharedText> {
        let end = start.checked_add(length)?;
        if end > self.scalar_len {
            return None;
        }
        let root_start = self.scalar_start.checked_add(start)?;
        let root_end = root_start.checked_add(length)?;
        let byte_start = self.root.byte_of_scalar(root_start)?;
        let byte_end = self.root.byte_of_scalar(root_end)?;
        Some(SharedText {
            root: self.root.clone(),
            byte_start,
            byte_len: byte_end - byte_start,
            scalar_start: root_start,
            scalar_len: length,
            lookup_hash: AtomicU64::new(0),
        })
    }

    /// Read one Unicode scalar at a scalar position.
    pub fn scalar_at(&self, index: usize) -> Option<char> {
        if index >= self.scalar_len {
            return None;
        }
        if self.root.ascii {
            return Some(char::from(self.as_str().as_bytes()[index]));
        }
        let root_index = self.scalar_start.checked_add(index)?;
        let byte = self.root.byte_of_scalar(root_index)?;
        self.root.as_str()[byte..].chars().next()
    }

    /// Read one Unicode scalar at a UTF-8 byte boundary.
    pub fn scalar_at_byte(&self, index: usize) -> Option<char> {
        if index >= self.byte_len || !self.as_str().is_char_boundary(index) {
            return None;
        }
        self.as_str()[index..].chars().next()
    }

    /// Test one visible byte position as a UTF-8 boundary.
    pub fn is_char_boundary(&self, index: usize) -> bool {
        index <= self.byte_len && self.as_str().is_char_boundary(index)
    }

    /// Find text and return its scalar position.
    pub fn find_scalar(&self, needle: &SharedText) -> Option<usize> {
        let byte = self.as_str().find(needle.as_str())?;
        let root_byte = self.byte_start.checked_add(byte)?;
        self.root
            .scalar_of_byte(root_byte)?
            .checked_sub(self.scalar_start)
    }

    /// Find text and return its byte position.
    pub fn find_byte(&self, needle: &SharedText) -> Option<usize> {
        self.as_str().find(needle.as_str())
    }

    /// Join two values in new bounded storage.
    pub fn try_concat(&self, other: &SharedText) -> Result<SharedText, TryReserveError> {
        let mut text = String::new();
        text.try_reserve_exact(self.byte_len.saturating_add(other.byte_len))?;
        text.push_str(self.as_str());
        text.push_str(other.as_str());
        let scalar_count = self.scalar_len.saturating_add(other.scalar_len);
        let ascii = text.is_ascii();
        SharedText::try_from_string_parts(text, scalar_count, ascii)
    }

    /// Copy this text into bounded String storage when needed.
    pub fn bounded(&self) -> SharedText {
        if self.has_bounded_retention() {
            return self.clone();
        }
        self.compact()
    }

    /// Make bounded String storage with a fallible copy.
    pub fn try_bounded(&self) -> Result<SharedText, TryReserveError> {
        if self.has_bounded_retention() {
            return Ok(self.clone());
        }
        self.try_compact()
    }

    /// Copy this text into one exact visible allocation.
    pub fn compact(&self) -> SharedText {
        SharedText::from(self.as_str())
    }

    /// Copy this text with a fallible exact allocation.
    pub fn try_compact(&self) -> Result<SharedText, TryReserveError> {
        SharedText::try_from_str(self.as_str())
    }

    /// Share this text allocation as immutable bytes.
    pub fn bytes(&self) -> SharedBytes {
        let span = self
            .root
            .span
            .slice(self.byte_start, self.byte_start + self.byte_len)
            .expect("a text view stays inside its root");
        SharedBytes {
            span,
            lookup_hash: AtomicU64::new(0),
            utf8_state: AtomicU8::new(UTF8_VALID),
        }
    }

    /// Get the cached hash for map lookup.
    pub fn lookup_hash(&self) -> u64 {
        let cached = self.lookup_hash.load(Ordering::Relaxed);
        if cached != 0 {
            return cached;
        }
        let hash = process_lookup_hash(self.as_str());
        if hash != 0 {
            self.lookup_hash.store(hash, Ordering::Relaxed);
        }
        hash
    }

    /// Test whether this value shares its backing allocation.
    pub fn shares_storage(&self, other: &SharedText) -> bool {
        self.root.span.shares_storage(&other.root.span)
    }

    /// Test whether this text shares storage with binary data.
    pub fn shares_bytes_storage(&self, other: &SharedBytes) -> bool {
        self.root.span.shares_storage(&other.span)
    }

    /// Test whether this value has computed its lookup hash.
    pub fn has_cached_hash(&self) -> bool {
        self.lookup_hash.load(Ordering::Relaxed) != 0
    }
}

impl Default for SharedText {
    fn default() -> SharedText {
        SharedText::from(String::new())
    }
}

impl Clone for SharedText {
    fn clone(&self) -> SharedText {
        SharedText {
            root: self.root.clone(),
            byte_start: self.byte_start,
            byte_len: self.byte_len,
            scalar_start: self.scalar_start,
            scalar_len: self.scalar_len,
            lookup_hash: AtomicU64::new(self.lookup_hash.load(Ordering::Relaxed)),
        }
    }
}

impl From<String> for SharedText {
    fn from(text: String) -> SharedText {
        SharedText::try_from_string(text).expect("a String allocation failed")
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
    span: ByteSpan,
    lookup_hash: AtomicU64,
    utf8_state: AtomicU8,
}

impl SharedBytes {
    /// Make an empty byte value.
    pub fn new() -> SharedBytes {
        SharedBytes::default()
    }

    /// Get the visible bytes.
    pub fn as_slice(&self) -> &[u8] {
        self.span.as_slice()
    }

    /// Get the visible byte length.
    pub fn len(&self) -> usize {
        self.span.len
    }

    /// Test whether the visible bytes are empty.
    pub fn is_empty(&self) -> bool {
        self.span.len == 0
    }

    /// Get the retained byte allocation capacity.
    pub fn retained_capacity(&self) -> usize {
        self.span.retained_capacity()
    }

    pub(crate) fn allocation_key(&self) -> usize {
        Arc::as_ptr(&self.span.storage) as usize
    }

    /// Make a shared byte range.
    pub fn slice(&self, start: usize, end: usize) -> Option<SharedBytes> {
        Some(SharedBytes {
            span: self.span.slice(start, end)?,
            lookup_hash: AtomicU64::new(0),
            utf8_state: AtomicU8::new(UTF8_UNKNOWN),
        })
    }

    /// Copy this span into one exact visible allocation.
    pub fn compact(&self) -> SharedBytes {
        let compact = SharedBytes::from(self.as_slice());
        compact
            .utf8_state
            .store(self.utf8_state.load(Ordering::Relaxed), Ordering::Relaxed);
        compact
    }

    /// Copy this span with a fallible exact allocation.
    pub fn try_compact(&self) -> Result<SharedBytes, TryReserveError> {
        let compact = SharedBytes::try_from_slice(self.as_slice())?;
        compact
            .utf8_state
            .store(self.utf8_state.load(Ordering::Relaxed), Ordering::Relaxed);
        Ok(compact)
    }

    /// Copy bytes with a fallible exact allocation.
    pub fn try_from_slice(bytes: &[u8]) -> Result<SharedBytes, TryReserveError> {
        let mut buffer = Vec::new();
        buffer.try_reserve_exact(bytes.len())?;
        buffer.extend_from_slice(bytes);
        Ok(SharedBytes::from(buffer))
    }

    /// Validate this byte view as UTF-8 once.
    pub fn is_utf8(&self) -> bool {
        match self.utf8_state.load(Ordering::Relaxed) {
            UTF8_VALID => true,
            UTF8_INVALID => false,
            _ => {
                let valid = std::str::from_utf8(self.as_slice()).is_ok();
                self.utf8_state.store(
                    if valid { UTF8_VALID } else { UTF8_INVALID },
                    Ordering::Relaxed,
                );
                valid
            }
        }
    }

    /// Validate UTF-8 and return a shared text view.
    pub fn utf8_view(&self) -> Option<SharedText> {
        if !self.is_utf8() {
            return None;
        }
        Some(SharedText::from_valid_span(self.span.clone()))
    }

    /// Validate UTF-8 and return text with bounded retention.
    pub fn utf8_bounded(&self) -> Option<SharedText> {
        Some(self.utf8_view()?.bounded())
    }

    /// Validate UTF-8 and make bounded text with a fallible copy.
    pub fn try_utf8_bounded(&self) -> Result<Option<SharedText>, TryReserveError> {
        let Some(view) = self.utf8_view() else {
            return Ok(None);
        };
        Ok(Some(view.try_bounded()?))
    }

    /// Join two values in new storage.
    pub fn try_concat(&self, other: &SharedBytes) -> Result<SharedBytes, TryReserveError> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(self.len().saturating_add(other.len()))?;
        bytes.extend_from_slice(self.as_slice());
        bytes.extend_from_slice(other.as_slice());
        Ok(SharedBytes::from(bytes))
    }

    /// Get the cached hash for map lookup.
    pub fn lookup_hash(&self) -> u64 {
        let cached = self.lookup_hash.load(Ordering::Relaxed);
        if cached != 0 {
            return cached;
        }
        let hash = process_lookup_hash(self.as_slice());
        if hash != 0 {
            self.lookup_hash.store(hash, Ordering::Relaxed);
        }
        hash
    }

    /// Test whether this value shares its backing allocation.
    pub fn shares_storage(&self, other: &SharedBytes) -> bool {
        self.span.shares_storage(&other.span)
    }

    /// Test whether this value has computed its lookup hash.
    pub fn has_cached_hash(&self) -> bool {
        self.lookup_hash.load(Ordering::Relaxed) != 0
    }
}

impl Default for SharedBytes {
    fn default() -> SharedBytes {
        SharedBytes::from(Vec::new())
    }
}

impl Clone for SharedBytes {
    fn clone(&self) -> SharedBytes {
        SharedBytes {
            span: self.span.clone(),
            lookup_hash: AtomicU64::new(self.lookup_hash.load(Ordering::Relaxed)),
            utf8_state: AtomicU8::new(self.utf8_state.load(Ordering::Relaxed)),
        }
    }
}

impl From<Vec<u8>> for SharedBytes {
    fn from(bytes: Vec<u8>) -> SharedBytes {
        SharedBytes {
            span: ByteSpan::from_vec(bytes),
            lookup_hash: AtomicU64::new(0),
            utf8_state: AtomicU8::new(UTF8_UNKNOWN),
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

/// A unique mutable UTF-8 buffer with an explicit finished state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeStringBuilder {
    buffer: Option<String>,
    scalar_len: usize,
    ascii: bool,
}

impl NativeStringBuilder {
    /// Make an empty active builder.
    pub fn new() -> NativeStringBuilder {
        NativeStringBuilder {
            buffer: Some(String::new()),
            scalar_len: 0,
            ascii: true,
        }
    }

    /// Get the active buffer.
    pub fn buffer(&self) -> Option<&String> {
        self.buffer.as_ref()
    }

    /// Reserve more active buffer capacity.
    pub fn try_reserve(&mut self, additional: usize) -> Result<bool, TryReserveError> {
        let Some(buffer) = self.buffer.as_mut() else {
            return Ok(false);
        };
        buffer.try_reserve(additional)?;
        Ok(true)
    }

    /// Get the maximum amortized capacity increase for one reserve.
    pub fn reserve_growth(&self, additional: usize) -> Option<usize> {
        self.buffer
            .as_ref()
            .map(|buffer| amortized_growth(buffer.len(), buffer.capacity(), additional))
    }

    /// Get the Unicode scalar length.
    pub fn scalar_len(&self) -> Option<usize> {
        self.buffer.as_ref().map(|_| self.scalar_len)
    }

    /// Get the UTF-8 byte length.
    pub fn byte_len(&self) -> Option<usize> {
        self.buffer.as_ref().map(String::len)
    }

    /// Get the active buffer ASCII state.
    pub fn is_ascii(&self) -> Option<bool> {
        self.buffer.as_ref().map(|_| self.ascii)
    }

    /// Append validated text.
    pub fn append(&mut self, text: &SharedText) -> bool {
        let Some(buffer) = self.buffer.as_mut() else {
            return false;
        };
        buffer.push_str(text.as_str());
        self.scalar_len = self.scalar_len.saturating_add(text.char_count());
        self.ascii &= text.is_ascii();
        true
    }

    /// Append validated UTF-8 text.
    pub fn append_str(&mut self, text: &str) -> bool {
        let Some(buffer) = self.buffer.as_mut() else {
            return false;
        };
        buffer.push_str(text);
        self.scalar_len = self.scalar_len.saturating_add(text.chars().count());
        self.ascii &= text.is_ascii();
        true
    }

    /// Append one integer in decimal form.
    pub fn append_int(&mut self, value: i64) -> bool {
        use std::fmt::Write as _;

        let Some(buffer) = self.buffer.as_mut() else {
            return false;
        };
        let old_len = buffer.len();
        write!(buffer, "{value}").expect("writing to String cannot fail");
        self.scalar_len = self.scalar_len.saturating_add(buffer.len() - old_len);
        true
    }

    /// Append one Unicode scalar.
    pub fn push(&mut self, value: char) -> bool {
        let Some(buffer) = self.buffer.as_mut() else {
            return false;
        };
        buffer.push(value);
        self.scalar_len = self.scalar_len.saturating_add(1);
        self.ascii &= value.is_ascii();
        true
    }

    /// Clear the active buffer.
    pub fn clear(&mut self) -> bool {
        let Some(buffer) = self.buffer.as_mut() else {
            return false;
        };
        buffer.clear();
        self.scalar_len = 0;
        self.ascii = true;
        true
    }

    /// Move the buffer and its metadata out, then finish this builder.
    pub fn finish(&mut self) -> Option<(String, usize, bool)> {
        let buffer = self.buffer.take()?;
        let scalar_len = std::mem::take(&mut self.scalar_len);
        let ascii = std::mem::replace(&mut self.ascii, true);
        Some((buffer, scalar_len, ascii))
    }

    /// Get the retained mutable capacity.
    pub fn retained_capacity(&self) -> usize {
        self.buffer
            .as_ref()
            .map(|value| value.capacity())
            .unwrap_or(0)
    }

    /// Restore an active builder from snapshot text.
    pub fn from_string(buffer: String) -> NativeStringBuilder {
        let ascii = buffer.is_ascii();
        let scalar_len = if ascii {
            buffer.len()
        } else {
            buffer.chars().count()
        };
        NativeStringBuilder {
            buffer: Some(buffer),
            scalar_len,
            ascii,
        }
    }

    /// Restore a finished builder.
    pub fn finished() -> NativeStringBuilder {
        NativeStringBuilder {
            buffer: None,
            scalar_len: 0,
            ascii: true,
        }
    }

    /// Copy this builder with a fallible buffer allocation.
    pub fn try_clone_buffer(&self) -> Result<NativeStringBuilder, TryReserveError> {
        let Some(source) = self.buffer.as_ref() else {
            return Ok(NativeStringBuilder::finished());
        };
        let mut buffer = String::new();
        buffer.try_reserve_exact(source.len())?;
        buffer.push_str(source);
        Ok(NativeStringBuilder {
            buffer: Some(buffer),
            scalar_len: self.scalar_len,
            ascii: self.ascii,
        })
    }
}

impl Default for NativeStringBuilder {
    fn default() -> NativeStringBuilder {
        NativeStringBuilder::new()
    }
}

/// A unique mutable byte buffer with an explicit finished state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeByteBuffer {
    buffer: Option<Vec<u8>>,
}

impl NativeByteBuffer {
    /// Make an empty active buffer.
    pub fn new() -> NativeByteBuffer {
        NativeByteBuffer {
            buffer: Some(Vec::new()),
        }
    }

    /// Get the active bytes.
    pub fn buffer(&self) -> Option<&Vec<u8>> {
        self.buffer.as_ref()
    }

    /// Reserve more active buffer capacity.
    pub fn try_reserve(&mut self, additional: usize) -> Result<bool, TryReserveError> {
        let Some(buffer) = self.buffer.as_mut() else {
            return Ok(false);
        };
        buffer.try_reserve(additional)?;
        Ok(true)
    }

    /// Get the maximum amortized capacity increase for one reserve.
    pub fn reserve_growth(&self, additional: usize) -> Option<usize> {
        self.buffer
            .as_ref()
            .map(|buffer| amortized_growth(buffer.len(), buffer.capacity(), additional))
    }

    /// Append one byte to the active buffer.
    pub fn push(&mut self, byte: u8) -> bool {
        let Some(buffer) = self.buffer.as_mut() else {
            return false;
        };
        buffer.push(byte);
        true
    }

    /// Append immutable bytes to the active buffer.
    pub fn extend(&mut self, bytes: &SharedBytes) -> bool {
        let Some(buffer) = self.buffer.as_mut() else {
            return false;
        };
        buffer.extend_from_slice(bytes.as_slice());
        true
    }

    /// Get the active byte length.
    pub fn len(&self) -> Option<usize> {
        self.buffer.as_ref().map(Vec::len)
    }

    /// Test whether the active buffer is empty.
    pub fn is_empty(&self) -> Option<bool> {
        self.buffer.as_ref().map(Vec::is_empty)
    }

    /// Clear the active buffer.
    pub fn clear(&mut self) -> bool {
        let Some(buffer) = self.buffer.as_mut() else {
            return false;
        };
        buffer.clear();
        true
    }

    /// Move the bytes out and mark this buffer as finished.
    pub fn finish(&mut self) -> Option<Vec<u8>> {
        self.buffer.take()
    }

    /// Get the retained mutable capacity.
    pub fn retained_capacity(&self) -> usize {
        self.buffer
            .as_ref()
            .map(|value| value.capacity())
            .unwrap_or(0)
    }

    /// Restore an active buffer from snapshot bytes.
    pub fn from_vec(buffer: Vec<u8>) -> NativeByteBuffer {
        NativeByteBuffer {
            buffer: Some(buffer),
        }
    }

    /// Restore a finished buffer.
    pub fn finished() -> NativeByteBuffer {
        NativeByteBuffer { buffer: None }
    }

    /// Copy this buffer with a fallible allocation.
    pub fn try_clone_buffer(&self) -> Result<NativeByteBuffer, TryReserveError> {
        let Some(source) = self.buffer.as_ref() else {
            return Ok(NativeByteBuffer::finished());
        };
        let mut buffer = Vec::new();
        buffer.try_reserve_exact(source.len())?;
        buffer.extend_from_slice(source);
        Ok(NativeByteBuffer::from_vec(buffer))
    }
}

impl Default for NativeByteBuffer {
    fn default() -> NativeByteBuffer {
        NativeByteBuffer::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        NativeByteBuffer, NativeStringBuilder, SharedBytes, SharedText, UTF8_INVALID, UTF8_UNKNOWN,
        UTF8_VALID,
    };
    use std::sync::atomic::Ordering;

    #[test]
    fn scalar_slices_share_storage_and_keep_utf8_boundaries() {
        let text = SharedText::from("aé猫z");
        let slice = text.scalar_slice(1, 2).expect("the range is valid");
        assert_eq!(slice.as_str(), "é猫");
        assert_eq!(slice.char_count(), 2);
        assert!(text.shares_storage(&slice));
        assert!(text.slice(1, 2).is_none());
        assert_eq!(text.scalar_at_byte(1), Some('é'));
        assert_eq!(text.scalar_at_byte(2), None);
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
    fn text_and_bytes_share_one_allocation() {
        let text = SharedText::from("aéz");
        let bytes = text.bytes();
        assert_eq!(bytes.utf8_state.load(Ordering::Relaxed), UTF8_VALID);
        let view = bytes.utf8_view().expect("the bytes contain UTF-8");
        assert_eq!(bytes.as_slice(), "aéz".as_bytes());
        assert!(text.shares_bytes_storage(&bytes));
        assert!(text.shares_storage(&view));
    }

    #[test]
    fn durable_text_compacts_a_small_view_of_large_bytes() {
        let bytes = SharedBytes::from(vec![b'x'; 32 * 1024]);
        let small = bytes.slice(0, 2).expect("the range is valid");
        let view = small.utf8_view().expect("the bytes contain UTF-8");
        assert!(!view.has_bounded_retention());
        let durable = small.utf8_bounded().expect("the bytes contain UTF-8");
        assert!(durable.has_bounded_retention());
        assert!(!durable.shares_bytes_storage(&bytes));
    }

    #[test]
    fn byte_slices_share_storage_and_keep_binary_data() {
        let bytes = SharedBytes::from(vec![0, 0xff, 2, 3]);
        let slice = bytes.slice(1, 3).expect("the range is valid");
        assert_eq!(slice.as_slice(), &[0xff, 2]);
        assert!(bytes.shares_storage(&slice));
        assert_eq!(slice.lookup_hash(), slice.clone().lookup_hash());
    }

    #[test]
    fn byte_views_cache_utf8_validation() {
        let valid = SharedBytes::from("é".as_bytes());
        assert_eq!(valid.utf8_state.load(Ordering::Relaxed), UTF8_UNKNOWN);
        assert!(valid.is_utf8());
        assert_eq!(valid.utf8_state.load(Ordering::Relaxed), UTF8_VALID);

        let invalid = SharedBytes::from(&[0xff]);
        assert!(!invalid.is_utf8());
        assert_eq!(invalid.utf8_state.load(Ordering::Relaxed), UTF8_INVALID);
    }

    #[test]
    fn builder_growth_estimates_cover_amortized_reserves() {
        let mut text = NativeStringBuilder::new();
        for source in ["a", "bcdefgh", "longer text"] {
            let before = text.retained_capacity();
            let growth = text
                .reserve_growth(source.len())
                .expect("the builder is active");
            assert!(text
                .try_reserve(source.len())
                .expect("the reserve succeeds"));
            assert!(text.retained_capacity() - before <= growth);
            assert!(text.append_str(source));
        }

        let mut bytes = NativeByteBuffer::new();
        for additional in [1, 7, 1, 16] {
            let before = bytes.retained_capacity();
            let growth = bytes
                .reserve_growth(additional)
                .expect("the buffer is active");
            assert!(bytes.try_reserve(additional).expect("the reserve succeeds"));
            assert!(bytes.retained_capacity() - before <= growth);
            for _ in 0..additional {
                assert!(bytes.push(0));
            }
        }
    }

    #[test]
    fn string_builder_finish_moves_text_metadata() {
        let mut builder = NativeStringBuilder::new();
        assert!(builder.append_str("aé猫"));
        let (buffer, scalar_count, ascii) = builder.finish().expect("the builder is active");
        let text = SharedText::try_from_string_parts(buffer, scalar_count, ascii)
            .expect("the text allocation succeeds");
        assert_eq!(text.as_str(), "aé猫");
        assert_eq!(text.char_count(), 3);
        assert!(!text.is_ascii());
        assert!(builder.finish().is_none());
    }
}
