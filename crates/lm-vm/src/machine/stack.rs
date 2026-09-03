//! Canonical frame, local, and operand operations.

use super::*;

impl Machine {
    pub fn push(&mut self, value: Value) -> Result<(), FaultCode> {
        // A canonical stack mutation observes retained native state.
        // Materialize it before the mutation.
        if self.native_continuation.is_some()
            && crate::jit::materialize_native_continuation(self).is_err()
        {
            return Err(FaultCode::MalformedState);
        }
        if self.vm.operands.len() + self.vm.locals.len() >= self.config.max_stack_values as usize {
            return Err(FaultCode::StackLimit);
        }
        self.vm.operands.push(value);
        Ok(())
    }

    /// The arena position of one local slot of the running frame.
    ///
    /// The frame states its own local base, so a restored machine can
    /// state one the arena does not hold. The caller reads the arena
    /// through `get`, so the one bounds test of the slice answers the
    /// position as well.
    #[inline]
    pub(super) fn local_at(&self, slot: u32) -> Result<usize, FaultCode> {
        let base = self.vm.frames.last().ok_or(BAD_STATE)?.base_local;
        Ok(base as usize + slot as usize)
    }

    /// The operand `back` places below the top of the stack.
    #[inline]
    pub(super) fn peek(&self, back: usize) -> Result<Value, FaultCode> {
        let at = self
            .vm
            .operands
            .len()
            .checked_sub(back + 1)
            .ok_or(BAD_STATE)?;
        Ok(self.vm.operands[at])
    }

    /// Take the top operand.
    ///
    /// The independent verifier proves the operand type at every
    /// program point of every executed function, so verified code
    /// never reaches the error arm of this call or of the readers
    /// below.
    ///
    /// A restored machine states its own operand arena. Admission
    /// proves the structure of that arena and no type of it, so the
    /// readers test the tag and raise `TypeMismatch`. A short stack
    /// raises `MalformedState`. Both stop the machine and leave the
    /// host running.
    #[inline]
    pub(super) fn pop(&mut self) -> Result<Value, FaultCode> {
        self.vm.operands.pop().ok_or(BAD_STATE)
    }

    #[inline]
    pub(super) fn pop_int(&mut self) -> Result<i64, FaultCode> {
        match self.pop()? {
            Value::Int(v) => Ok(v),
            _ => Err(BAD_TYPE),
        }
    }

    #[inline]
    pub(super) fn pop_float_bits(&mut self) -> Result<u64, FaultCode> {
        match self.pop()? {
            Value::Float(bits) => Ok(bits),
            _ => Err(BAD_TYPE),
        }
    }

    #[inline]
    pub(super) fn pop_float(&mut self) -> Result<f64, FaultCode> {
        self.pop_float_bits().map(f64::from_bits)
    }

    #[inline]
    pub(super) fn push_float(&mut self, value: f64) -> Result<(), FaultCode> {
        self.push(Value::Float(canonical_float_bits(value.to_bits())))
    }

    #[inline]
    pub(super) fn pop_bool(&mut self) -> Result<bool, FaultCode> {
        match self.pop()? {
            Value::Bool(v) => Ok(v),
            _ => Err(BAD_TYPE),
        }
    }

    #[inline]
    pub(super) fn pop_char(&mut self) -> Result<char, FaultCode> {
        match self.pop()? {
            Value::Char(value) => Ok(value),
            _ => Err(BAD_TYPE),
        }
    }

    #[inline]
    pub(super) fn pop_obj(&mut self) -> Result<ObjRef, FaultCode> {
        match self.pop()? {
            Value::Obj(r) => Ok(r),
            _ => Err(BAD_TYPE),
        }
    }

    /// Get immutable text from a String or Substring object.
    pub(super) fn text_value(&self, reference: ObjRef) -> Result<TextRef<'_>, FaultCode> {
        self.vm.heap.text(reference).ok_or(BAD_TYPE)
    }

    /// Read two integer operands and preserve successful input.
    ///
    /// An error consumes the same operands as two ordered `pop_int`
    /// calls. A successful caller replaces both operands in place.
    #[inline(always)]
    pub(super) fn int_pair(&mut self) -> Result<(usize, i64, i64), FaultCode> {
        let len = self.vm.operands.len();
        if len < 2 {
            return self.short_int_pair();
        }
        let b = match self.vm.operands[len - 1] {
            Value::Int(value) => value,
            _ => {
                self.vm.operands.truncate(len - 1);
                return Err(BAD_TYPE);
            }
        };
        let at = len - 2;
        let a = match self.vm.operands[at] {
            Value::Int(value) => value,
            _ => {
                self.vm.operands.truncate(at);
                return Err(BAD_TYPE);
            }
        };
        Ok((at, a, b))
    }

    #[cold]
    #[inline(never)]
    pub(super) fn short_int_pair(&mut self) -> Result<(usize, i64, i64), FaultCode> {
        match self.vm.operands.pop() {
            None | Some(Value::Int(_)) => Err(BAD_STATE),
            Some(_) => Err(BAD_TYPE),
        }
    }

    #[inline(always)]
    pub(super) fn replace_pair(&mut self, at: usize, value: Value) {
        self.vm.operands[at] = value;
        self.vm.operands.truncate(at + 1);
    }

    #[inline(always)]
    pub(super) fn int_binary(
        &mut self,
        op: impl Fn(i64, i64) -> Option<i64>,
    ) -> Result<(), FaultCode> {
        let (at, a, b) = self.int_pair()?;
        let Some(value) = op(a, b) else {
            self.vm.operands.truncate(at);
            return Err(FaultCode::IntegerOverflow);
        };
        self.replace_pair(at, Value::Int(value));
        Ok(())
    }

    #[inline(always)]
    pub(super) fn int_compare(&mut self, op: impl Fn(i64, i64) -> bool) -> Result<(), FaultCode> {
        let (at, a, b) = self.int_pair()?;
        self.replace_pair(at, Value::Bool(op(a, b)));
        Ok(())
    }

    #[inline(never)]
    pub(super) fn int_div(&mut self) -> Result<(), FaultCode> {
        let (at, left, right) = self.int_pair()?;
        if right == 0 {
            self.vm.operands.truncate(at);
            return Err(FaultCode::DivideByZero);
        }
        if left == i64::MIN && right == -1 {
            self.vm.operands.truncate(at);
            return Err(FaultCode::IntegerOverflow);
        }
        self.replace_pair(at, Value::Int(left / right));
        Ok(())
    }

    #[inline]
    pub(super) fn int_rem(&mut self) -> Result<(), FaultCode> {
        let (at, left, right) = self.int_pair()?;
        if right == 0 {
            self.vm.operands.truncate(at);
            return Err(FaultCode::DivideByZero);
        }
        if left == i64::MIN && right == -1 {
            self.vm.operands.truncate(at);
            return Err(FaultCode::IntegerOverflow);
        }
        self.replace_pair(at, Value::Int(left % right));
        Ok(())
    }

    pub(super) fn str_compare(&mut self, want_equal: bool) -> Result<(), FaultCode> {
        let b = self.pop_obj()?;
        let a = self.pop_obj()?;
        let equal = self.text_value(a)? == self.text_value(b)?;
        self.push(Value::Bool(equal == want_equal))
    }
}
