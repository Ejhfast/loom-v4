//! Stable owned array storage.

use lm_value::Value;
use std::collections::TryReserveError;
use std::fmt;
use std::iter::FromIterator;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

/// One owned array with a stable native layout.
#[repr(C)]
pub struct OwnedArray<T> {
    data: *mut T,
    len: usize,
    capacity: usize,
}

/// One fixed owned slice with a stable native layout.
#[repr(C)]
pub struct OwnedSlice<T> {
    data: *mut T,
    len: usize,
}

/// One owned array of canonical heap values.
pub type ValueArray = OwnedArray<Value>;

/// Byte offset of the array data pointer.
pub const OWNED_ARRAY_DATA_OFFSET: usize = std::mem::offset_of!(OwnedArray<u8>, data);
/// Byte offset of the array length.
pub const OWNED_ARRAY_LEN_OFFSET: usize = std::mem::offset_of!(OwnedArray<u8>, len);
/// Byte offset of the array capacity.
pub const OWNED_ARRAY_CAPACITY_OFFSET: usize = std::mem::offset_of!(OwnedArray<u8>, capacity);
/// Size of one native value-array record.
pub const OWNED_ARRAY_SIZE: usize = std::mem::size_of::<OwnedArray<u8>>();

/// Byte offset of the owned-slice data pointer.
pub const OWNED_SLICE_DATA_OFFSET: usize = std::mem::offset_of!(OwnedSlice<u8>, data);
/// Byte offset of the owned-slice length.
pub const OWNED_SLICE_LEN_OFFSET: usize = std::mem::offset_of!(OwnedSlice<u8>, len);
/// Size of one owned-slice record.
pub const OWNED_SLICE_SIZE: usize = std::mem::size_of::<OwnedSlice<u8>>();

/// Byte offset of the value-array data pointer.
pub const VALUE_ARRAY_DATA_OFFSET: usize = OWNED_ARRAY_DATA_OFFSET;
/// Byte offset of the value-array length.
pub const VALUE_ARRAY_LEN_OFFSET: usize = OWNED_ARRAY_LEN_OFFSET;
/// Byte offset of the value-array capacity.
pub const VALUE_ARRAY_CAPACITY_OFFSET: usize = OWNED_ARRAY_CAPACITY_OFFSET;
/// Size of one native value-array record.
pub const VALUE_ARRAY_SIZE: usize = OWNED_ARRAY_SIZE;

const _: () = assert!(VALUE_ARRAY_DATA_OFFSET == 0);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(VALUE_ARRAY_LEN_OFFSET == 8);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(VALUE_ARRAY_CAPACITY_OFFSET == 16);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(VALUE_ARRAY_SIZE == 24);

impl<T> OwnedArray<T> {
    /// Create one empty array.
    pub fn new() -> OwnedArray<T> {
        OwnedArray {
            data: NonNull::<T>::dangling().as_ptr(),
            len: 0,
            capacity: 0,
        }
    }

    /// Return the current allocation capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Return this array as one slice.
    pub fn as_slice(&self) -> &[T] {
        self
    }

    /// Return this array as one mutable slice.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        self
    }

    /// Reserve capacity for at least `additional` more values.
    pub fn reserve(&mut self, additional: usize) {
        self.vector().reserve(additional);
    }

    /// Reserve exact capacity for `additional` more values.
    pub fn reserve_exact(&mut self, additional: usize) {
        self.vector().reserve_exact(additional);
    }

    /// Try to reserve capacity for at least `additional` more values.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.vector().try_reserve(additional)
    }

    /// Try to reserve exact capacity for `additional` more values.
    pub fn try_reserve_exact(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.vector().try_reserve_exact(additional)
    }

    /// Append one value.
    pub fn push(&mut self, value: T) {
        self.vector().push(value);
    }

    /// Remove and return the last value.
    pub fn pop(&mut self) -> Option<T> {
        self.vector().pop()
    }

    /// Insert one value at an index.
    pub fn insert(&mut self, index: usize, value: T) {
        self.vector().insert(index, value);
    }

    /// Remove and return one value.
    pub fn remove(&mut self, index: usize) -> T {
        self.vector().remove(index)
    }

    /// Remove one value without preserving order.
    pub fn swap_remove(&mut self, index: usize) -> T {
        self.vector().swap_remove(index)
    }

    /// Truncate this array to at most `len` values.
    pub fn truncate(&mut self, len: usize) {
        self.vector().truncate(len);
    }

    /// Remove all values.
    pub fn clear(&mut self) {
        self.vector().clear();
    }

    /// Keep only elements that satisfy one predicate.
    pub fn retain<F>(&mut self, keep: F)
    where
        F: FnMut(&T) -> bool,
    {
        self.vector().retain(keep);
    }

    /// Convert this array into the standard owned form.
    pub fn into_vec(self) -> Vec<T> {
        let held = ManuallyDrop::new(self);
        // SAFETY: `OwnedArray` owns this exact `Vec` allocation.
        unsafe { Vec::from_raw_parts(held.data, held.len, held.capacity) }
    }

    fn vector(&mut self) -> VectorGuard<'_, T> {
        let data = self.data;
        let len = self.len;
        let capacity = self.capacity;
        self.data = NonNull::<T>::dangling().as_ptr();
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

impl<T> OwnedSlice<T> {
    /// Create one empty owned slice.
    pub fn new() -> OwnedSlice<T> {
        OwnedSlice {
            data: NonNull::<T>::dangling().as_ptr(),
            len: 0,
        }
    }

    /// Return this owned slice as one slice.
    pub fn as_slice(&self) -> &[T] {
        self
    }

    /// Return this owned slice as one mutable slice.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        self
    }
}

impl<T> Default for OwnedSlice<T> {
    fn default() -> OwnedSlice<T> {
        OwnedSlice::new()
    }
}

impl<T> Drop for OwnedSlice<T> {
    fn drop(&mut self) {
        let slice = std::ptr::slice_from_raw_parts_mut(self.data, self.len);
        // SAFETY: This slice owns one exact boxed-slice allocation.
        unsafe {
            drop(Box::from_raw(slice));
        }
    }
}

impl<T: Clone> Clone for OwnedSlice<T> {
    fn clone(&self) -> OwnedSlice<T> {
        self.as_slice().to_vec().into()
    }
}

impl<T: fmt::Debug> fmt::Debug for OwnedSlice<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(formatter)
    }
}

impl<T> Deref for OwnedSlice<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        // SAFETY: `data` names `len` initialized elements.
        unsafe { std::slice::from_raw_parts(self.data, self.len) }
    }
}

impl<T> DerefMut for OwnedSlice<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        // SAFETY: This mutable borrow has exclusive slice access.
        unsafe { std::slice::from_raw_parts_mut(self.data, self.len) }
    }
}

impl<T> From<Vec<T>> for OwnedSlice<T> {
    fn from(values: Vec<T>) -> OwnedSlice<T> {
        let mut values = values.into_boxed_slice();
        let out = OwnedSlice {
            data: values.as_mut_ptr(),
            len: values.len(),
        };
        std::mem::forget(values);
        out
    }
}

// SAFETY: The owned allocation contains only `Send` elements.
unsafe impl<T: Send> Send for OwnedSlice<T> {}
// SAFETY: Shared access exposes only immutable `Sync` elements.
unsafe impl<T: Sync> Sync for OwnedSlice<T> {}

impl<T> Default for OwnedArray<T> {
    fn default() -> OwnedArray<T> {
        OwnedArray::new()
    }
}

impl<T> Drop for OwnedArray<T> {
    fn drop(&mut self) {
        // SAFETY: This array owns one allocation with these raw parts.
        unsafe {
            drop(Vec::from_raw_parts(self.data, self.len, self.capacity));
        }
    }
}

impl<T: Clone> Clone for OwnedArray<T> {
    fn clone(&self) -> OwnedArray<T> {
        self.as_slice().to_vec().into()
    }
}

impl<T: fmt::Debug> fmt::Debug for OwnedArray<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(formatter)
    }
}

impl<T: PartialEq> PartialEq for OwnedArray<T> {
    fn eq(&self, other: &OwnedArray<T>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Eq> Eq for OwnedArray<T> {}

impl<T> Deref for OwnedArray<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        // SAFETY: `data` names `len` initialized values owned by this array.
        unsafe { std::slice::from_raw_parts(self.data, self.len) }
    }
}

impl<T> DerefMut for OwnedArray<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        // SAFETY: This mutable borrow has exclusive access to the array.
        unsafe { std::slice::from_raw_parts_mut(self.data, self.len) }
    }
}

impl<T> AsRef<[T]> for OwnedArray<T> {
    fn as_ref(&self) -> &[T] {
        self
    }
}

impl<T> AsMut<[T]> for OwnedArray<T> {
    fn as_mut(&mut self) -> &mut [T] {
        self
    }
}

impl<T> From<Vec<T>> for OwnedArray<T> {
    fn from(mut values: Vec<T>) -> OwnedArray<T> {
        let out = OwnedArray {
            data: values.as_mut_ptr(),
            len: values.len(),
            capacity: values.capacity(),
        };
        std::mem::forget(values);
        out
    }
}

impl<T> From<OwnedArray<T>> for Vec<T> {
    fn from(values: OwnedArray<T>) -> Vec<T> {
        values.into_vec()
    }
}

impl<T> FromIterator<T> for OwnedArray<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> OwnedArray<T> {
        iter.into_iter().collect::<Vec<_>>().into()
    }
}

impl<T> Extend<T> for OwnedArray<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        self.vector().extend(iter);
    }
}

impl<T> IntoIterator for OwnedArray<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl<'a, T> IntoIterator for &'a OwnedArray<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut OwnedArray<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

// SAFETY: The owned allocation contains only `Send` elements.
unsafe impl<T: Send> Send for OwnedArray<T> {}
// SAFETY: Shared access exposes only immutable `Sync` elements.
unsafe impl<T: Sync> Sync for OwnedArray<T> {}

struct VectorGuard<'a, T> {
    owner: &'a mut OwnedArray<T>,
    vector: ManuallyDrop<Vec<T>>,
}

impl<T> Deref for VectorGuard<'_, T> {
    type Target = Vec<T>;

    fn deref(&self) -> &Vec<T> {
        &self.vector
    }
}

impl<T> DerefMut for VectorGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        &mut self.vector
    }
}

impl<T> Drop for VectorGuard<'_, T> {
    fn drop(&mut self) {
        self.owner.data = self.vector.as_mut_ptr();
        self.owner.len = self.vector.len();
        self.owner.capacity = self.vector.capacity();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_operations_preserve_values_and_capacity() {
        let mut values: ValueArray = vec![Value::Int(1), Value::Int(3)].into();
        values.insert(1, Value::Int(2));
        values.push(Value::Int(4));
        assert_eq!(
            values.as_slice(),
            [Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)]
        );
        assert_eq!(values.remove(1), Value::Int(2));
        assert_eq!(values.pop(), Some(Value::Int(4)));
        assert_eq!(values.as_slice(), [Value::Int(1), Value::Int(3)]);
        assert!(values.capacity() >= values.len());
    }

    #[test]
    fn fallible_growth_keeps_the_array_valid() {
        let mut values = ValueArray::new();
        values
            .try_reserve_exact(4)
            .expect("the small reserve succeeds");
        values.extend([Value::Unit, Value::Bool(true)]);
        assert_eq!(values.as_slice(), [Value::Unit, Value::Bool(true)]);
    }

    #[test]
    fn vector_conversion_transfers_ownership() {
        let source = vec![Value::Int(4), Value::Int(2)];
        let array: ValueArray = source.into();
        let restored: Vec<Value> = array.into();
        assert_eq!(restored, [Value::Int(4), Value::Int(2)]);
    }
}
