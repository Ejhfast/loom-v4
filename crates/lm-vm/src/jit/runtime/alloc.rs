//! Allocation and heap-growth runtime paths.

use super::*;

impl MachineRuntime<'_> {
    pub(super) fn allocate_object(
        &mut self,
        object: crate::Object,
        roots: NativeRoots<'_>,
        allow_collection: bool,
    ) -> AllocationResult {
        let cost = self.machine.vm.heap.allocation_cost(&object);
        if !self.machine.vm.heap.collection_due(cost) {
            let reference = self.machine.vm.heap.alloc(object);
            self.allocations = self.allocations.saturating_add(1);
            let heap = if reference.slot & lm_heap::JIT_PAGE_MASK == 0 {
                Some(self.machine.vm.heap.jit_view())
            } else {
                None
            };
            return AllocationResult::Value {
                bits: object_bits(reference),
                heap,
            };
        }
        if !allow_collection {
            return AllocationResult::CollectionRequired;
        }
        self.collection_slow_paths = self.collection_slow_paths.saturating_add(1);
        let roots = match decode_root_objects(roots) {
            Ok(roots) => roots,
            Err(CaptureDecodeFailure::Limit) => return AllocationResult::HeapLimit,
            Err(CaptureDecodeFailure::Invalid) => return AllocationResult::Interpreter,
        };
        match self
            .machine
            .alloc_native(object, self.base_local, self.base_operand, &roots)
        {
            Ok(Value::Obj(reference)) => {
                self.allocations = self.allocations.saturating_add(1);
                AllocationResult::Value {
                    bits: object_bits(reference),
                    heap: Some(self.machine.vm.heap.jit_view()),
                }
            }
            Ok(_) => AllocationResult::Interpreter,
            Err(crate::FaultCode::HeapLimit) => AllocationResult::HeapLimit,
            Err(_) => AllocationResult::Interpreter,
        }
    }

    pub(super) fn allocate_heap_object(
        &mut self,
        object: crate::Object,
        request: &HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        match self.allocate_object(object, request.roots, request.allow_collection) {
            AllocationResult::Value { bits, heap } => HeapOperationResult::Value { bits, heap },
            AllocationResult::CollectionRequired => HeapOperationResult::Interpreter,
            AllocationResult::HeapLimit => HeapOperationResult::HeapLimit,
            AllocationResult::Interpreter => HeapOperationResult::Interpreter,
        }
    }

    pub(super) fn allocate_heap_reference(
        &mut self,
        object: crate::Object,
        request: &HeapOperationRequest<'_>,
        extra_roots: &[ObjRef],
    ) -> Result<ObjRef, HeapOperationResult> {
        let cost = self.machine.vm.heap.allocation_cost(&object);
        if !self.machine.vm.heap.collection_due(cost) {
            let reference = self.machine.vm.heap.alloc(object);
            self.allocations = self.allocations.saturating_add(1);
            return Ok(reference);
        }
        if !request.allow_collection {
            return Err(HeapOperationResult::Interpreter);
        }
        let mut roots = match decode_root_objects(request.roots) {
            Ok(roots) => roots,
            Err(CaptureDecodeFailure::Limit) => return Err(HeapOperationResult::HeapLimit),
            Err(CaptureDecodeFailure::Invalid) => {
                return Err(HeapOperationResult::Interpreter);
            }
        };
        if roots.try_reserve_exact(extra_roots.len()).is_err() {
            return Err(HeapOperationResult::HeapLimit);
        }
        roots.extend_from_slice(extra_roots);
        match self
            .machine
            .alloc_native(object, self.base_local, self.base_operand, &roots)
        {
            Ok(Value::Obj(reference)) => {
                self.allocations = self.allocations.saturating_add(1);
                Ok(reference)
            }
            Ok(_) => Err(HeapOperationResult::Interpreter),
            Err(crate::FaultCode::HeapLimit) => Err(HeapOperationResult::HeapLimit),
            Err(fault) => Err(HeapOperationResult::Fault(fault)),
        }
    }

    pub(super) fn reserve_heap_growth(
        &mut self,
        growth: usize,
        request: &HeapOperationRequest<'_>,
    ) -> Result<(), HeapOperationResult> {
        if growth == 0 {
            return Ok(());
        }
        if self.machine.vm.heap.collection_due(growth) && !request.allow_collection {
            return Err(HeapOperationResult::Interpreter);
        }
        let roots = match decode_root_objects(request.roots) {
            Ok(roots) => roots,
            Err(CaptureDecodeFailure::Limit) => return Err(HeapOperationResult::HeapLimit),
            Err(CaptureDecodeFailure::Invalid) => {
                return Err(HeapOperationResult::Interpreter);
            }
        };
        match self
            .machine
            .reserve_native(growth, self.base_local, self.base_operand, &roots)
        {
            Ok(()) => Ok(()),
            Err(crate::FaultCode::HeapLimit) => Err(HeapOperationResult::HeapLimit),
            Err(fault) => Err(HeapOperationResult::Fault(fault)),
        }
    }

    pub(super) fn heap_object_value(reference: ObjRef) -> HeapOperationResult {
        HeapOperationResult::Value {
            bits: object_bits(reference),
            heap: None,
        }
    }

    pub(super) fn allocate_frozen_instance(
        &mut self,
        class: u32,
        fields: Vec<Value>,
        request: &HeapOperationRequest<'_>,
        extra_roots: &[ObjRef],
    ) -> Result<ObjRef, HeapOperationResult> {
        let reference = self.allocate_heap_reference(
            crate::Object::Instance {
                class,
                fields: fields.into(),
                env: Witness::EMPTY,
            },
            request,
            extra_roots,
        )?;
        self.machine.vm.heap.set_frozen(reference);
        Ok(reference)
    }

    pub(super) fn runtime_record_inline_allocations(&mut self, count: u64) {
        self.allocations = self.allocations.saturating_add(count);
        self.inline_allocations = self.inline_allocations.saturating_add(count);
    }

    pub(super) fn runtime_record_pending_instances(&mut self, allocations: u64, releases: u64) {
        self.pending_instance_allocations = self
            .pending_instance_allocations
            .saturating_add(allocations);
        self.pending_instance_releases = self.pending_instance_releases.saturating_add(releases);
    }

    pub(super) fn runtime_record_scalar_replacements(&mut self, allocations: u64) {
        self.scalar_replaced_allocations =
            self.scalar_replaced_allocations.saturating_add(allocations);
    }

    pub(super) fn runtime_allocate_instance(
        &mut self,
        class: u32,
        environment: u32,
        roots: NativeRoots<'_>,
        allow_collection: bool,
    ) -> AllocationResult {
        let Some(class_entry) = self.module.classes.get(class as usize) else {
            return AllocationResult::Interpreter;
        };
        let object = crate::Object::Instance {
            class,
            fields: vec![Value::Uninit; class_entry.fields.len()].into(),
            env: lm_value::Witness(lm_value::TypeEnvId(environment)),
        };
        self.allocate_object(object, roots, allow_collection)
    }

    pub(super) fn runtime_allocate_closure(
        &mut self,
        request: ClosureAllocationRequest<'_>,
    ) -> AllocationResult {
        let captures = match decode_captures(request.capture_bits, request.capture_tags) {
            Ok(captures) => captures,
            Err(CaptureDecodeFailure::Limit) => return AllocationResult::HeapLimit,
            Err(CaptureDecodeFailure::Invalid) => return AllocationResult::Interpreter,
        };
        let object = crate::Object::Closure {
            func: request.function,
            captures: captures.into(),
            env: lm_value::Witness(lm_value::TypeEnvId(request.environment)),
        };
        self.allocate_object(object, request.roots, request.allow_collection)
    }

    pub(super) fn runtime_allocate_callback(
        &mut self,
        request: CallbackAllocationRequest<'_>,
    ) -> CallbackAllocationResult {
        let captures = match decode_captures(request.capture_bits, request.capture_tags) {
            Ok(captures) => captures,
            Err(CaptureDecodeFailure::Limit) => return CallbackAllocationResult::StackLimit,
            Err(CaptureDecodeFailure::Invalid) => {
                return CallbackAllocationResult::Interpreter;
            }
        };
        match self.machine.alloc_callback_native(
            request.function,
            captures,
            lm_value::TypeEnvId(request.environment),
            request.owner_depth,
        ) {
            Ok(Value::Callback(reference)) => CallbackAllocationResult::Value {
                bits: reference_bits(reference),
            },
            Ok(_) => CallbackAllocationResult::Interpreter,
            Err(crate::FaultCode::StackLimit) => CallbackAllocationResult::StackLimit,
            Err(_) => CallbackAllocationResult::Interpreter,
        }
    }

    pub(super) fn runtime_allocate_tuple(
        &mut self,
        request: ValueArrayAllocationRequest<'_>,
    ) -> AllocationResult {
        let items = match decode_captures(request.item_bits, request.item_tags) {
            Ok(items) => items,
            Err(CaptureDecodeFailure::Limit) => return AllocationResult::HeapLimit,
            Err(CaptureDecodeFailure::Invalid) => return AllocationResult::Interpreter,
        };
        self.allocate_object(
            crate::Object::Tuple {
                items: items.into(),
            },
            request.roots,
            request.allow_collection,
        )
    }

    pub(super) fn runtime_allocate_list(
        &mut self,
        request: ValueArrayAllocationRequest<'_>,
    ) -> AllocationResult {
        let items = match decode_captures(request.item_bits, request.item_tags) {
            Ok(items) => items,
            Err(CaptureDecodeFailure::Limit) => return AllocationResult::HeapLimit,
            Err(CaptureDecodeFailure::Invalid) => return AllocationResult::Interpreter,
        };
        self.allocate_object(
            crate::Object::List {
                items: items.into(),
                epoch: StructuralEpoch::default(),
            },
            request.roots,
            request.allow_collection,
        )
    }

    pub(super) fn runtime_allocate_map(
        &mut self,
        request: ValueArrayAllocationRequest<'_>,
    ) -> AllocationResult {
        let flat = match decode_captures(request.item_bits, request.item_tags) {
            Ok(flat) => flat,
            Err(CaptureDecodeFailure::Limit) => return AllocationResult::HeapLimit,
            Err(CaptureDecodeFailure::Invalid) => return AllocationResult::Interpreter,
        };
        if flat.len() % 2 != 0 {
            return AllocationResult::Interpreter;
        }
        let mut entries: Vec<MapEntry> = Vec::new();
        let mut index = MapIndex::default();
        if entries.try_reserve_exact(flat.len() / 2).is_err() {
            return AllocationResult::HeapLimit;
        }
        for pair in flat.chunks_exact(2) {
            let key = pair[0];
            let value = pair[1];
            let semantic = match self.machine.key_semantic_hash(key) {
                Ok(semantic) => semantic,
                Err(_) => return AllocationResult::Interpreter,
            };
            let hash = Machine::map_index_hash(semantic);
            let hit = index
                .candidates(hash)
                .find(|position| self.machine.key_eq(entries[*position as usize].key, key));
            match hit {
                Some(position) => entries[position as usize].value = value,
                None => {
                    index.push_live(hash, entries.len() as u32);
                    entries.push(MapEntry {
                        key,
                        value,
                        semantic_hash: semantic,
                    });
                }
            }
        }
        self.allocate_object(
            crate::Object::Map {
                entries: entries.into(),
                index,
            },
            request.roots,
            request.allow_collection,
        )
    }
}
