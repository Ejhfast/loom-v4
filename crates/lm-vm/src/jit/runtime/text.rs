//! Text, byte, and numeric conversion runtime paths.

use super::*;

impl MachineRuntime<'_> {
    pub(super) fn bytes_binary(
        &mut self,
        request: HeapOperationRequest<'_>,
        operation: fn(u8, u8) -> u8,
    ) -> HeapOperationResult {
        let left = object_reference(request.first);
        let right = object_reference(request.second);
        let (left, right) = match (
            self.machine.vm.heap.try_get(left),
            self.machine.vm.heap.try_get(right),
        ) {
            (Some(crate::Object::Bytes(left)), Some(crate::Object::Bytes(right))) => {
                (left.clone(), right.clone())
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if left.len() != right.len() {
            return HeapOperationResult::Fault(crate::FaultCode::LengthMismatch);
        }
        if let Err(result) = self.reserve_heap_growth(left.len(), &request) {
            return result;
        }
        let mut output = Vec::new();
        if output.try_reserve_exact(left.len()).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        output.extend(
            left.as_slice()
                .iter()
                .copied()
                .zip(right.as_slice().iter().copied())
                .map(|(left, right)| operation(left, right)),
        );
        self.allocate_heap_object(crate::Object::Bytes(SharedBytes::from(output)), &request)
    }

    pub(super) fn text_pair(
        &self,
        first: u64,
        second: u64,
    ) -> Result<(SharedText, SharedText), HeapOperationResult> {
        let first = object_reference(first);
        let second = object_reference(second);
        match (
            self.machine.vm.heap.try_get(first),
            self.machine.vm.heap.try_get(second),
        ) {
            (
                Some(crate::Object::Str(first) | crate::Object::Substring(first)),
                Some(crate::Object::Str(second) | crate::Object::Substring(second)),
            ) => Ok((first.clone(), second.clone())),
            _ => Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        }
    }

    pub(super) fn bytes_pair(
        &self,
        first: u64,
        second: u64,
    ) -> Result<(SharedBytes, SharedBytes), HeapOperationResult> {
        let first = object_reference(first);
        let second = object_reference(second);
        match (
            self.machine.vm.heap.try_get(first),
            self.machine.vm.heap.try_get(second),
        ) {
            (Some(crate::Object::Bytes(first)), Some(crate::Object::Bytes(second))) => {
                Ok((first.clone(), second.clone()))
            }
            _ => Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        }
    }

    pub(super) fn text_value(&self, reference: u64) -> Result<SharedText, HeapOperationResult> {
        let reference = object_reference(reference);
        match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Str(text) | crate::Object::Substring(text)) => Ok(text.clone()),
            _ => Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        }
    }

    pub(super) fn bytes_value(&self, reference: u64) -> Result<SharedBytes, HeapOperationResult> {
        let reference = object_reference(reference);
        match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Bytes(bytes)) => Ok(bytes.clone()),
            _ => Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        }
    }

    pub(super) fn text_concat_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let (left, right) = match self.text_pair(request.first, request.second) {
            Ok(pair) => pair,
            Err(result) => return result,
        };
        let Some(length) = left.len().checked_add(right.len()) else {
            return HeapOperationResult::HeapLimit;
        };
        if let Err(result) = self.reserve_heap_growth(length, &request) {
            return result;
        }
        let text = match left.try_concat(&right) {
            Ok(text) => text,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(text), &request)
    }

    pub(super) fn text_predicate_operation(
        &self,
        request: HeapOperationRequest<'_>,
        predicate: fn(&str, &str) -> bool,
    ) -> HeapOperationResult {
        let (text, argument) = match self.text_pair(request.first, request.second) {
            Ok(pair) => pair,
            Err(result) => return result,
        };
        heap_bool(predicate(text.as_str(), argument.as_str()))
    }

    pub(super) fn text_find_operation(
        &self,
        request: HeapOperationRequest<'_>,
        scalar: bool,
    ) -> HeapOperationResult {
        let (text, needle) = match self.text_pair(request.first, request.second) {
            Ok(pair) => pair,
            Err(result) => return result,
        };
        let found = if scalar {
            text.find_scalar(&needle)
        } else {
            text.find_byte(&needle)
        };
        let value = match found {
            Some(index) => match i64::try_from(index) {
                Ok(index) => index,
                Err(_) => {
                    return HeapOperationResult::Fault(crate::FaultCode::IntegerOverflow);
                }
            },
            None => -1,
        };
        heap_int(value)
    }

    pub(super) fn text_trim_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
        trim_start: bool,
        trim_end: bool,
    ) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let source = text.as_str();
        let start = if trim_start {
            source.len() - source.trim_start().len()
        } else {
            0
        };
        let end = if trim_end {
            source.trim_end().len()
        } else {
            source.len()
        }
        .max(start);
        let Some(slice) = text.slice(start, end) else {
            return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds);
        };
        self.allocate_heap_object(crate::Object::Substring(slice), &request)
    }

    pub(super) fn text_ascii_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
        lower: bool,
    ) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let length = text.len();
        if let Err(result) = self.reserve_heap_growth(length, &request) {
            return result;
        }
        let mut output = String::new();
        if output.try_reserve_exact(length).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        output.extend(text.as_str().chars().map(|value| {
            if lower {
                value.to_ascii_lowercase()
            } else {
                value.to_ascii_uppercase()
            }
        }));
        let output = match SharedText::try_from_string(output) {
            Ok(output) => output,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(output), &request)
    }

    pub(super) fn text_replace_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let needle = match self.text_value(request.second) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let replacement = match self.text_value(request.third) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let source = text.as_str();
        let needle_text = needle.as_str();
        let replacement_text = replacement.as_str();
        let matches = source.match_indices(needle_text).count();
        let Some(removed) = matches.checked_mul(needle_text.len()) else {
            return HeapOperationResult::HeapLimit;
        };
        let Some(added) = matches.checked_mul(replacement_text.len()) else {
            return HeapOperationResult::HeapLimit;
        };
        let Some(length) = source
            .len()
            .checked_sub(removed)
            .and_then(|kept| kept.checked_add(added))
        else {
            return HeapOperationResult::HeapLimit;
        };
        if let Err(result) = self.reserve_heap_growth(length, &request) {
            return result;
        }
        let mut output = String::new();
        if output.try_reserve_exact(length).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        let mut cursor = 0;
        for (at, matched) in source.match_indices(needle_text) {
            output.push_str(&source[cursor..at]);
            output.push_str(replacement_text);
            cursor = at + matched.len();
        }
        output.push_str(&source[cursor..]);
        let output = match SharedText::try_from_string(output) {
            Ok(output) => output,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(output), &request)
    }

    pub(super) fn text_parse_int_operation(
        &self,
        request: HeapOperationRequest<'_>,
        status: bool,
    ) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let radix = u32::try_from(request.second as i64)
            .ok()
            .filter(|radix| (2..=36).contains(radix));
        let Some(radix) = radix else {
            return heap_int(if status { 3 } else { 0 });
        };
        let parsed = i64::from_str_radix(text.as_str(), radix);
        let answer = match (status, parsed) {
            (true, Ok(_)) => 0,
            (true, Err(error)) => match error.kind() {
                std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => 2,
                _ => 1,
            },
            (false, Ok(value)) => value,
            (false, Err(_)) => 0,
        };
        heap_int(answer)
    }

    pub(super) fn text_pad_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
        before: bool,
    ) -> HeapOperationResult {
        let reference = object_reference(request.first);
        let text = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Str(text) | crate::Object::Substring(text)) => text.clone(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        let scalar_length = match i64::try_from(text.char_count()) {
            Ok(length) => length,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::IntegerOverflow),
        };
        let padding = (request.second as i64).saturating_sub(scalar_length);
        if padding <= 0
            && matches!(
                self.machine.vm.heap.try_get(reference),
                Some(crate::Object::Str(_))
            )
        {
            return heap_bits(request.first);
        }
        let padding = match usize::try_from(padding.max(0)) {
            Ok(padding) => padding,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        let Some(length) = text.len().checked_add(padding) else {
            return HeapOperationResult::HeapLimit;
        };
        if let Err(result) = self.reserve_heap_growth(length, &request) {
            return result;
        }
        let mut output = String::new();
        if output.try_reserve_exact(length).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        if before {
            output.extend(std::iter::repeat_n(' ', padding));
        }
        output.push_str(text.as_str());
        if !before {
            output.extend(std::iter::repeat_n(' ', padding));
        }
        let output = match SharedText::try_from_string(output) {
            Ok(output) => output,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(output), &request)
    }

    pub(super) fn bytes_predicate_operation(
        &self,
        request: HeapOperationRequest<'_>,
        predicate: fn(&[u8], &[u8]) -> bool,
    ) -> HeapOperationResult {
        let (bytes, argument) = match self.bytes_pair(request.first, request.second) {
            Ok(pair) => pair,
            Err(result) => return result,
        };
        heap_bool(predicate(bytes.as_slice(), argument.as_slice()))
    }

    pub(super) fn bytes_contains_operation(
        &self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let (bytes, needle) = match self.bytes_pair(request.first, request.second) {
            Ok(pair) => pair,
            Err(result) => return result,
        };
        let needle = needle.as_slice();
        heap_bool(
            needle.is_empty()
                || bytes
                    .as_slice()
                    .windows(needle.len())
                    .any(|window| window == needle),
        )
    }

    pub(super) fn text_split_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
        split: bool,
    ) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let separator = if split {
            match self.text_value(request.second) {
                Ok(separator) => Some(separator),
                Err(result) => return result,
            }
        } else {
            None
        };
        let pieces = match separator.as_ref() {
            Some(separator) => text.try_split_views(separator.as_str()),
            None => text.try_line_views(),
        };
        let pieces = match pieces {
            Ok(pieces) => pieces,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        let Some(cost) = lm_heap::Heap::text_view_list_base_cost(pieces.len()) else {
            return HeapOperationResult::HeapLimit;
        };
        if let Err(result) = self.reserve_heap_growth(cost, &request) {
            return result;
        }
        let piece_count = pieces.len();
        let Some(reference) = self.machine.vm.heap.try_alloc_text_view_list(pieces) else {
            return HeapOperationResult::HeapLimit;
        };
        self.allocations = self
            .allocations
            .saturating_add(u64::try_from(piece_count).unwrap_or(u64::MAX))
            .saturating_add(1);
        HeapOperationResult::Value {
            bits: object_bits(reference),
            heap: Some(self.machine.vm.heap.jit_view()),
        }
    }

    pub(super) fn text_slice_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
        scalar: bool,
    ) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let start = match usize::try_from(request.second as i64) {
            Ok(start) => start,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds),
        };
        let length = match usize::try_from(request.third as i64) {
            Ok(length) => length,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds),
        };
        let slice = if scalar {
            text.scalar_slice(start, length)
        } else {
            start
                .checked_add(length)
                .and_then(|end| text.slice(start, end))
        };
        let Some(slice) = slice else {
            return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds);
        };
        self.allocate_heap_object(crate::Object::Substring(slice), &request)
    }

    pub(super) fn text_bytes_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        self.allocate_heap_object(crate::Object::Bytes(text.bytes()), &request)
    }

    pub(super) fn text_to_string_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let reference = object_reference(request.first);
        let text = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Str(_)) => return heap_bits(request.first),
            Some(crate::Object::Substring(text)) => text.clone(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if !text.has_bounded_retention() {
            if let Err(result) = self.reserve_heap_growth(text.len(), &request) {
                return result;
            }
        }
        let text = match text.try_bounded() {
            Ok(text) => text,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(text), &request)
    }

    pub(super) fn bytes_text_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let bytes = match self.bytes_value(request.first) {
            Ok(bytes) => bytes,
            Err(result) => return result,
        };
        let Some(text) = bytes.utf8_view() else {
            return HeapOperationResult::Fault(crate::FaultCode::BadCast);
        };
        if !text.has_bounded_retention() {
            if let Err(result) = self.reserve_heap_growth(text.len(), &request) {
                return result;
            }
        }
        let text = match text.try_bounded() {
            Ok(text) => text,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(text), &request)
    }

    pub(super) fn bytes_find_operation(
        &self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let (bytes, needle) = match self.bytes_pair(request.first, request.second) {
            Ok(pair) => pair,
            Err(result) => return result,
        };
        let needle = needle.as_slice();
        let found = if needle.is_empty() {
            Some(0)
        } else {
            bytes
                .as_slice()
                .windows(needle.len())
                .position(|window| window == needle)
        };
        let value = match found {
            Some(index) => match i64::try_from(index) {
                Ok(index) => index,
                Err(_) => {
                    return HeapOperationResult::Fault(crate::FaultCode::IntegerOverflow);
                }
            },
            None => -1,
        };
        heap_int(value)
    }

    pub(super) fn bytes_hex_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let bytes = match self.bytes_value(request.first) {
            Ok(bytes) => bytes,
            Err(result) => return result,
        };
        let Some(length) = bytes.len().checked_mul(2) else {
            return HeapOperationResult::HeapLimit;
        };
        if let Err(result) = self.reserve_heap_growth(length, &request) {
            return result;
        }
        let mut output = String::new();
        if output.try_reserve_exact(length).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in bytes.as_slice() {
            output.push(char::from(HEX[(byte >> 4) as usize]));
            output.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
        let output = match SharedText::try_from_string(output) {
            Ok(output) => output,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(output), &request)
    }

    pub(super) fn bytes_is_utf8_operation(
        &self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let bytes = match self.bytes_value(request.first) {
            Ok(bytes) => bytes,
            Err(result) => return result,
        };
        heap_bool(bytes.is_utf8())
    }

    pub(super) fn text_parse_float_operation(
        &self,
        request: HeapOperationRequest<'_>,
        status: bool,
    ) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let parsed = parse_float_text(text.as_str());
        if status {
            heap_int(parsed.err().unwrap_or(0))
        } else {
            heap_bits(canonical_float_bits(parsed.unwrap_or(0.0).to_bits()))
        }
    }

    pub(super) fn float_fixed_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let digits = request.second as i64;
        if digits < 0 {
            return HeapOperationResult::Fault(crate::FaultCode::InvalidPrecision);
        }
        let digits = match usize::try_from(digits) {
            Ok(digits) => digits,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        let value = f64::from_bits(request.first);
        let capacity = if value.is_finite() {
            match digits.checked_add(312) {
                Some(capacity) => capacity,
                None => return HeapOperationResult::HeapLimit,
            }
        } else {
            4
        };
        if let Err(result) = self.reserve_heap_growth(capacity, &request) {
            return result;
        }
        let mut output = String::new();
        if output.try_reserve_exact(capacity).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        if write!(&mut output, "{value:.digits$}").is_err() {
            return HeapOperationResult::HeapLimit;
        }
        let output = match SharedText::try_from_string(output) {
            Ok(output) => output,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(output), &request)
    }

    pub(super) fn runtime_bytes_from_text(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let source = object_reference(request.first);
        let text = match self.machine.vm.heap.try_get(source) {
            Some(crate::Object::Str(text)) => text.clone(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        self.allocate_heap_object(crate::Object::Bytes(text.bytes()), &request)
    }

    pub(super) fn runtime_bytes_slice(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let source = object_reference(request.first);
        let Ok(start) = usize::try_from(request.second as i64) else {
            return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds);
        };
        let Ok(length) = usize::try_from(request.third as i64) else {
            return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds);
        };
        let Some(end) = start.checked_add(length) else {
            return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds);
        };
        let bytes = match self.machine.vm.heap.try_get(source) {
            Some(crate::Object::Bytes(bytes)) => match bytes.slice(start, end) {
                Some(bytes) => bytes,
                None => return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds),
            },
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        self.allocate_heap_object(crate::Object::Bytes(bytes), &request)
    }

    pub(super) fn runtime_bytes_concat(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let left = object_reference(request.first);
        let right = object_reference(request.second);
        let (left, right) = match (
            self.machine.vm.heap.try_get(left),
            self.machine.vm.heap.try_get(right),
        ) {
            (Some(crate::Object::Bytes(left)), Some(crate::Object::Bytes(right))) => {
                (left.clone(), right.clone())
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        let Some(length) = left.len().checked_add(right.len()) else {
            return HeapOperationResult::HeapLimit;
        };
        if let Err(result) = self.reserve_heap_growth(length, &request) {
            return result;
        }
        let bytes = match left.try_concat(&right) {
            Ok(bytes) => bytes,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Bytes(bytes), &request)
    }

    pub(super) fn runtime_bytes_compact(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let source = object_reference(request.first);
        let bytes = match self.machine.vm.heap.try_get(source) {
            Some(crate::Object::Bytes(bytes)) => bytes.clone(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if let Err(result) = self.reserve_heap_growth(bytes.len(), &request) {
            return result;
        }
        let bytes = match bytes.try_compact() {
            Ok(bytes) => bytes,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Bytes(bytes), &request)
    }

    pub(super) fn runtime_bytes_text_view(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let source = object_reference(request.first);
        let text = match self.machine.vm.heap.try_get(source) {
            Some(crate::Object::Bytes(bytes)) => match bytes.utf8_view() {
                Some(text) => text,
                None => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
            },
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        self.allocate_heap_object(crate::Object::Substring(text), &request)
    }

    pub(super) fn runtime_bytes_bit_and(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.bytes_binary(request, |left, right| left & right)
    }

    pub(super) fn runtime_bytes_bit_or(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.bytes_binary(request, |left, right| left | right)
    }

    pub(super) fn runtime_bytes_bit_xor(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.bytes_binary(request, |left, right| left ^ right)
    }

    pub(super) fn runtime_bytes_bit_not(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let source = object_reference(request.first);
        let bytes = match self.machine.vm.heap.try_get(source) {
            Some(crate::Object::Bytes(bytes)) => bytes.clone(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if let Err(result) = self.reserve_heap_growth(bytes.len(), &request) {
            return result;
        }
        let mut output = Vec::new();
        if output.try_reserve_exact(bytes.len()).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        output.extend(bytes.as_slice().iter().map(|value| !value));
        self.allocate_heap_object(crate::Object::Bytes(SharedBytes::from(output)), &request)
    }

    pub(super) fn runtime_text_concat(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_concat_operation(request)
    }

    pub(super) fn runtime_text_starts_with(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_predicate_operation(request, |text, prefix| text.starts_with(prefix))
    }

    pub(super) fn runtime_text_ends_with(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_predicate_operation(request, |text, suffix| text.ends_with(suffix))
    }

    pub(super) fn runtime_text_contains(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_predicate_operation(request, |text, needle| text.contains(needle))
    }

    pub(super) fn runtime_text_find_scalar(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_find_operation(request, true)
    }

    pub(super) fn runtime_text_find_byte(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_find_operation(request, false)
    }

    pub(super) fn runtime_text_trim(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_trim_operation(request, true, true)
    }

    pub(super) fn runtime_text_trim_start(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_trim_operation(request, true, false)
    }

    pub(super) fn runtime_text_trim_end(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_trim_operation(request, false, true)
    }

    pub(super) fn runtime_text_lower_ascii(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_ascii_operation(request, true)
    }

    pub(super) fn runtime_text_upper_ascii(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_ascii_operation(request, false)
    }

    pub(super) fn runtime_text_replace(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_replace_operation(request)
    }

    pub(super) fn runtime_text_parse_int_status(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_parse_int_operation(request, true)
    }

    pub(super) fn runtime_text_parse_int_value(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_parse_int_operation(request, false)
    }

    pub(super) fn runtime_text_pad_start(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_pad_operation(request, true)
    }

    pub(super) fn runtime_text_pad_end(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_pad_operation(request, false)
    }

    pub(super) fn runtime_bytes_ends_with(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.bytes_predicate_operation(request, <[u8]>::ends_with)
    }

    pub(super) fn runtime_bytes_contains(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.bytes_contains_operation(request)
    }

    pub(super) fn runtime_text_split(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_split_operation(request, true)
    }

    pub(super) fn runtime_text_lines(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_split_operation(request, false)
    }

    pub(super) fn runtime_text_slice(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_slice_operation(request, true)
    }

    pub(super) fn runtime_text_slice_bytes(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_slice_operation(request, false)
    }

    pub(super) fn runtime_text_bytes(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_bytes_operation(request)
    }

    pub(super) fn runtime_text_to_string(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_to_string_operation(request)
    }

    pub(super) fn runtime_bytes_text(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.bytes_text_operation(request)
    }

    pub(super) fn runtime_bytes_starts_with(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.bytes_predicate_operation(request, <[u8]>::starts_with)
    }

    pub(super) fn runtime_bytes_find_index(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.bytes_find_operation(request)
    }

    pub(super) fn runtime_bytes_hex(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.bytes_hex_operation(request)
    }

    pub(super) fn runtime_bytes_is_utf8(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.bytes_is_utf8_operation(request)
    }

    pub(super) fn runtime_text_parse_float_status(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_parse_float_operation(request, true)
    }

    pub(super) fn runtime_text_parse_float_value(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_parse_float_operation(request, false)
    }

    pub(super) fn runtime_float_fixed(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.float_fixed_operation(request)
    }
}
