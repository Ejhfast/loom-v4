//! Compression runtime paths.

use super::*;
use crate::machine::{compression_format, PendingDecompression};

impl MachineRuntime<'_> {
    pub(super) fn runtime_compress_encode(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let bytes = match self.bytes_value(request.first) {
            Ok(bytes) => bytes,
            Err(result) => return result,
        };
        let format = match compression_format(request.second as i64) {
            Ok(format) => format,
            Err(fault) => return HeapOperationResult::Fault(fault),
        };
        let level = match u32::try_from(request.third as i64) {
            Ok(level) => level,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::MalformedState),
        };
        let output = match lm_compress::compress(bytes.as_slice(), format, level) {
            Ok(output) => output,
            Err(lm_compress::CompressError::Allocation) => {
                return HeapOperationResult::HeapLimit;
            }
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::MalformedState),
        };
        self.allocate_heap_object(crate::Object::Bytes(SharedBytes::from(output)), &request)
    }

    pub(super) fn runtime_compress_decode_status(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.machine.pending_decompression = None;
        let bytes = match self.bytes_value(request.first) {
            Ok(bytes) => bytes,
            Err(result) => return result,
        };
        let format_value = request.second as i64;
        let format = match compression_format(format_value) {
            Ok(format) => format,
            Err(fault) => return HeapOperationResult::Fault(fault),
        };
        let limit_value = request.third as i64;
        let limit = match usize::try_from(limit_value) {
            Ok(limit) => limit,
            Err(_) => return heap_int(2),
        };
        let output = match lm_compress::decompress(bytes.as_slice(), format, limit) {
            Ok(output) => output,
            Err(lm_compress::DecompressError::InvalidData) => return heap_int(1),
            Err(lm_compress::DecompressError::Limit) => return heap_int(2),
            Err(lm_compress::DecompressError::Allocation) => {
                return HeapOperationResult::HeapLimit;
            }
        };
        match self.allocate_object(
            crate::Object::Bytes(SharedBytes::from(output)),
            request.roots,
            request.allow_collection,
        ) {
            AllocationResult::Value { bits, heap } => {
                let output = object_reference(bits);
                self.machine.pending_decompression = Some(PendingDecompression {
                    input: object_reference(request.first),
                    format: format_value,
                    limit: limit_value,
                    output,
                });
                HeapOperationResult::Value {
                    bits: 0,
                    heap,
                    object: false,
                }
            }
            AllocationResult::CollectionRequired => HeapOperationResult::Interpreter,
            AllocationResult::HeapLimit => HeapOperationResult::HeapLimit,
            AllocationResult::Interpreter => HeapOperationResult::Interpreter,
        }
    }

    pub(super) fn runtime_compress_decode_value(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let input = object_reference(request.first);
        let format_value = request.second as i64;
        let limit_value = request.third as i64;
        if let Some(pending) = self.machine.pending_decompression.take() {
            if pending.input == input
                && pending.format == format_value
                && pending.limit == limit_value
                && self.machine.vm.heap.try_get(pending.output).is_some()
            {
                return MachineRuntime::heap_object_value(pending.output);
            }
        }
        let bytes = match self.bytes_value(request.first) {
            Ok(bytes) => bytes,
            Err(result) => return result,
        };
        let format = match compression_format(format_value) {
            Ok(format) => format,
            Err(fault) => return HeapOperationResult::Fault(fault),
        };
        let limit = match usize::try_from(limit_value) {
            Ok(limit) => limit,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::MalformedState),
        };
        let output = match lm_compress::decompress(bytes.as_slice(), format, limit) {
            Ok(output) => output,
            Err(lm_compress::DecompressError::Allocation) => {
                return HeapOperationResult::HeapLimit;
            }
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::MalformedState),
        };
        self.allocate_heap_object(crate::Object::Bytes(SharedBytes::from(output)), &request)
    }
}
