//! Stable owned storage for mutable byte buffers.

use std::collections::TryReserveError;
use std::fmt;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::ptr::NonNull;

/// One owned byte array with a stable native layout.
#[repr(C)]
pub(crate) struct ByteArray {
    data: *mut u8,
    len: usize,
    capacity: usize,
}

/// Byte offset of the array data pointer.
pub(crate) const BYTE_ARRAY_DATA_OFFSET: usize = std::mem::offset_of!(ByteArray, data);
/// Byte offset of the array length.
pub(crate) const BYTE_ARRAY_LEN_OFFSET: usize = std::mem::offset_of!(ByteArray, len);
/// Byte offset of the array capacity.
pub(crate) const BYTE_ARRAY_CAPACITY_OFFSET: usize = std::mem::offset_of!(ByteArray, capacity);

const _: () = assert!(BYTE_ARRAY_DATA_OFFSET == 0);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(BYTE_ARRAY_LEN_OFFSET == 8);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(BYTE_ARRAY_CAPACITY_OFFSET == 16);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<ByteArray>() == 24);

impl ByteArray {
    /// Create one empty array.
    pub(crate) fn new() -> ByteArray {
        ByteArray {
            data: NonNull::<u8>::dangling().as_ptr(),
            len: 0,
            capacity: 0,
        }
    }

    /// Return the current allocation capacity.
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    /// Try to reserve capacity for more bytes.
    pub(crate) fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.vector().try_reserve(additional)
    }

    /// Append one byte.
    pub(crate) fn push(&mut self, value: u8) {
        self.vector().push(value);
    }

    /// Append one byte slice.
    pub(crate) fn extend_from_slice(&mut self, values: &[u8]) {
        self.vector().extend_from_slice(values);
    }

    /// Replace one byte when the index exists.
    pub(crate) fn set(&mut self, index: usize, value: u8) -> bool {
        if index >= self.len {
            return false;
        }
        // SAFETY: The index is inside the initialized allocation.
        unsafe {
            *self.data.add(index) = value;
        }
        true
    }

    /// Remove bytes after one length.
    pub(crate) fn truncate(&mut self, length: usize) {
        self.len = self.len.min(length);
    }

    /// Remove all bytes.
    pub(crate) fn clear(&mut self) {
        self.len = 0;
    }

    /// Transfer this allocation into a standard vector.
    pub(crate) fn into_vec(self) -> Vec<u8> {
        let held = ManuallyDrop::new(self);
        // SAFETY: `ByteArray` owns this exact vector allocation.
        unsafe { Vec::from_raw_parts(held.data, held.len, held.capacity) }
    }

    fn vector(&mut self) -> VectorGuard<'_> {
        let data = self.data;
        let len = self.len;
        let capacity = self.capacity;
        self.data = NonNull::<u8>::dangling().as_ptr();
        self.len = 0;
        self.capacity = 0;
        // SAFETY: This array owns one allocation with these raw parts.
        let vector = unsafe { Vec::from_raw_parts(data, len, capacity) };
        VectorGuard {
            owner: self,
            vector: ManuallyDrop::new(vector),
        }
    }
}

impl Default for ByteArray {
    fn default() -> ByteArray {
        ByteArray::new()
    }
}

impl Drop for ByteArray {
    fn drop(&mut self) {
        // SAFETY: This array owns one allocation with these raw parts.
        unsafe {
            drop(Vec::from_raw_parts(self.data, self.len, self.capacity));
        }
    }
}

impl Clone for ByteArray {
    fn clone(&self) -> ByteArray {
        self.as_ref().to_vec().into()
    }
}

impl fmt::Debug for ByteArray {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(formatter)
    }
}

impl PartialEq for ByteArray {
    fn eq(&self, other: &ByteArray) -> bool {
        self.as_ref() == other.as_ref()
    }
}

impl Eq for ByteArray {}

impl Deref for ByteArray {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: `data` names `len` initialized bytes owned by this array.
        unsafe { std::slice::from_raw_parts(self.data, self.len) }
    }
}

impl AsRef<[u8]> for ByteArray {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl From<Vec<u8>> for ByteArray {
    fn from(mut values: Vec<u8>) -> ByteArray {
        let array = ByteArray {
            data: values.as_mut_ptr(),
            len: values.len(),
            capacity: values.capacity(),
        };
        std::mem::forget(values);
        array
    }
}

struct VectorGuard<'a> {
    owner: &'a mut ByteArray,
    vector: ManuallyDrop<Vec<u8>>,
}

impl std::ops::Deref for VectorGuard<'_> {
    type Target = Vec<u8>;

    fn deref(&self) -> &Vec<u8> {
        &self.vector
    }
}

impl std::ops::DerefMut for VectorGuard<'_> {
    fn deref_mut(&mut self) -> &mut Vec<u8> {
        &mut self.vector
    }
}

impl Drop for VectorGuard<'_> {
    fn drop(&mut self) {
        self.owner.data = self.vector.as_mut_ptr();
        self.owner.len = self.vector.len();
        self.owner.capacity = self.vector.capacity();
    }
}

// SAFETY: The owned allocation contains only bytes.
unsafe impl Send for ByteArray {}
// SAFETY: Shared access exposes only immutable bytes.
unsafe impl Sync for ByteArray {}

#[cfg(test)]
mod tests {
    use super::ByteArray;

    #[test]
    fn array_operations_preserve_bytes_and_capacity() {
        let mut bytes: ByteArray = vec![1, 2].into();
        bytes.try_reserve(2).expect("the small reserve succeeds");
        bytes.push(3);
        bytes.extend_from_slice(&[4, 5]);
        assert_eq!(bytes.as_ref(), [1, 2, 3, 4, 5]);
        assert!(bytes.capacity() >= bytes.len());
        assert!(bytes.set(1, 9));
        assert!(!bytes.set(5, 0));
        bytes.truncate(3);
        bytes.truncate(8);
        assert_eq!(bytes.as_ref(), [1, 9, 3]);
        bytes.clear();
        assert!(bytes.is_empty());
    }

    #[test]
    fn vector_conversion_transfers_ownership() {
        let bytes: ByteArray = vec![1, 2, 3].into();
        assert_eq!(bytes.into_vec(), [1, 2, 3]);
    }
}
