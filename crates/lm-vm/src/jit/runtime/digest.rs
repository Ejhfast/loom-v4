//! Content digest runtime paths.

use super::*;

impl MachineRuntime<'_> {
    pub(super) fn digest_sha256_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let bytes = match self.bytes_value(request.first) {
            Ok(bytes) => bytes,
            Err(result) => return result,
        };
        if let Err(result) = self.reserve_heap_growth(32, &request) {
            return result;
        }
        let digest = lm_digest::sha256(bytes.as_slice());
        self.allocate_heap_object(crate::Object::Bytes(SharedBytes::from(&digest)), &request)
    }

    pub(super) fn digest_crc32_operation(
        &self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let bytes = match self.bytes_value(request.first) {
            Ok(bytes) => bytes,
            Err(result) => return result,
        };
        heap_int(i64::from(lm_digest::crc32(bytes.as_slice())))
    }

    pub(super) fn digest_md5_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let bytes = match self.bytes_value(request.first) {
            Ok(bytes) => bytes,
            Err(result) => return result,
        };
        if let Err(result) = self.reserve_heap_growth(16, &request) {
            return result;
        }
        let digest = lm_digest::md5(bytes.as_slice());
        self.allocate_heap_object(crate::Object::Bytes(SharedBytes::from(&digest)), &request)
    }
}
