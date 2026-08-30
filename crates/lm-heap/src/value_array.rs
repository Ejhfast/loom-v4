//! Stable owned storage for heap values.

use lm_value::Value;
use std::collections::TryReserveError;
use std::fmt;
use std::iter::FromIterator;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

/// One owned value array with a stable native layout.
#[repr(C)]
pub struct ValueArray {
    data: *mut Value,
    len: usize,
    capacity: usize,
}

/// Byte offset of the array data pointer.
pub const VALUE_ARRAY_DATA_OFFSET: usize = std::mem::offset_of!(ValueArray, data);
/// Byte offset of the array length.
pub const VALUE_ARRAY_LEN_OFFSET: usize = std::mem::offset_of!(ValueArray, len);
/// Byte offset of the array capacity.
pub const VALUE_ARRAY_CAPACITY_OFFSET: usize = std::mem::offset_of!(ValueArray, capacity);
/// Size of one native value-array record.
pub const VALUE_ARRAY_SIZE: usize = std::mem::size_of::<ValueArray>();

const _: () = assert!(VALUE_ARRAY_DATA_OFFSET == 0);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(VALUE_ARRAY_LEN_OFFSET == 8);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(VALUE_ARRAY_CAPACITY_OFFSET == 16);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(VALUE_ARRAY_SIZE == 24);

impl ValueArray {
    /// Create one empty array.
    pub fn new() -> ValueArray {
        ValueArray {
            data: NonNull::<Value>::dangling().as_ptr(),
            len: 0,
            capacity: 0,
        }
    }

    /// Return the current allocation capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Return this array as one slice.
    pub fn as_slice(&self) -> &[Value] {
        self
    }

    /// Return this array as one mutable slice.
    pub fn as_mut_slice(&mut self) -> &mut [Value] {
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
    pub fn push(&mut self, value: Value) {
        self.vector().push(value);
    }

    /// Remove and return the last value.
    pub fn pop(&mut self) -> Option<Value> {
        self.vector().pop()
    }

    /// Insert one value at an index.
    pub fn insert(&mut self, index: usize, value: Value) {
        self.vector().insert(index, value);
    }

    /// Remove and return one value.
    pub fn remove(&mut self, index: usize) -> Value {
        self.vector().remove(index)
    }

    /// Remove one value without preserving order.
    pub fn swap_remove(&mut self, index: usize) -> Value {
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

    /// Convert this array into the standard owned form.
    pub fn into_vec(self) -> Vec<Value> {
        let held = ManuallyDrop::new(self);
        // SAFETY: `ValueArray` owns this exact `Vec` allocation.
        unsafe { Vec::from_raw_parts(held.data, held.len, held.capacity) }
    }

    fn vector(&mut self) -> VectorGuard<'_> {
        let data = self.data;
        let len = self.len;
        let capacity = self.capacity;
        self.data = NonNull::<Value>::dangling().as_ptr();
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

impl Default for ValueArray {
    fn default() -> ValueArray {
        ValueArray::new()
    }
}

impl Drop for ValueArray {
    fn drop(&mut self) {
        // SAFETY: This array owns one allocation with these raw parts.
        unsafe {
            drop(Vec::from_raw_parts(self.data, self.len, self.capacity));
        }
    }
}

impl Clone for ValueArray {
    fn clone(&self) -> ValueArray {
        self.as_slice().to_vec().into()
    }
}

impl fmt::Debug for ValueArray {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(formatter)
    }
}

impl PartialEq for ValueArray {
    fn eq(&self, other: &ValueArray) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for ValueArray {}

impl Deref for ValueArray {
    type Target = [Value];

    fn deref(&self) -> &[Value] {
        // SAFETY: `data` names `len` initialized values owned by this array.
        unsafe { std::slice::from_raw_parts(self.data, self.len) }
    }
}

impl DerefMut for ValueArray {
    fn deref_mut(&mut self) -> &mut [Value] {
        // SAFETY: This mutable borrow has exclusive access to the array.
        unsafe { std::slice::from_raw_parts_mut(self.data, self.len) }
    }
}

impl AsRef<[Value]> for ValueArray {
    fn as_ref(&self) -> &[Value] {
        self
    }
}

impl AsMut<[Value]> for ValueArray {
    fn as_mut(&mut self) -> &mut [Value] {
        self
    }
}

impl From<Vec<Value>> for ValueArray {
    fn from(mut values: Vec<Value>) -> ValueArray {
        let out = ValueArray {
            data: values.as_mut_ptr(),
            len: values.len(),
            capacity: values.capacity(),
        };
        std::mem::forget(values);
        out
    }
}

impl From<ValueArray> for Vec<Value> {
    fn from(values: ValueArray) -> Vec<Value> {
        values.into_vec()
    }
}

impl FromIterator<Value> for ValueArray {
    fn from_iter<T: IntoIterator<Item = Value>>(iter: T) -> ValueArray {
        iter.into_iter().collect::<Vec<_>>().into()
    }
}

impl Extend<Value> for ValueArray {
    fn extend<T: IntoIterator<Item = Value>>(&mut self, iter: T) {
        self.vector().extend(iter);
    }
}

impl IntoIterator for ValueArray {
    type Item = Value;
    type IntoIter = std::vec::IntoIter<Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl<'a> IntoIterator for &'a ValueArray {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut ValueArray {
    type Item = &'a mut Value;
    type IntoIter = std::slice::IterMut<'a, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

// SAFETY: The owned allocation contains only `Value`, which is `Send`.
unsafe impl Send for ValueArray {}
// SAFETY: Shared access exposes only immutable `Value` references.
unsafe impl Sync for ValueArray {}

struct VectorGuard<'a> {
    owner: &'a mut ValueArray,
    vector: ManuallyDrop<Vec<Value>>,
}

impl Deref for VectorGuard<'_> {
    type Target = Vec<Value>;

    fn deref(&self) -> &Vec<Value> {
        &self.vector
    }
}

impl DerefMut for VectorGuard<'_> {
    fn deref_mut(&mut self) -> &mut Vec<Value> {
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
