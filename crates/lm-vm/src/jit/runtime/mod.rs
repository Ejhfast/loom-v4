//! Typed native runtime slow paths.

use crate::machine::{
    float_text, integer_text_len, map_probe_parts, map_probe_token, parse_float_text, Machine,
};
use crate::NamespaceRuntime;
use lm_heap::{
    MapEntry, MapIndex, NativeByteBuffer, NativeStringBuilder, SharedBytes, SharedText,
    StructuralEpoch,
};
use lm_jit::{
    AllocationResult, CallbackAllocationRequest, CallbackAllocationResult,
    ClosureAllocationRequest, CollectionReserveRequest, CollectionReserveResult, DigestRequest,
    HeapOperationRequest, HeapOperationResult, ListGrowthRequest, ListGrowthResult,
    ListInsertRequest, MapInsertHashedRequest, MapInternTextRangeRequest, MapPutCommitRequest,
    MapPutDiscardRequest, MapPutProbeResult, NativeResolvedCallCache, NativeRootError, NativeRoots,
    NativeRuntime, NativeTypeEnvironmentCache, RuntimeUnitResult, RuntimeValueResult, ScalarKind,
    ValueArrayAllocationRequest,
};
use lm_value::{canonical_float_bits, CallbackRef, ObjRef, TypeEnvId, Value, ValueTag, Witness};
use std::fmt::Write;

pub(super) fn scalar_parts(kind: ScalarKind, value: Value) -> Option<(u64, u64)> {
    let expected = match kind {
        ScalarKind::Unit => Some(ValueTag::Unit),
        ScalarKind::Bool => Some(ValueTag::Bool),
        ScalarKind::Int => Some(ValueTag::Int),
        ScalarKind::Float => Some(ValueTag::Float),
        ScalarKind::Char => Some(ValueTag::Char),
        ScalarKind::Object(_) => Some(ValueTag::Obj),
        ScalarKind::Tagged(_) => None,
        ScalarKind::Callback(_) => None,
        ScalarKind::Operation => Some(ValueTag::Op),
    };
    if expected.is_some_and(|tag| value.tag() != tag) {
        return None;
    }
    if matches!(kind, ScalarKind::Callback(_))
        && !matches!(value, Value::Obj(_) | Value::Callback(_))
    {
        return None;
    }
    let bits = value_bits(value)?;
    if matches!(kind, ScalarKind::Float) && canonical_float_bits(bits) != bits {
        return None;
    }
    Some((value.tag() as u64, bits))
}

pub(super) fn parts_value(kind: ScalarKind, tag: u64, bits: u64) -> Option<Value> {
    let value = tagged_value(tag, bits)?;
    scalar_parts(kind, value).map(|_| value)
}

pub(super) fn value_bits(value: Value) -> Option<u64> {
    Some(match value {
        Value::Unit => 0,
        Value::Bool(value) => u64::from(value),
        Value::Int(value) => value as u64,
        Value::Float(bits) => bits,
        Value::Char(value) => u64::from(u32::from(value)),
        Value::Obj(reference) => object_bits(reference),
        Value::Op(operation) => u64::from(operation),
        Value::Callback(reference) => reference_bits(reference),
        Value::EmptyCase { ty, arm } => u64::from(ty) | (u64::from(arm) << 32),
        Value::Uninit => return None,
    })
}

pub(super) fn tagged_value(tag: u64, bits: u64) -> Option<Value> {
    Some(match tag {
        tag if tag == ValueTag::Unit as u64 && bits == 0 => Value::Unit,
        tag if tag == ValueTag::Bool as u64 && bits <= 1 => Value::Bool(bits != 0),
        tag if tag == ValueTag::Int as u64 => Value::Int(bits as i64),
        tag if tag == ValueTag::Float as u64 => Value::Float(bits),
        tag if tag == ValueTag::Char as u64 && bits <= u64::from(u32::MAX) => {
            Value::Char(char::from_u32(bits as u32)?)
        }
        tag if tag == ValueTag::Obj as u64 => Value::Obj(object_reference(bits)),
        tag if tag == ValueTag::Op as u64 && bits <= u64::from(u32::MAX) => Value::Op(bits as u32),
        tag if tag == ValueTag::Callback as u64 => Value::Callback(callback_reference(bits)),
        tag if tag == ValueTag::EmptyCase as u64 => Value::EmptyCase {
            ty: bits as u32,
            arm: (bits >> 32) as u32,
        },
        _ => return None,
    })
}

fn object_bits(reference: ObjRef) -> u64 {
    u64::from(reference.slot) | (u64::from(reference.generation) << 32)
}

fn object_reference(bits: u64) -> ObjRef {
    ObjRef {
        slot: bits as u32,
        generation: (bits >> 32) as u32,
    }
}

fn reference_bits(reference: CallbackRef) -> u64 {
    u64::from(reference.slot) | (u64::from(reference.generation) << 32)
}

fn callback_reference(bits: u64) -> CallbackRef {
    CallbackRef {
        slot: bits as u32,
        generation: (bits >> 32) as u32,
    }
}

pub(super) struct MachineRuntime<'a> {
    pub(super) machine: &'a mut Machine,
    pub(super) envs: &'a mut lm_bytecode::closed::TypeEnvs,
    pub(super) type_environments: NativeTypeEnvironmentCache,
    pub(super) resolved_calls: NativeResolvedCallCache,
    pub(super) module: &'a NamespaceRuntime,
    pub(super) base_local: usize,
    pub(super) base_operand: usize,
    pub(super) allocations: u64,
    pub(super) inline_allocations: u64,
    pub(super) pending_instance_allocations: u64,
    pub(super) pending_instance_releases: u64,
    pub(super) pending_instance_materializations: u64,
    pub(super) scalar_replaced_allocations: u64,
    pub(super) collection_slow_paths: u64,
}

impl Drop for MachineRuntime<'_> {
    fn drop(&mut self) {
        self.machine.native_type_environments = std::mem::take(&mut self.type_environments);
        self.machine.native_resolved_calls = std::mem::take(&mut self.resolved_calls);
    }
}

enum CaptureDecodeFailure {
    Limit,
    Invalid,
}

fn decode_captures(bits: &[u64], tags: &[u64]) -> Result<Vec<Value>, CaptureDecodeFailure> {
    if bits.len() != tags.len() {
        return Err(CaptureDecodeFailure::Invalid);
    }
    let mut values = Vec::new();
    values
        .try_reserve_exact(bits.len())
        .map_err(|_| CaptureDecodeFailure::Limit)?;
    for (&bits, &tag) in bits.iter().zip(tags) {
        values.push(tagged_value(tag, bits).ok_or(CaptureDecodeFailure::Invalid)?);
    }
    Ok(values)
}

fn runtime_value(value: Value) -> RuntimeValueResult {
    let Some(bits) = value_bits(value) else {
        return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
    };
    RuntimeValueResult::Value {
        bits,
        tag: value.tag() as u64,
    }
}

fn decode_root_objects(view: NativeRoots<'_>) -> Result<Vec<ObjRef>, CaptureDecodeFailure> {
    let mut roots = Vec::new();
    view.extend_objects(&mut roots)
        .map_err(|failure| match failure {
            NativeRootError::Invalid => CaptureDecodeFailure::Invalid,
            NativeRootError::Limit => CaptureDecodeFailure::Limit,
        })?;
    Ok(roots)
}

struct MapInsertRequest<'a> {
    reference: ObjRef,
    key: Value,
    value: Value,
    semantic_hash: i64,
    entry_count: usize,
    roots: NativeRoots<'a>,
    allow_collection: bool,
    key_storage: MapInsertKeyStorage,
}

/// Select how one runtime map insertion stores its key.
#[derive(Clone, Copy)]
enum MapInsertKeyStorage {
    /// Store the supplied declared key.
    Declared,
    /// Store one owned String for a borrowed text key.
    BorrowedString,
}

struct SyntaxTreeParts {
    source: Value,
    records: Value,
    text: SharedText,
    data: SharedBytes,
}

struct SyntaxElementParts {
    source: Value,
    records: Value,
    text: SharedText,
    data: SharedBytes,
    index: u32,
}

struct SyntaxElementRefs<'a> {
    source: Value,
    records: Value,
    text: &'a SharedText,
    data: &'a SharedBytes,
    index: u32,
}

mod alloc;
mod builders;
mod digest;
mod lists;
mod maps;
mod regex;
mod syntax;
mod text;

impl NativeRuntime for MachineRuntime<'_> {
    fn record_inline_allocations(&mut self, count: u64) {
        self.runtime_record_inline_allocations(count)
    }

    fn record_pending_instances(&mut self, allocations: u64, releases: u64) {
        self.runtime_record_pending_instances(allocations, releases)
    }

    fn record_scalar_replacements(&mut self, allocations: u64) {
        self.runtime_record_scalar_replacements(allocations)
    }

    fn allocate_instance(
        &mut self,
        class: u32,
        environment: u32,
        roots: NativeRoots<'_>,
        allow_collection: bool,
    ) -> AllocationResult {
        self.runtime_allocate_instance(class, environment, roots, allow_collection)
    }

    fn allocate_closure(&mut self, request: ClosureAllocationRequest<'_>) -> AllocationResult {
        self.runtime_allocate_closure(request)
    }

    fn allocate_callback(
        &mut self,
        request: CallbackAllocationRequest<'_>,
    ) -> CallbackAllocationResult {
        self.runtime_allocate_callback(request)
    }

    fn allocate_tuple(&mut self, request: ValueArrayAllocationRequest<'_>) -> AllocationResult {
        self.runtime_allocate_tuple(request)
    }

    fn allocate_list(&mut self, request: ValueArrayAllocationRequest<'_>) -> AllocationResult {
        self.runtime_allocate_list(request)
    }

    fn allocate_map(&mut self, request: ValueArrayAllocationRequest<'_>) -> AllocationResult {
        self.runtime_allocate_map(request)
    }

    fn map_has(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> RuntimeValueResult {
        self.runtime_map_has(reference, key_bits, key_tag)
    }

    fn list_contains(
        &mut self,
        reference: u64,
        value_bits: u64,
        value_tag: u64,
    ) -> RuntimeValueResult {
        self.runtime_list_contains(reference, value_bits, value_tag)
    }

    fn map_at(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> RuntimeValueResult {
        self.runtime_map_at(reference, key_bits, key_tag)
    }

    fn map_get(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> RuntimeValueResult {
        self.runtime_map_get(reference, key_bits, key_tag)
    }

    fn map_next_index(&mut self, reference: u64, cursor: u64, expected: u64) -> RuntimeValueResult {
        self.runtime_map_next_index(reference, cursor, expected)
    }

    fn map_key_at(&mut self, reference: u64, index: u64) -> RuntimeValueResult {
        self.runtime_map_key_at(reference, index)
    }

    fn map_value_at(&mut self, reference: u64, index: u64) -> RuntimeValueResult {
        self.runtime_map_value_at(reference, index)
    }

    fn map_remove(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> RuntimeValueResult {
        self.runtime_map_remove(reference, key_bits, key_tag)
    }

    fn map_clear(&mut self, reference: u64) -> RuntimeValueResult {
        self.runtime_map_clear(reference)
    }

    fn map_probe(&mut self, reference: u64, semantic: u64, prior: u64) -> RuntimeValueResult {
        self.runtime_map_probe(reference, semantic, prior)
    }

    fn map_probe_key(&mut self, reference: u64, token: u64) -> RuntimeValueResult {
        self.runtime_map_probe_key(reference, token)
    }

    fn map_probe_value(&mut self, reference: u64, token: u64) -> RuntimeValueResult {
        self.runtime_map_probe_value(reference, token)
    }

    fn map_probe_set_value(
        &mut self,
        reference: u64,
        token: u64,
        value_bits: u64,
        value_tag: u64,
    ) -> RuntimeValueResult {
        self.runtime_map_probe_set_value(reference, token, value_bits, value_tag)
    }

    fn map_probe_remove(&mut self, reference: u64, token: u64) -> RuntimeValueResult {
        self.runtime_map_probe_remove(reference, token)
    }

    fn map_insert_hashed(&mut self, request: MapInsertHashedRequest<'_>) -> RuntimeUnitResult {
        self.runtime_map_insert_hashed(request)
    }

    fn map_put_probe(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> MapPutProbeResult {
        self.runtime_map_put_probe(reference, key_bits, key_tag)
    }

    fn map_put_discard(&mut self, request: MapPutDiscardRequest<'_>) -> RuntimeUnitResult {
        self.runtime_map_put_discard(request)
    }

    fn map_put_commit(&mut self, request: MapPutCommitRequest<'_>) -> RuntimeUnitResult {
        self.runtime_map_put_commit(request)
    }

    fn map_intern_text_range(
        &mut self,
        request: MapInternTextRangeRequest<'_>,
    ) -> HeapOperationResult {
        self.runtime_map_intern_text_range(request)
    }

    fn grow_list(&mut self, request: ListGrowthRequest<'_>) -> ListGrowthResult {
        self.runtime_grow_list(request)
    }

    fn insert_list(&mut self, request: ListInsertRequest<'_>) -> ListGrowthResult {
        self.runtime_insert_list(request)
    }

    fn reserve_list(&mut self, request: CollectionReserveRequest<'_>) -> CollectionReserveResult {
        self.runtime_reserve_list(request)
    }

    fn reserve_map(&mut self, request: CollectionReserveRequest<'_>) -> CollectionReserveResult {
        self.runtime_reserve_map(request)
    }

    fn values_equal(
        &mut self,
        left_bits: u64,
        left_tag: u64,
        right_bits: u64,
        right_tag: u64,
    ) -> RuntimeValueResult {
        self.runtime_values_equal(left_bits, left_tag, right_bits, right_tag)
    }

    fn compare_text(&mut self, left: u64, right: u64) -> RuntimeValueResult {
        self.runtime_compare_text(left, right)
    }

    fn compare_bytes(&mut self, left: u64, right: u64) -> RuntimeValueResult {
        self.runtime_compare_bytes(left, right)
    }

    fn hash_text(&mut self, reference: u64) -> RuntimeValueResult {
        self.runtime_hash_text(reference)
    }

    fn hash_bytes(&mut self, reference: u64) -> RuntimeValueResult {
        self.runtime_hash_bytes(reference)
    }

    fn freeze_graph(&mut self, reference: u64) -> RuntimeValueResult {
        self.runtime_freeze_graph(reference)
    }

    fn digest_value(&mut self, request: DigestRequest<'_>) -> AllocationResult {
        self.runtime_digest_value(request)
    }

    fn fault_code(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_fault_code(request)
    }

    fn fault_denied(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_fault_denied(request)
    }

    fn dyn_pack(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_dyn_pack(request)
    }

    fn syntax_tree_root(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_syntax_tree_root(request)
    }

    fn syntax_kind(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_syntax_kind(request)
    }

    fn syntax_category(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_syntax_category(request)
    }

    fn syntax_range_start(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_syntax_range_start(request)
    }

    fn syntax_range_end(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_syntax_range_end(request)
    }

    fn syntax_text(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_syntax_text(request)
    }

    fn syntax_children(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_syntax_children(request)
    }

    fn syntax_detach(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_syntax_detach(request)
    }

    fn syntax_build_token(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_syntax_build_token(request)
    }

    fn syntax_build_trivia(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_syntax_build_trivia(request)
    }

    fn syntax_build_node(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_syntax_build_node(request)
    }

    fn syntax_to_tree(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_syntax_to_tree(request)
    }

    fn string_builder_new(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_string_builder_new(request)
    }

    fn string_builder_append_text(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.runtime_string_builder_append_text(request)
    }

    fn string_builder_append_int(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.runtime_string_builder_append_int(request)
    }

    fn string_builder_append_bool(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.runtime_string_builder_append_bool(request)
    }

    fn string_builder_append_char(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.runtime_string_builder_append_char(request)
    }

    fn string_builder_append_float(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.runtime_string_builder_append_float(request)
    }

    fn string_builder_build(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_string_builder_build(request)
    }

    fn string_builder_finish(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_string_builder_finish(request)
    }

    fn byte_buffer_new(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_byte_buffer_new(request)
    }

    fn byte_buffer_append(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_byte_buffer_append(request)
    }

    fn byte_buffer_build(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_byte_buffer_build(request)
    }

    fn byte_buffer_extend(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_byte_buffer_extend(request)
    }

    fn byte_buffer_reserve(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_byte_buffer_reserve(request)
    }

    fn byte_buffer_finish(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_byte_buffer_finish(request)
    }

    fn bytes_from_text(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_from_text(request)
    }

    fn bytes_slice(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_slice(request)
    }

    fn bytes_concat(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_concat(request)
    }

    fn bytes_compact(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_compact(request)
    }

    fn bytes_text_view(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_text_view(request)
    }

    fn bytes_bit_and(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_bit_and(request)
    }

    fn bytes_bit_or(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_bit_or(request)
    }

    fn bytes_bit_xor(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_bit_xor(request)
    }

    fn bytes_bit_not(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_bit_not(request)
    }

    fn text_concat(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_concat(request)
    }

    fn text_starts_with(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_starts_with(request)
    }

    fn text_ends_with(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_ends_with(request)
    }

    fn text_contains(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_contains(request)
    }

    fn text_find_scalar(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_find_scalar(request)
    }

    fn text_find_byte(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_find_byte(request)
    }

    fn text_trim(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_trim(request)
    }

    fn text_trim_start(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_trim_start(request)
    }

    fn text_trim_end(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_trim_end(request)
    }

    fn text_lower_ascii(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_lower_ascii(request)
    }

    fn text_upper_ascii(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_upper_ascii(request)
    }

    fn text_replace(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_replace(request)
    }

    fn text_parse_int_status(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_parse_int_status(request)
    }

    fn text_parse_int_value(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_parse_int_value(request)
    }

    fn text_pad_start(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_pad_start(request)
    }

    fn text_pad_end(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_pad_end(request)
    }

    fn bytes_ends_with(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_ends_with(request)
    }

    fn bytes_contains(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_contains(request)
    }

    fn text_split(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_split(request)
    }

    fn text_lines(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_lines(request)
    }

    fn text_slice(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_slice(request)
    }

    fn text_slice_bytes(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_slice_bytes(request)
    }

    fn text_bytes(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_bytes(request)
    }

    fn text_to_string(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_to_string(request)
    }

    fn bytes_text(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_text(request)
    }

    fn bytes_text_range(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_text_range(request)
    }

    fn byte_buffer_find_from(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_byte_buffer_find_from(request)
    }

    fn bytes_starts_with(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_starts_with(request)
    }

    fn bytes_find_index(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_find_index(request)
    }

    fn bytes_hex(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_hex(request)
    }

    fn bytes_is_utf8(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_bytes_is_utf8(request)
    }

    fn digest_sha256(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.digest_sha256_operation(request)
    }

    fn digest_crc32(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.digest_crc32_operation(request)
    }

    fn digest_md5(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.digest_md5_operation(request)
    }

    fn text_parse_float_status(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.runtime_text_parse_float_status(request)
    }

    fn text_parse_float_value(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_text_parse_float_value(request)
    }

    fn float_fixed(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_float_fixed(request)
    }

    fn regex_compile_status(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_regex_compile_status(request)
    }

    fn regex_compile_value(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_regex_compile_value(request)
    }

    fn regex_source(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_regex_source(request)
    }

    fn regex_is_match(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_regex_is_match(request)
    }

    fn regex_captures(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_regex_captures(request)
    }

    fn regex_count(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_regex_count(request)
    }

    fn regex_split(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_regex_split(request)
    }

    fn regex_replace_all(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_regex_replace_all(request)
    }

    fn regex_match_start(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_regex_match_start(request)
    }

    fn regex_match_end(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_regex_match_end(request)
    }

    fn regex_match_text(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_regex_match_text(request)
    }

    fn regex_match_group_count(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.runtime_regex_match_group_count(request)
    }

    fn regex_match_group(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_regex_match_group(request)
    }

    fn regex_match_named(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.runtime_regex_match_named(request)
    }
}

fn heap_bits(bits: u64) -> HeapOperationResult {
    HeapOperationResult::Value {
        bits,
        heap: None,
        object: false,
    }
}

fn heap_bool(value: bool) -> HeapOperationResult {
    heap_bits(u64::from(value))
}

fn heap_int(value: i64) -> HeapOperationResult {
    heap_bits(value as u64)
}

fn runtime_int(value: i64) -> RuntimeValueResult {
    RuntimeValueResult::Value {
        bits: value as u64,
        tag: ValueTag::Int as u64,
    }
}

fn runtime_ordering(ordering: std::cmp::Ordering) -> RuntimeValueResult {
    runtime_int(match ordering {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}
