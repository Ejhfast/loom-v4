//! Regular-expression runtime paths.

use super::*;
use crate::machine::{build_regex_match, regex_group_text};

impl MachineRuntime<'_> {
    pub(super) fn runtime_regex_compile_status(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.machine.pending_regex_compile = None;
        let pattern = match self.text_value(request.first) {
            Ok(pattern) => pattern,
            Err(result) => return result,
        };
        match lm_regex::Regex::compile(pattern.as_str()) {
            Ok(regex) => match self.allocate_object(
                crate::Object::NativeRegex(std::sync::Arc::new(regex)),
                request.roots,
                request.allow_collection,
            ) {
                AllocationResult::Value { bits, heap } => {
                    self.machine.pending_regex_compile = Some(object_reference(bits));
                    HeapOperationResult::Value {
                        bits: 0,
                        heap,
                        object: false,
                    }
                }
                AllocationResult::CollectionRequired => HeapOperationResult::Interpreter,
                AllocationResult::HeapLimit => HeapOperationResult::HeapLimit,
                AllocationResult::Interpreter => HeapOperationResult::Interpreter,
            },
            Err(error) if error.kind() == lm_regex::CompileErrorKind::Limit => {
                self.machine.pending_regex_compile = None;
                heap_int(2)
            }
            Err(_) => {
                self.machine.pending_regex_compile = None;
                heap_int(1)
            }
        }
    }

    pub(super) fn runtime_regex_compile_value(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let pattern = match self.text_value(request.first) {
            Ok(pattern) => pattern,
            Err(result) => return result,
        };
        let cached = self.machine.pending_regex_compile.take();
        if let Some(reference) = cached {
            if matches!(
                self.machine.vm.heap.try_get(reference),
                Some(crate::Object::NativeRegex(regex)) if regex.source() == pattern.as_str()
            ) {
                return HeapOperationResult::Value {
                    bits: object_bits(reference),
                    heap: None,
                    object: false,
                };
            }
        }
        let regex = match lm_regex::Regex::compile(pattern.as_str()) {
            Ok(regex) => regex,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::MalformedState),
        };
        self.allocate_heap_object(
            crate::Object::NativeRegex(std::sync::Arc::new(regex)),
            &request,
        )
    }

    pub(super) fn runtime_regex_source(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let regex = match self.regex_value(request.first) {
            Ok(regex) => regex,
            Err(result) => return result,
        };
        let text = match SharedText::try_from_str(regex.source()) {
            Ok(text) => text,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(text), &request)
    }

    pub(super) fn runtime_regex_is_match(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let regex = match self.regex_value(request.first) {
            Ok(regex) => regex,
            Err(result) => return result,
        };
        let text = match self.text_value(request.second) {
            Ok(text) => text,
            Err(result) => return result,
        };
        heap_bool(regex.is_match(text.as_str()))
    }

    pub(super) fn runtime_regex_captures(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let regex = match self.regex_value(request.first) {
            Ok(regex) => regex,
            Err(result) => return result,
        };
        let text = match self.text_value(request.second) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let matched = match build_regex_match(regex, text.as_str()) {
            Ok(Some(matched)) => matched,
            Ok(None) => return heap_bits(lm_jit::REGEX_OPTION_NONE),
            Err(crate::FaultCode::HeapLimit) => return HeapOperationResult::HeapLimit,
            Err(fault) => return HeapOperationResult::Fault(fault),
        };
        self.allocate_heap_object(
            crate::Object::NativeRegexMatch(std::sync::Arc::new(matched)),
            &request,
        )
    }

    pub(super) fn runtime_regex_count(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let regex = match self.regex_value(request.first) {
            Ok(regex) => regex,
            Err(result) => return result,
        };
        let text = match self.text_value(request.second) {
            Ok(text) => text,
            Err(result) => return result,
        };
        match i64::try_from(regex.count(text.as_str())) {
            Ok(count) => heap_int(count),
            Err(_) => HeapOperationResult::Fault(crate::FaultCode::IntegerOverflow),
        }
    }

    pub(super) fn runtime_regex_split(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let source = object_reference(request.second);
        let regex = match self.regex_value(request.first) {
            Ok(regex) => regex,
            Err(result) => return result,
        };
        let text = match self.text_value(request.second) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let ranges = regex
            .split_range_iter(text.as_str())
            .map(|range| (range.start, range.end));
        let pieces = match self
            .machine
            .vm
            .heap
            .try_text_range_view_batch(source, ranges)
        {
            Some(Ok(Some(pieces))) => pieces,
            Some(Ok(None)) | Some(Err(_)) => return HeapOperationResult::HeapLimit,
            None => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
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
            object: true,
        }
    }

    pub(super) fn runtime_regex_replace_all(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let regex = match self.regex_value(request.first) {
            Ok(regex) => regex,
            Err(result) => return result,
        };
        let text = match self.text_value(request.second) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let replacement = match self.text_value(request.third) {
            Ok(replacement) => replacement,
            Err(result) => return result,
        };
        let limit = self.machine.vm.heap.stats().cap_bytes;
        let output = match regex.replace_all(text.as_str(), replacement.as_str(), limit) {
            Ok(output) => output,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        let output = match SharedText::try_from_string(output) {
            Ok(output) => output,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(output), &request)
    }

    pub(super) fn runtime_regex_match_start(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        match self.regex_match_value(request.first) {
            Ok(matched) => heap_int(i64::from(matched.start)),
            Err(result) => result,
        }
    }

    pub(super) fn runtime_regex_match_end(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        match self.regex_match_value(request.first) {
            Ok(matched) => heap_int(i64::from(matched.end)),
            Err(result) => result,
        }
    }

    pub(super) fn runtime_regex_match_text(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let matched = match self.regex_match_value(request.first) {
            Ok(matched) => matched,
            Err(result) => return result,
        };
        let text = matched.text.clone();
        self.allocate_heap_object(crate::Object::Str(text), &request)
    }

    pub(super) fn runtime_regex_match_group_count(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let matched = match self.regex_match_value(request.first) {
            Ok(matched) => matched,
            Err(result) => return result,
        };
        match i64::try_from(matched.groups.len()) {
            Ok(count) => heap_int(count),
            Err(_) => HeapOperationResult::Fault(crate::FaultCode::IntegerOverflow),
        }
    }

    pub(super) fn runtime_regex_match_group(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let Ok(index) = usize::try_from(request.second as i64) else {
            return heap_bits(lm_jit::REGEX_OPTION_NONE);
        };
        let text = {
            let matched = match self.regex_match_value(request.first) {
                Ok(matched) => matched,
                Err(result) => return result,
            };
            match matched.groups.get(index).copied().flatten() {
                Some(range) => match regex_group_text(matched, range) {
                    Some(text) => Some(text),
                    None => return HeapOperationResult::Fault(crate::FaultCode::MalformedState),
                },
                None => None,
            }
        };
        self.regex_group_result(text, &request)
    }

    pub(super) fn runtime_regex_match_named(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let name = match self.text_value(request.second) {
            Ok(name) => name,
            Err(result) => return result,
        };
        let text = {
            let matched = match self.regex_match_value(request.first) {
                Ok(matched) => matched,
                Err(result) => return result,
            };
            let range = matched
                .names
                .iter()
                .find_map(|(candidate, index)| {
                    (candidate == name.as_str()).then_some(*index as usize)
                })
                .and_then(|index| matched.groups.get(index).copied().flatten());
            match range {
                Some(range) => match regex_group_text(matched, range) {
                    Some(text) => Some(text),
                    None => return HeapOperationResult::Fault(crate::FaultCode::MalformedState),
                },
                None => None,
            }
        };
        self.regex_group_result(text, &request)
    }

    fn regex_value(&self, bits: u64) -> Result<&lm_regex::Regex, HeapOperationResult> {
        match self.machine.vm.heap.try_get(object_reference(bits)) {
            Some(crate::Object::NativeRegex(regex)) => Ok(regex),
            _ => Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        }
    }

    fn regex_match_value(
        &self,
        bits: u64,
    ) -> Result<&lm_heap::NativeRegexMatch, HeapOperationResult> {
        match self.machine.vm.heap.try_get(object_reference(bits)) {
            Some(crate::Object::NativeRegexMatch(matched)) => Ok(matched),
            _ => Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        }
    }

    fn regex_group_result(
        &mut self,
        text: Option<SharedText>,
        request: &HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let Some(text) = text else {
            return heap_bits(lm_jit::REGEX_OPTION_NONE);
        };
        self.allocate_heap_object(crate::Object::Substring(text), request)
    }
}
