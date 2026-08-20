//! The child budget, record reclamation, and the VM kernel.
//!
//! One part of the `World` surface. `world/mod.rs` holds the
//! state these methods read.

use super::*;

impl World {
    /// The largest live VM image count of this world.
    pub(crate) fn vm_image_limit(&self) -> usize {
        self.budget.limits.max_vm_images as usize
    }

    /// Reserve one child machine from the budget of `parent`.
    ///
    /// The parent holds a child budget. Each reservation charges one
    /// unit to the parent and hands the rest of the budget to the
    /// child, so the machine tower can never grow deeper than the
    /// budget the root minted. The reservation happens before any
    /// machine record exists, so a refusal changes nothing.
    ///
    /// The local budget bounds tower depth per branch. `WorldBudget`
    /// bounds the total machine count and shared resources.
    pub(crate) fn reserve_child(&mut self, parent: VmId) -> Option<VmConfig> {
        if !self.share_heap_budget() {
            return None;
        }
        if !self.can_reserve_child(parent) {
            // A dead record still holds a slot and a budget unit. Free
            // the dead records once, then answer the request again.
            self.collect_machines();
        }
        if !self.can_reserve_child(parent) {
            return None;
        }
        let m = &mut self.machines[parent as usize];
        let budget = m.config.max_children;
        m.children += 1;
        let remaining = budget - m.children;
        Some(VmConfig {
            max_children: remaining,
            ..m.config
        })
    }

    /// True when the parent can charge one more child right now.
    pub(super) fn can_reserve_child(&mut self, parent: VmId) -> bool {
        if !self.has_machine_room(1) {
            return false;
        }
        if self.vm_free.is_empty() && self.machines.try_reserve(1).is_err() {
            return false;
        }
        let m = &self.machines[parent as usize];
        m.children < m.config.max_children
    }

    /// True when the machine table can add `count` records.
    ///
    /// A free slot takes a new record, so the count of live records
    /// decides the limit, not the length of the table.
    pub(crate) fn has_machine_room(&self, count: usize) -> bool {
        self.machines
            .len()
            .saturating_sub(self.vm_free.len())
            .saturating_sub(self.mock_free.len())
            .checked_add(count)
            .is_some_and(|total| total <= self.budget.limits.max_machines as usize)
    }

    /// Reclaim every machine record that no live machine names.
    ///
    /// A machine is data (specification 1). A record that no live
    /// machine names can never run again and can never be inspected,
    /// so the world frees the record, returns the slot, and returns
    /// the child budget to the parent. A driver that restores one
    /// world for each branch of a search therefore pays for the
    /// branches it still holds, not for every branch it ever built.
    ///
    /// The reachability walk is the walk a snapshot cut uses
    /// (`machine_references`), so the live set here is the set a
    /// capture would close over.
    ///
    /// The pass is conservative. It keeps every machine it cannot
    /// prove dead: a machine on a live activation stack, a machine the
    /// scheduler owns, a paused machine, a machine one barrier holds,
    /// a machine that owns a host resource, and a machine that waits
    /// on the host. A walk that fails frees nothing.
    pub(crate) fn collect_machines(&mut self) -> usize {
        let count = self.machines.len();
        let mut free_slot = vec![false; count];
        for id in self.vm_free.iter().chain(self.mock_free.iter()) {
            free_slot[*id as usize] = true;
        }
        let mut live = vec![false; count];
        let mut queue: Vec<VmId> = Vec::new();
        let root = |live: &mut Vec<bool>, queue: &mut Vec<VmId>, vm: VmId| {
            if (vm as usize) < count && !live[vm as usize] {
                live[vm as usize] = true;
                queue.push(vm);
            }
        };
        for id in 0..count as VmId {
            if free_slot[id as usize] {
                continue;
            }
            let m = &self.machines[id as usize];
            // A machine with no parent is the world root or one mock
            // record. Neither takes part in the child budget.
            //
            // The embedding API can hold one empty record before a
            // guest value names it.
            if m.vm.parent.is_none()
                || m.vm.state == MachineState::Empty
                || m.active > 0
                || m.owner != Ownership::Holder
                || m.paused
                || m.barrier.is_some()
            {
                root(&mut live, &mut queue, id);
            }
        }
        for (holder, stack) in &self.suspended {
            root(&mut live, &mut queue, *holder);
            for act in &stack.activations {
                root(&mut live, &mut queue, act.vm);
                if let Some(reply) = act.reply_to {
                    root(&mut live, &mut queue, reply);
                }
            }
        }
        for group in &self.gate_groups {
            for member in &group.members {
                root(&mut live, &mut queue, *member);
            }
        }
        for bound in self.bound_resources.values() {
            root(&mut live, &mut queue, bound.owner);
        }
        let mut head = 0;
        while head < queue.len() {
            let vm = queue[head];
            head += 1;
            if !self.is_live_machine(vm) {
                continue;
            }
            let Ok(found) = self.machine_references(vm) else {
                // The walk proved nothing, so the pass frees nothing.
                return 0;
            };
            for target in found {
                if (target as usize) < count && !live[target as usize] {
                    live[target as usize] = true;
                    queue.push(target);
                }
            }
        }
        let mut freed = 0;
        for id in 0..count as VmId {
            if live[id as usize] || free_slot[id as usize] {
                continue;
            }
            let m = &self.machines[id as usize];
            if m.active > 0
                || m.owner != Ownership::Holder
                || m.paused
                || m.barrier.is_some()
                || m.resources.live_count() > 0
                || matches!(
                    m.vm.state,
                    MachineState::Running | MachineState::Waiting | MachineState::Blocked
                )
            {
                continue;
            }
            let parent = m.vm.parent;
            let generation = m.generation.wrapping_add(1);
            self.machines[id as usize] = self.empty_machine(self.config, None, generation);
            if let Some(up) = parent {
                let record = &mut self.machines[up as usize];
                record.children = record.children.saturating_sub(1);
            }
            self.vm_free.push(id);
            freed += 1;
        }
        self.collect_vm_images();
        self.collect_images();
        freed
    }

    /// Free every VM image that no surviving value or run names.
    pub(super) fn collect_vm_images(&mut self) {
        if self.vm_images.is_empty() {
            return;
        }
        let mut used = vec![false; self.vm_images.len()];
        let mut free_slot = vec![false; self.machines.len()];
        for id in self.vm_free.iter().chain(self.mock_free.iter()) {
            free_slot[*id as usize] = true;
        }
        for id in 0..self.machines.len() as VmId {
            if free_slot[id as usize] || !self.is_live_machine(id) {
                continue;
            }
            if let Some(key) = self.machines[id as usize].image {
                self.mark_live_image(key, &mut used);
            }
            let roots = self.machines[id as usize].snapshot_roots();
            let limits = self.machines[id as usize].config.graph;
            let order = {
                let machine = &mut self.machines[id as usize];
                match lm_graph::snapshot_ordinals(&mut machine.vm.heap, &roots, &limits) {
                    Ok(order) => order,
                    Err(_) => return,
                }
            };
            for reference in order {
                if let Object::NativeVm { image, generation } =
                    self.machines[id as usize].vm.heap.get(reference)
                {
                    self.mark_live_image(
                        VmImageKey {
                            image: *image,
                            generation: *generation,
                        },
                        &mut used,
                    );
                }
            }
        }
        for image in 0..self.vm_images.len() as u32 {
            let record = &mut self.vm_images[image as usize];
            if !record.live || used[image as usize] {
                continue;
            }
            record.live = false;
            record.generation = record.generation.wrapping_add(1);
            record.heap = Heap::with_budget(record.config.heap_bytes, self.budget.heap.clone());
            self.vm_image_free.push(image);
        }
    }

    fn mark_live_image(&self, key: VmImageKey, used: &mut [bool]) {
        let Some(record) = self.vm_images.get(key.image as usize) else {
            return;
        };
        if record.live && record.generation == key.generation {
            used[key.image as usize] = true;
        }
    }

    /// Reserve one persistent VM image record.
    pub(super) fn new_vm_image(&mut self, holder: VmId) -> Option<VmImageKey> {
        let live = self
            .vm_images
            .len()
            .saturating_sub(self.vm_image_free.len());
        if live >= self.budget.limits.max_vm_images as usize {
            self.collect_machines();
        }
        let live = self
            .vm_images
            .len()
            .saturating_sub(self.vm_image_free.len());
        if live >= self.budget.limits.max_vm_images as usize {
            return None;
        }
        let config = self.machines.get(holder as usize)?.config;
        if !self.share_heap_budget() {
            return None;
        }
        let mut slots = Vec::new();
        slots.try_reserve_exact(self.module.slots.len()).ok()?;
        for (index, slot) in self.module.slots.iter().enumerate() {
            if index >= self.base_slot_count {
                slots.push(ImageSlotTarget::Empty);
                continue;
            }
            slots.push(match slot.initial {
                Some(lm_bytecode::SlotTarget::Function(func)) => ImageSlotTarget::Function(func),
                Some(lm_bytecode::SlotTarget::Class(class)) => ImageSlotTarget::Class(class),
                None => ImageSlotTarget::Empty,
            });
        }
        if let Some(image) = self.vm_image_free.pop() {
            let record = &mut self.vm_images[image as usize];
            record.live = true;
            record.config = config;
            record.slots = slots;
            record.heap = Heap::with_budget(config.heap_bytes, self.budget.heap.clone());
            record.instances.clear();
            return Some(VmImageKey {
                image,
                generation: record.generation,
            });
        }
        self.vm_images.try_reserve(1).ok()?;
        let image = u32::try_from(self.vm_images.len()).ok()?;
        self.vm_images.push(VmImageRecord {
            generation: 0,
            live: true,
            config,
            slots,
            heap: Heap::with_budget(config.heap_bytes, self.budget.heap.clone()),
            instances: Vec::new(),
        });
        Some(VmImageKey {
            image,
            generation: 0,
        })
    }

    /// Reclaim one image whose handle allocation failed.
    fn rollback_vm_image(&mut self, key: VmImageKey) {
        let Some(record) = self.vm_images.get_mut(key.image as usize) else {
            return;
        };
        if !record.live || record.generation != key.generation {
            return;
        }
        record.live = false;
        record.generation = record.generation.wrapping_add(1);
        record.heap = Heap::with_budget(record.config.heap_bytes, self.budget.heap.clone());
        record.instances.clear();
        self.vm_image_free.push(key.image);
    }

    /// Install one verified linked artifact into one stopped image.
    ///
    /// The append linker preserves all existing numeric indices.
    /// The method commits only after the aggregate module verifies.
    pub(super) fn install_artifact(
        &mut self,
        key: VmImageKey,
        artifact: SharedBytes,
    ) -> Result<u32, String> {
        self.check_slot_safepoint(key)
            .map_err(|_| "the VM image is not at a safe installation point".to_string())?;

        let addition = lm_bytecode::decode(artifact.as_slice())
            .map_err(|error| format!("the artifact did not decode: {error}"))?;
        let admitted = crate::load(addition.clone())
            .map_err(|error| format!("the artifact did not verify: {error}"))?;
        let identity = admitted
            .identity()
            .map_err(|_| "the artifact has no semantic identity".to_string())?;
        let appended = lm_bytecode::append::append_linked(&self.module, &addition)?;
        let next = crate::load(appended.module)
            .map_err(|error| format!("the installed code did not verify: {error}"))?;

        let slot_count = next.module().slots.len();
        let target_index = key.image as usize;
        let instance_index = self.vm_images[target_index].instances.len();
        let instance_index = u32::try_from(instance_index)
            .map_err(|_| "the VM image has too many module instances".to_string())?;
        let installation = u32::try_from(self.installations.len())
            .map_err(|_| "the world has too many installations".to_string())?;

        for image in &mut self.vm_images {
            if image.live && image.slots.len() < slot_count {
                image
                    .slots
                    .try_reserve_exact(slot_count - image.slots.len())
                    .map_err(|_| "the VM image has no slot capacity".to_string())?;
            }
        }
        self.vm_images[target_index]
            .instances
            .try_reserve(1)
            .map_err(|_| "the VM image has no instance capacity".to_string())?;
        self.installations
            .try_reserve(1)
            .map_err(|_| "the world has no installation capacity".to_string())?;

        for image in &mut self.vm_images {
            if image.live {
                image.slots.resize(slot_count, ImageSlotTarget::Empty);
            }
        }
        let target = &mut self.vm_images[target_index];
        for (source, initial) in appended.slot_initials.iter().enumerate() {
            let slot = appended.reloc.slots[source] as usize;
            if matches!(target.slots[slot], ImageSlotTarget::Empty) {
                target.slots[slot] = match initial {
                    Some(lm_bytecode::SlotTarget::Function(func)) => {
                        ImageSlotTarget::Function(*func)
                    }
                    Some(lm_bytecode::SlotTarget::Class(class)) => ImageSlotTarget::Class(*class),
                    None => ImageSlotTarget::Empty,
                };
            }
        }
        target.instances.push(InstalledInstance {
            installation,
            artifact: artifact.clone(),
            semantic_hash: identity.semantic_hash,
            entry: appended.reloc.funcs[addition.entry as usize],
            funcs: appended.reloc.funcs.clone(),
            classes: appended.reloc.classes,
            slots: appended.reloc.slots,
            exports: addition
                .exports
                .iter()
                .filter(|export| export.kind == lm_bytecode::ExportKind::Function)
                .map(|export| {
                    (
                        export.name.clone(),
                        appended.reloc.funcs[export.def as usize],
                    )
                })
                .collect(),
        });
        self.loaded = next;
        self.module = self.loaded.module_store();
        self.dispatch = self.loaded.dispatch_store();
        self.core = self.loaded.core_layout();
        self.installations.push(artifact);
        Ok(instance_index)
    }

    /// Replace one callable slot at an image safepoint.
    #[allow(dead_code)]
    pub(crate) fn replace_function_slot(
        &mut self,
        key: VmImageKey,
        slot: u32,
        target: u32,
    ) -> Result<(), FaultCode> {
        let live = self
            .vm_images
            .get(key.image as usize)
            .is_some_and(|image| image.live && image.generation == key.generation);
        if !live {
            return Err(FaultCode::InvalidVmState);
        }
        if self
            .machines
            .iter()
            .any(|machine| machine.image == Some(key) && machine.active > 0)
        {
            return Err(FaultCode::InvalidVmState);
        }
        let spec = self
            .module
            .slots
            .get(slot as usize)
            .ok_or(FaultCode::TypeMismatch)?;
        let contract = match &spec.contract {
            lm_bytecode::SlotContract::Function(contract)
            | lm_bytecode::SlotContract::Method(contract) => contract,
            _ => return Err(FaultCode::TypeMismatch),
        };
        let func = self
            .module
            .funcs
            .get(target as usize)
            .ok_or(FaultCode::TypeMismatch)?;
        let bounds = self
            .module
            .func_bounds
            .get(target as usize)
            .ok_or(FaultCode::TypeMismatch)?;
        let method_ok = !matches!(&spec.contract, lm_bytecode::SlotContract::Method(_))
            || self
                .module
                .classes
                .iter()
                .any(|class| class.methods.iter().any(|(_, func)| *func == target));
        if !method_ok
            || !func.captures.is_empty()
            || func.type_params != contract.type_params
            || func.effect_params != contract.effect_params
            || bounds != &contract.type_bounds
            || func.params != contract.params
            || func.param_muts != contract.param_muts
            || func.ret != contract.ret
            || func.row != contract.row
        {
            return Err(FaultCode::TypeMismatch);
        }
        self.vm_images[key.image as usize].slots[slot as usize] = ImageSlotTarget::Function(target);
        Ok(())
    }

    /// Replace one class slot at an image safepoint.
    #[allow(dead_code)]
    pub(crate) fn replace_class_slot(
        &mut self,
        key: VmImageKey,
        slot: u32,
        target: u32,
    ) -> Result<(), FaultCode> {
        self.check_slot_safepoint(key)?;
        let spec = self
            .module
            .slots
            .get(slot as usize)
            .ok_or(FaultCode::TypeMismatch)?;
        let lm_bytecode::SlotContract::Class {
            type_params,
            abi,
            ty,
        } = &spec.contract
        else {
            return Err(FaultCode::TypeMismatch);
        };
        let class = self
            .module
            .classes
            .get(target as usize)
            .ok_or(FaultCode::TypeMismatch)?;
        let contract_class = match self.module.types.get(*ty as usize) {
            Some(lm_bytecode::BcType::Class(class)) | Some(lm_bytecode::BcType::Inst(class, _)) => {
                *class
            }
            _ => return Err(FaultCode::TypeMismatch),
        };
        let identity = self.identity()?;
        if class.type_params != *type_params
            || target != contract_class
            || identity.class_hashes.get(target as usize) != Some(abi)
        {
            return Err(FaultCode::TypeMismatch);
        }
        self.vm_images[key.image as usize].slots[slot as usize] = ImageSlotTarget::Class(target);
        Ok(())
    }

    /// Replace one value slot with a frozen image-owned copy.
    #[allow(dead_code)]
    pub(crate) fn replace_value_slot(
        &mut self,
        key: VmImageKey,
        slot: u32,
        source: VmId,
        value: Value,
    ) -> Result<(), FaultCode> {
        self.check_slot_safepoint(key)?;
        let ty = match self.module.slots.get(slot as usize) {
            Some(lm_bytecode::SlotSpec {
                contract: lm_bytecode::SlotContract::Value { ty },
                ..
            }) => *ty,
            _ => return Err(FaultCode::TypeMismatch),
        };
        let source_heap = &self
            .machines
            .get(source as usize)
            .ok_or(FaultCode::InvalidVmState)?
            .vm
            .heap;
        crate::typecheck::check_boundary_value(
            &self.module,
            source_heap,
            &mut self.envs,
            &mut self.check,
            value,
            ty,
            lm_value::TypeEnvId::EMPTY,
        )?;
        let roots: Vec<lm_value::ObjRef> = self.vm_images[key.image as usize]
            .slots
            .iter()
            .filter_map(|target| match target {
                ImageSlotTarget::Value(Value::Obj(reference)) => Some(*reference),
                _ => None,
            })
            .collect();
        let limits = self.vm_images[key.image as usize].config.graph;
        let moved = lm_graph::detach(
            &mut self.machines[source as usize].vm.heap,
            &mut self.vm_images[key.image as usize].heap,
            &roots,
            value,
            &limits,
        )?;
        self.vm_images[key.image as usize].slots[slot as usize] = ImageSlotTarget::Value(moved);
        Ok(())
    }

    /// Replace one process slot with a compatible live handle.
    #[allow(dead_code)]
    pub(crate) fn replace_process_slot(
        &mut self,
        key: VmImageKey,
        slot: u32,
        holder: VmId,
        handle: Value,
    ) -> Result<(), FaultCode> {
        self.check_slot_safepoint(key)?;
        let (message, result) = match self.module.slots.get(slot as usize) {
            Some(lm_bytecode::SlotSpec {
                contract: lm_bytecode::SlotContract::Process { message, result },
                ..
            }) => (*message, *result),
            _ => return Err(FaultCode::TypeMismatch),
        };
        let (proc, generation) = self
            .handle_proc(holder, handle)
            .ok_or(FaultCode::TypeMismatch)?;
        if !self.process_matches_contract(proc, generation, message, result)? {
            return Err(FaultCode::TypeMismatch);
        }
        self.vm_images[key.image as usize].slots[slot as usize] =
            ImageSlotTarget::Process { proc, generation };
        Ok(())
    }

    /// Copy one value slot target into a run heap.
    pub(super) fn load_value_slot(&mut self, vm: VmId, slot: u32) -> Result<(), FaultCode> {
        let key = self.machines[vm as usize]
            .image
            .ok_or(FaultCode::InvalidVmState)?;
        let value = match self
            .vm_images
            .get(key.image as usize)
            .filter(|record| record.live && record.generation == key.generation)
            .and_then(|record| record.slots.get(slot as usize))
        {
            Some(ImageSlotTarget::Value(value)) => *value,
            Some(ImageSlotTarget::Empty) => return Err(FaultCode::InvalidVmState),
            _ => return Err(FaultCode::MalformedState),
        };
        let ty = match self.module.slots.get(slot as usize) {
            Some(lm_bytecode::SlotSpec {
                contract: lm_bytecode::SlotContract::Value { ty },
                ..
            }) => *ty,
            _ => return Err(FaultCode::MalformedState),
        };
        crate::typecheck::check_boundary_value(
            &self.module,
            &self.vm_images[key.image as usize].heap,
            &mut self.envs,
            &mut self.check,
            value,
            ty,
            lm_value::TypeEnvId::EMPTY,
        )?;
        let moved = match value {
            Value::Obj(_) => {
                let roots = self.machines[vm as usize].gc_roots(&[]);
                let limits = self.machines[vm as usize].config.graph;
                lm_graph::transfer(
                    &mut self.vm_images[key.image as usize].heap,
                    &mut self.machines[vm as usize].vm.heap,
                    &roots,
                    value,
                    &limits,
                )?
            }
            scalar => scalar,
        };
        self.machines[vm as usize].push(moved)
    }

    /// Prove that an image exists and no owned run executes.
    fn check_slot_safepoint(&self, key: VmImageKey) -> Result<(), FaultCode> {
        let live = self
            .vm_images
            .get(key.image as usize)
            .is_some_and(|image| image.live && image.generation == key.generation);
        if !live
            || self
                .machines
                .iter()
                .any(|machine| machine.image == Some(key) && machine.active > 0)
        {
            return Err(FaultCode::InvalidVmState);
        }
        Ok(())
    }

    /// Test one process target against one closed slot contract.
    fn process_matches_contract(
        &mut self,
        proc: VmId,
        generation: u32,
        message: u32,
        result: u32,
    ) -> Result<bool, FaultCode> {
        let Some(machine) = self.machines.get(proc as usize) else {
            return Ok(false);
        };
        if machine.generation != generation || !machine.is_proc {
            return Ok(false);
        }
        let Some(body_index) = machine.body_func else {
            return Ok(false);
        };
        let body = self
            .module
            .funcs
            .get(body_index as usize)
            .ok_or(FaultCode::MalformedState)?;
        let Some(receiver) = body.params.first() else {
            return Ok(false);
        };
        let witness = machine.witness;
        let expected_message = self
            .envs
            .close(&self.module, message, lm_value::TypeEnvId::EMPTY)
            .map_err(|_| FaultCode::BoundaryLimit)?;
        let expected_result = self
            .envs
            .close(&self.module, result, lm_value::TypeEnvId::EMPTY)
            .map_err(|_| FaultCode::BoundaryLimit)?;
        let actual_result = self
            .envs
            .close(&self.module, body.ret, witness)
            .map_err(|_| FaultCode::BoundaryLimit)?;
        if actual_result != expected_result {
            return Ok(false);
        }
        let receiver = self
            .envs
            .close(&self.module, *receiver, witness)
            .map_err(|_| FaultCode::BoundaryLimit)?;
        let Some((class, args)) = self.envs.as_instance(receiver) else {
            return Ok(false);
        };
        let Some(proc_class) = self.core.proc_class else {
            return Err(FaultCode::MalformedState);
        };
        Ok(self
            .envs
            .ancestor_args(&self.module, class, &args, proc_class)
            .is_some_and(|args| args.as_slice() == [expected_message]))
    }

    /// Free every admitted image that no surviving machine names.
    ///
    /// A guest snapshot value names one slot of the image table. The
    /// pass reads the surviving machines, so it runs after the
    /// machine sweep and never frees an image a freed machine held.
    pub(super) fn collect_images(&mut self) {
        if self.images.is_empty() {
            return;
        }
        let mut used = vec![false; self.images.len()];
        let mut free_slot = vec![false; self.machines.len()];
        for id in self.vm_free.iter().chain(self.mock_free.iter()) {
            free_slot[*id as usize] = true;
        }
        for id in 0..self.machines.len() as VmId {
            if free_slot[id as usize] || !self.is_live_machine(id) {
                continue;
            }
            let Ok(found) = self.image_references(id) else {
                // The walk proved nothing, so the pass frees nothing.
                return;
            };
            for slot in found {
                if let Some(seen) = used.get_mut(slot as usize) {
                    *seen = true;
                }
            }
        }
        for slot in 0..self.images.len() as u32 {
            if used[slot as usize] || self.images[slot as usize].is_none() {
                continue;
            }
            self.images[slot as usize] = None;
            self.image_free.push(slot);
        }
    }

    /// Attach the aggregate heap ledger before a second machine exists.
    pub(crate) fn share_heap_budget(&mut self) -> bool {
        if self.heap_shared {
            return true;
        }
        if self.machines.len() != 1 {
            return false;
        }
        if !self.machines[0]
            .vm
            .heap
            .attach_budget(self.budget.heap.clone())
        {
            return false;
        }
        self.heap_shared = true;
        true
    }

    /// Create one detached machine with the world ledgers.
    pub(crate) fn empty_machine(
        &self,
        config: VmConfig,
        parent: Option<VmId>,
        generation: u32,
    ) -> Machine {
        debug_assert!(self.heap_shared);
        Machine::empty_with_budgets(
            config,
            parent,
            generation,
            self.budget.heap.clone(),
            self.budget.resources.clone(),
        )
    }

    /// Create one image-owned heap with the world ledger.
    pub(crate) fn empty_image_heap(&self, config: VmConfig) -> Heap {
        debug_assert!(self.heap_shared);
        Heap::with_budget(config.heap_bytes, self.budget.heap.clone())
    }

    /// Enter the proc body after the constructor frame returned.
    pub(super) fn enter_proc_body(&mut self, vm: VmId, instance: Value) {
        let Some(body) = self.machines[vm as usize].start_body.take() else {
            self.machines[vm as usize].set_fault(
                FaultCode::MalformedState,
                "the machine stores no proc body",
                None,
            );
            return;
        };
        let (func, env) = match self.machines[vm as usize].vm.heap.get(body) {
            Object::Closure { func, env, .. } => (*func, env.env()),
            _ => {
                self.machines[vm as usize].set_fault(
                    FaultCode::TypeMismatch,
                    "the proc body is not a closure",
                    None,
                );
                return;
            }
        };
        self.machines[vm as usize].load_frame(&self.module, func, vec![instance], Some(body), env);
    }

    /// Read one VM image handle out of a holder value.
    ///
    /// The argument comes from the pending record of the machine, and
    /// a restored machine states that record, so the read tests the
    /// shape. `None` faults the caller at its use site.
    pub(super) fn handle_vm(&self, holder: VmId, value: Value) -> Option<VmImageKey> {
        let r = value.as_obj()?;
        match self.machines[holder as usize].vm.heap.get(r) {
            Object::NativeVm { image, generation } => Some(VmImageKey {
                image: *image,
                generation: *generation,
            }),
            _ => None,
        }
    }

    /// Read one run handle out of a holder value.
    pub(super) fn handle_run(&self, holder: VmId, value: Value) -> Option<VmId> {
        let r = value.as_obj()?;
        match self.machines[holder as usize].vm.heap.get(r) {
            Object::NativeRun { vm } => Some(*vm),
            _ => None,
        }
    }

    /// The VM image one argument names, or a fault on the caller.
    pub(super) fn image_arg(&mut self, vm: VmId, op: u32, value: Value) -> Option<VmImageKey> {
        match self.handle_vm(vm, value) {
            Some(key)
                if self
                    .vm_images
                    .get(key.image as usize)
                    .is_some_and(|image| image.live && image.generation == key.generation) =>
            {
                Some(key)
            }
            None => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::TypeMismatch,
                    "the receiver is not a VM image handle",
                );
                None
            }
            Some(_) => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::InvalidVmState,
                    "the VM image handle is stale",
                );
                None
            }
        }
    }

    /// The run one argument names, or a fault on the caller.
    pub(super) fn run_arg(&mut self, vm: VmId, op: u32, value: Value) -> Option<VmId> {
        match self.handle_run(vm, value) {
            Some(target) => Some(target),
            None => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::TypeMismatch,
                    "the receiver is not a run handle",
                );
                None
            }
        }
    }

    /// Remove one child whose handle was not returned.
    pub(super) fn rollback_child(&mut self, parent: VmId, child: VmId) {
        let generation = self.machines[child as usize].generation;
        self.machines[child as usize] = self.empty_machine(self.config, None, generation);
        self.vm_free.push(child);
        self.machines[parent as usize].children =
            self.machines[parent as usize].children.saturating_sub(1);
    }

    /// Create one normal run record for an image.
    pub(super) fn prepare_run_target(&mut self, parent: VmId, image: VmImageKey) -> Option<VmId> {
        let run_config = self.reserve_child(parent)?;
        let image_config = self.vm_images.get(image.image as usize)?.config;
        let config = intersect_config(run_config, image_config);
        let target = self.install_child(config, parent);
        self.machines[target as usize].image = Some(image);
        Some(target)
    }

    /// Roll back a run target that no handle received.
    pub(super) fn rollback_run_target(&mut self, parent: VmId, target: VmId) {
        self.rollback_child(parent, target);
    }

    /// Execute one VM control operation of the machine `vm`.
    pub(super) fn kernel_exec(
        &mut self,
        stack: &mut Vec<Activation>,
        vm: VmId,
        op: u32,
        dispatch_mode: DispatchMode,
    ) {
        let stored: Vec<Value> = match self.machines[vm as usize].vm.pending.as_ref() {
            Some(pending) => pending.args.clone(),
            None => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::MalformedState,
                    "the kernel found no pending request",
                );
                return;
            }
        };
        // A restored machine states its own argument list. `arg` reads
        // a missing position as the uninitialized marker, and every
        // shape test below rejects that marker, so a short list faults
        // the caller instead of indexing past the list.
        let args = Args(&stored);
        match op {
            lm_abi::OP_VM_NEW => {
                let image = match self.new_vm_image(vm) {
                    Some(image) => image,
                    None => {
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::InvalidVmState,
                            "the world has no VM image capacity left",
                        );
                        return;
                    }
                };
                match self.machines[vm as usize].alloc(Object::NativeVm {
                    image: image.image,
                    generation: image.generation,
                }) {
                    Ok(handle) => self.install_value_reply(vm, handle),
                    Err(code) => {
                        self.rollback_vm_image(image);
                        self.machines[vm as usize].set_fault(code, "", Some(op));
                    }
                }
            }
            lm_abi::OP_VM_ARTIFACT
            | lm_abi::OP_VM_VERIFY
            | lm_abi::OP_VM_INSTALL
            | lm_abi::OP_VM_INSTANCE_ENTRY
            | lm_abi::OP_VM_INSTANCE_FUNCTION
            | lm_abi::OP_VM_INSTANCE_SLOT
            | lm_abi::OP_VM_INSTANCE_SLOT_SPEC
            | lm_abi::OP_VM_ACTIVATE_DEF
            | lm_abi::OP_VM_REPLACE_FUNCTION => self.code_exec(vm, op, args),
            lm_abi::OP_VM_ACTIVATE => {
                let Some(image) = self.image_arg(vm, op, args[0]) else {
                    return;
                };
                let target = match self.prepare_run_target(vm, image) {
                    Some(target) => target,
                    None => {
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::InvalidVmState,
                            "the VM image has no run budget left",
                        );
                        return;
                    }
                };
                let program = match self.transfer(vm, target, args[1]) {
                    Ok(value) => value,
                    Err(code) => {
                        self.rollback_run_target(vm, target);
                        self.fault_caller(vm, op, code, "the program is not sendable");
                        return;
                    }
                };
                // The argument view: unit, or a tuple whose elements
                /* become the initial parameter locals. */
                let Some(closure_ref) = program.as_obj() else {
                    self.rollback_run_target(vm, target);
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the program value is not a closure",
                    );
                    return;
                };
                let mut locals = Vec::new();
                if let Value::Obj(r) = args[2] {
                    let items = match self.machines[vm as usize].vm.heap.get(r) {
                        Object::Tuple { items } => items.clone(),
                        _ => {
                            self.rollback_run_target(vm, target);
                            self.fault_caller(
                                vm,
                                op,
                                FaultCode::TypeMismatch,
                                "the argument view is not a tuple",
                            );
                            return;
                        }
                    };
                    // The program is not reachable from the target
                    // machine yet, so it stays rooted while the
                    // arguments cross.
                    self.machines[target as usize]
                        .vm
                        .heap
                        .push_host_root(closure_ref);
                    let moved = self.transfer_all(vm, target, &items);
                    self.machines[target as usize]
                        .vm
                        .heap
                        .pop_host_root(closure_ref);
                    match moved {
                        Ok(values) => locals = values,
                        Err(code) => {
                            self.rollback_run_target(vm, target);
                            self.fault_caller(vm, op, code, "an argument is not sendable");
                            return;
                        }
                    }
                }
                // The program closure carries the environment of the
                // frame that built it, so a machine whose entry
                // function is generic records the arguments that frame
                // applied.
                let (func, env) = match self.machines[target as usize].vm.heap.get(closure_ref) {
                    Object::Closure { func, env, .. } => (*func, env.env()),
                    _ => {
                        self.rollback_run_target(vm, target);
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::TypeMismatch,
                            "the program value is not a closure",
                        );
                        return;
                    }
                };
                // The arguments cross a machine boundary, so they meet
                // the parameter types of the program before the frame
                // loads them.
                if let Err(code) = self.check_frame_args(target, func, env, &locals) {
                    self.rollback_run_target(vm, target);
                    self.fault_caller(vm, op, code, "an argument does not carry its declared type");
                    return;
                }
                self.machines[target as usize].load_frame(
                    &self.module,
                    func,
                    locals,
                    Some(closure_ref),
                    env,
                );
                match self.machines[vm as usize].alloc(Object::NativeRun { vm: target }) {
                    Ok(handle) => self.install_value_reply(vm, handle),
                    Err(code) => {
                        self.rollback_run_target(vm, target);
                        self.machines[vm as usize].set_fault(code, "", Some(op));
                    }
                }
            }
            lm_abi::OP_VM_RUN
            | lm_abi::OP_VM_STEP
            | lm_abi::OP_VM_DRIVE
            | lm_abi::OP_VM_DRIVE_FOR => {
                let Some(target) = self.run_arg(vm, op, args[0]) else {
                    return;
                };
                // `Vm.DriveFor` bounds the turn. A bound below one
                // retires nothing, so it reads as one instruction.
                let turn_fuel = if op == lm_abi::OP_VM_DRIVE_FOR {
                    match args[1] {
                        Value::Int(n) => Some((n.max(1)).min(u32::MAX as i64) as u32),
                        _ => {
                            self.fault_caller(
                                vm,
                                op,
                                FaultCode::TypeMismatch,
                                "`Vm.DriveFor` needs an instruction count",
                            );
                            return;
                        }
                    }
                } else {
                    None
                };
                let (mode, family) = match op {
                    lm_abi::OP_VM_RUN => (StopMode::RunToTerminal, Family::Run),
                    lm_abi::OP_VM_STEP => (StopMode::OneStep, Family::Step),
                    lm_abi::OP_VM_DRIVE_FOR => (StopMode::DriveToAsk, Family::DriveFor),
                    _ => (StopMode::DriveToAsk, Family::Drive),
                };
                if target == vm || self.machines[target as usize].active > 0 {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
                    return;
                }
                if !self.expect_holder_owned(vm, op, target) {
                    return;
                }
                // The first run, step, or drive of a restored root
                // opens the world gate (specification 17.5).
                self.open_gate(target);
                if self.machines[target as usize].vm.routed.is_some() {
                    if matches!(op, lm_abi::OP_VM_DRIVE | lm_abi::OP_VM_DRIVE_FOR) {
                        self.recover_routed_asked(target, vm, op);
                    } else {
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::InvalidVmState,
                            "the machine holds a routed request; drive it",
                        );
                    }
                    return;
                }
                match self.machines[target as usize].vm.state {
                    MachineState::Empty => {
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::InvalidVmState,
                            "the machine is empty",
                        );
                    }
                    MachineState::Done | MachineState::Faulted => {
                        // Terminal execution calls return the stored
                        // event idempotently.
                        match self.build_terminal_event(target, vm, family) {
                            Ok(value) => self.install_value_reply(vm, value),
                            Err(code) => {
                                self.machines[vm as usize].set_fault(code, "", Some(op));
                            }
                        }
                    }
                    MachineState::Asked => {
                        if matches!(op, lm_abi::OP_VM_DRIVE | lm_abi::OP_VM_DRIVE_FOR) {
                            // Token recovery: the same semantic request
                            // with a fresh holder token.
                            if self.machines[target as usize].vm.pending.is_none() {
                                self.fault_caller(
                                    vm,
                                    op,
                                    FaultCode::MalformedState,
                                    "the asked machine holds no request",
                                );
                                return;
                            }
                            let fresh = match self.machines[target as usize].take_request_ordinal()
                            {
                                Ok(ordinal) => ordinal,
                                Err(code) => {
                                    self.machines[target as usize].set_fault(
                                        code,
                                        "the request ordinal is exhausted",
                                        Some(op),
                                    );
                                    let built =
                                        self.build_terminal_event(target, vm, Family::Drive);
                                    self.reply_or_fault(vm, op, built);
                                    return;
                                }
                            };
                            if let Some(pending) =
                                self.machines[target as usize].vm.pending.as_mut()
                            {
                                pending.ordinal = fresh;
                            }
                            self.deliver_asked(target, vm, fresh);
                        } else {
                            self.fault_caller(
                                vm,
                                op,
                                FaultCode::InvalidVmState,
                                "the machine is asked; drive it",
                            );
                        }
                    }
                    // A blocked machine takes this path too. It waits
                    // on another machine of this world, exactly as a
                    // waiting machine waits on the host. The driver
                    // loop suspends the whole stack at its next turn,
                    // and the scheduler parks this holder on the same
                    // wake condition. The holder resumes its control
                    // call when the condition clears.
                    MachineState::Ready | MachineState::Waiting | MachineState::Blocked => {
                        if self.machines[vm as usize].vm.nested.is_some() {
                            self.fault_caller(
                                vm,
                                op,
                                FaultCode::InvalidVmState,
                                "the machine already waits on nested control",
                            );
                            return;
                        }
                        self.machines[vm as usize].vm.nested = Some(target);
                        if dispatch_mode == DispatchMode::DeferNested {
                            self.machines[vm as usize].vm.state = MachineState::Ready;
                        } else {
                            self.push_activation(
                                stack,
                                Activation {
                                    vm: target,
                                    mode,
                                    family,
                                    reply_to: Some(vm),
                                    retired: false,
                                    fuel: turn_fuel,
                                },
                            );
                        }
                    }
                    // A running machine holds an execution reference,
                    // and the guard above already refused one.
                    MachineState::Running => {
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::InvalidVmState,
                            "the machine is in use",
                        );
                    }
                }
            }
            lm_abi::OP_VM_DRIVE_WAIT => {
                let Some(target) = self.run_arg(vm, op, args[0]) else {
                    return;
                };
                if target == vm || self.machines[target as usize].active > 0 {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
                    return;
                }
                if !self.expect_holder_owned(vm, op, target) {
                    return;
                }
                if self.machines[target as usize].vm.state == MachineState::Empty {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is empty");
                    return;
                }
                self.create_wait(vm, op, WaitSource::Drive { target });
            }
            lm_abi::OP_VM_TABLE => {
                let Some(target) = self.run_arg(vm, op, args[0]) else {
                    return;
                };
                match self.machines[vm as usize].alloc(Object::NativeTable { vm: target }) {
                    Ok(handle) => self.install_value_reply(vm, handle),
                    Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
                }
            }
            lm_abi::OP_VM_HANDLES => {
                let Some(target) = self.run_arg(vm, op, args[0]) else {
                    return;
                };
                if target == vm || self.machines[target as usize].active > 0 {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
                    return;
                }
                if !self.expect_holder_owned(vm, op, target) {
                    return;
                }
                let built = self
                    .controlled_file_resources(target)
                    .and_then(|resources| self.build_resource_list(vm, target, &resources));
                self.reply_or_fault(vm, op, built);
            }
            lm_abi::OP_VM_RESOURCE => {
                let Some(target) = self.run_arg(vm, op, args[0]) else {
                    return;
                };
                if target == vm || self.machines[target as usize].active > 0 {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
                    return;
                }
                if !self.expect_holder_owned(vm, op, target) {
                    return;
                }
                let Some(resource) = self.file_handle_resource(vm, args[1]) else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not an external resource",
                    );
                    return;
                };
                let built = self.build_resource_control(vm, target, resource);
                self.reply_or_fault(vm, op, built);
            }
            lm_abi::OP_VM_SERVE_FILE => {
                let Some(surface) = self.run_arg(vm, op, args[0]) else {
                    return;
                };
                let found = args[1].as_obj().and_then(|reference| {
                    match self.machines[vm as usize].vm.heap.get(reference) {
                        Object::NativeCall {
                            vm,
                            ordinal,
                            op: call_op,
                        } => Some((*vm, *ordinal, *call_op)),
                        _ => None,
                    }
                });
                let Some(token) = found else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not a call token",
                    );
                    return;
                };
                let Some(sink) =
                    self.reply_sink(vm, op, surface, token.0, token.1, Some(lm_abi::OP_FS_OPEN))
                else {
                    return;
                };
                let resource = match self.register_driver_file(sink.target, vm) {
                    Ok(resource) => resource,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the resource limit is full");
                        return;
                    }
                };
                let built = self.machines[sink.target as usize]
                    .alloc(Object::NativeFileHandle { resource })
                    .and_then(|handle| {
                        self.make_instance(sink.target, self.core.result_ok, vec![handle])
                    });
                let reply = match built {
                    Ok(reply) => reply,
                    Err(code) => {
                        self.retire_file(resource, false);
                        self.fault_caller(vm, op, code, "the file reply allocation failed");
                        return;
                    }
                };
                let control = match self.build_resource_control(vm, surface, resource) {
                    Ok(control) => control,
                    Err(code) => {
                        self.retire_file(resource, false);
                        self.fault_caller(vm, op, code, "the resource control allocation failed");
                        return;
                    }
                };
                self.install_value_reply(sink.target, reply);
                if self.machines[sink.target as usize].vm.state == MachineState::Faulted {
                    self.retire_file(resource, false);
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the minted reply did not match Fs.Open",
                    );
                    return;
                }
                self.consume_reply_sink(sink);
                self.install_value_reply(vm, control);
            }
            lm_abi::OP_VM_SERVE_TCP_STREAM => {
                let Some(surface) = self.run_arg(vm, op, args[0]) else {
                    return;
                };
                let found = args[1].as_obj().and_then(|reference| {
                    match self.machines[vm as usize].vm.heap.get(reference) {
                        Object::NativeCall {
                            vm,
                            ordinal,
                            op: call_op,
                        } => Some((*vm, *ordinal, *call_op)),
                        _ => None,
                    }
                });
                let Some(token) = found else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not a call token",
                    );
                    return;
                };
                if token.2 != lm_abi::OP_TCP_CONNECT && token.2 != lm_abi::OP_TCP_ACCEPT {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::InvalidRequestToken,
                        "the call token is not for Tcp.Connect or Tcp.Accept",
                    );
                    return;
                }
                let Some(sink) = self.reply_sink(vm, op, surface, token.0, token.1, Some(token.2))
                else {
                    return;
                };
                let peer = match self.transfer(vm, sink.target, args[2]) {
                    Ok(peer) => peer,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the peer address is not sendable");
                        return;
                    }
                };
                let resource = match self.register_driver_resource(
                    sink.target,
                    vm,
                    crate::ResourceKind::TcpStream,
                    token.2,
                ) {
                    Ok(resource) => resource,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the resource limit is full");
                        return;
                    }
                };
                let built = self.machines[sink.target as usize]
                    .alloc(Object::NativeTcpStream { resource })
                    .and_then(|stream| {
                        if token.2 == lm_abi::OP_TCP_ACCEPT {
                            self.make_instance(sink.target, self.core.pair, vec![stream, peer])
                        } else {
                            Ok(stream)
                        }
                    })
                    .and_then(|value| {
                        self.make_instance(sink.target, self.core.result_ok, vec![value])
                    });
                let reply = match built {
                    Ok(reply) => reply,
                    Err(code) => {
                        self.retire_resource(resource, false);
                        self.fault_caller(vm, op, code, "the TCP reply allocation failed");
                        return;
                    }
                };
                let control = match self.build_resource_control(vm, surface, resource) {
                    Ok(control) => control,
                    Err(code) => {
                        self.retire_resource(resource, false);
                        self.fault_caller(vm, op, code, "the resource control allocation failed");
                        return;
                    }
                };
                self.install_value_reply(sink.target, reply);
                if self.machines[sink.target as usize].vm.state == MachineState::Faulted {
                    self.retire_resource(resource, false);
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the minted reply did not match the TCP call",
                    );
                    return;
                }
                self.consume_reply_sink(sink);
                self.install_value_reply(vm, control);
            }
            lm_abi::OP_VM_SERVE_TCP_LISTENER => {
                let Some(surface) = self.run_arg(vm, op, args[0]) else {
                    return;
                };
                let found = args[1].as_obj().and_then(|reference| {
                    match self.machines[vm as usize].vm.heap.get(reference) {
                        Object::NativeCall {
                            vm,
                            ordinal,
                            op: call_op,
                        } => Some((*vm, *ordinal, *call_op)),
                        _ => None,
                    }
                });
                let Some(token) = found else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not a call token",
                    );
                    return;
                };
                let Some(sink) = self.reply_sink(
                    vm,
                    op,
                    surface,
                    token.0,
                    token.1,
                    Some(lm_abi::OP_TCP_LISTEN),
                ) else {
                    return;
                };
                let resource = match self.register_driver_resource(
                    sink.target,
                    vm,
                    crate::ResourceKind::TcpListener,
                    lm_abi::OP_TCP_LISTEN,
                ) {
                    Ok(resource) => resource,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the resource limit is full");
                        return;
                    }
                };
                let built = self.machines[sink.target as usize]
                    .alloc(Object::NativeTcpListener { resource })
                    .and_then(|listener| {
                        self.make_instance(sink.target, self.core.result_ok, vec![listener])
                    });
                let reply = match built {
                    Ok(reply) => reply,
                    Err(code) => {
                        self.retire_resource(resource, false);
                        self.fault_caller(vm, op, code, "the TCP reply allocation failed");
                        return;
                    }
                };
                let control = match self.build_resource_control(vm, surface, resource) {
                    Ok(control) => control,
                    Err(code) => {
                        self.retire_resource(resource, false);
                        self.fault_caller(vm, op, code, "the resource control allocation failed");
                        return;
                    }
                };
                self.install_value_reply(sink.target, reply);
                if self.machines[sink.target as usize].vm.state == MachineState::Faulted {
                    self.retire_resource(resource, false);
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the minted reply did not match Tcp.Listen",
                    );
                    return;
                }
                self.consume_reply_sink(sink);
                self.install_value_reply(vm, control);
            }
            lm_abi::OP_VM_SERVE_TLS_STREAM => {
                let Some(surface) = self.run_arg(vm, op, args[0]) else {
                    return;
                };
                let found = args[1].as_obj().and_then(|reference| {
                    match self.machines[vm as usize].vm.heap.get(reference) {
                        Object::NativeCall {
                            vm,
                            ordinal,
                            op: call_op,
                        } => Some((*vm, *ordinal, *call_op)),
                        _ => None,
                    }
                });
                let Some(token) = found else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not a call token",
                    );
                    return;
                };
                let Some(sink) = self.reply_sink(
                    vm,
                    op,
                    surface,
                    token.0,
                    token.1,
                    Some(lm_abi::OP_TLS_HANDSHAKE),
                ) else {
                    return;
                };
                let Some(source) = self.pending_resource_of(sink.target, ResourceErrors::Net)
                else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the TLS handshake has no TCP stream",
                    );
                    return;
                };
                let valid_source = self.bound_resources.get(&source).is_some_and(|bound| {
                    bound.owner == sink.target
                        && bound.kind == crate::ResourceKind::TcpStream
                        && matches!(bound.backing, ResourceBacking::Driver(driver) if driver == vm)
                });
                if !valid_source {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the TLS handshake does not use this driver's TCP stream",
                    );
                    return;
                }
                // The handshake consumes its TCP stream.
                self.retire_resource(source, false);
                let resource = match self.register_driver_resource(
                    sink.target,
                    vm,
                    crate::ResourceKind::TlsStream,
                    lm_abi::OP_TLS_HANDSHAKE,
                ) {
                    Ok(resource) => resource,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the resource limit is full");
                        return;
                    }
                };
                let built = self.machines[sink.target as usize]
                    .alloc(Object::NativeTlsStream { resource })
                    .and_then(|stream| {
                        self.make_instance(sink.target, self.core.result_ok, vec![stream])
                    });
                let reply = match built {
                    Ok(reply) => reply,
                    Err(code) => {
                        self.retire_resource(resource, false);
                        self.fault_caller(vm, op, code, "the TLS reply allocation failed");
                        return;
                    }
                };
                let control = match self.build_resource_control(vm, surface, resource) {
                    Ok(control) => control,
                    Err(code) => {
                        self.retire_resource(resource, false);
                        self.fault_caller(vm, op, code, "the resource control allocation failed");
                        return;
                    }
                };
                self.install_value_reply(sink.target, reply);
                if self.machines[sink.target as usize].vm.state == MachineState::Faulted {
                    self.retire_resource(resource, false);
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the minted reply did not match Tls.Handshake",
                    );
                    return;
                }
                self.consume_reply_sink(sink);
                self.install_value_reply(vm, control);
            }
            lm_abi::OP_VM_RESOURCE_SAME => {
                let Some((_left_surface, left)) = self.resource_control(vm, args[0]) else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the receiver is not a resource control",
                    );
                    return;
                };
                let Some((_right_surface, right)) = self.resource_control(vm, args[1]) else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not a resource control",
                    );
                    return;
                };
                let same = left == right && self.bound_resources.contains_key(&left);
                self.install_value_reply(vm, Value::Bool(same));
            }
            lm_abi::OP_VM_RESOURCE_IS_OPEN
            | lm_abi::OP_VM_RESOURCE_CLOSE
            | lm_abi::OP_VM_RESOURCE_KIND => {
                let Some((_surface, resource)) = self.resource_control(vm, args[0]) else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the receiver is not a resource control",
                    );
                    return;
                };
                match op {
                    lm_abi::OP_VM_RESOURCE_IS_OPEN => {
                        let open = self.bound_resources.contains_key(&resource);
                        self.install_value_reply(vm, Value::Bool(open));
                    }
                    lm_abi::OP_VM_RESOURCE_CLOSE => {
                        let closed = self.retire_resource(resource, true);
                        self.install_value_reply(vm, Value::Bool(closed));
                    }
                    _ => {
                        let name = self
                            .bound_resources
                            .get(&resource)
                            .map(|bound| match bound.kind {
                                crate::ResourceKind::File => "file",
                                crate::ResourceKind::TcpStream => "tcp-stream",
                                crate::ResourceKind::TcpListener => "tcp-listener",
                                crate::ResourceKind::TlsStream => "tls-stream",
                                crate::ResourceKind::PendingOperation => "pending-operation",
                            })
                            .unwrap_or("closed");
                        let built = self.machines[vm as usize].alloc(Object::Str(name.into()));
                        self.reply_or_fault(vm, op, built);
                    }
                }
            }
            lm_abi::OP_VM_ANSWER => {
                let Some(surface) = self.run_arg(vm, op, args[0]) else {
                    return;
                };
                let found = args[1].as_obj().and_then(|r| {
                    match self.machines[vm as usize].vm.heap.get(r) {
                        Object::NativeCall { vm, ordinal, op } => Some((*vm, *ordinal, *op)),
                        _ => None,
                    }
                });
                let Some(token) = found else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not a call token",
                    );
                    return;
                };
                let Some(sink) = self.reply_sink(vm, op, surface, token.0, token.1, Some(token.2))
                else {
                    return;
                };
                let reply = match self.transfer(vm, sink.target, args[2]) {
                    Ok(value) => value,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the reply is not sendable");
                        return;
                    }
                };
                self.install_value_reply(sink.target, reply);
                self.consume_reply_sink(sink);
                self.install_value_reply(vm, Value::Unit);
            }
            lm_abi::OP_VM_REJECT | lm_abi::OP_VM_DISPATCH => {
                let Some(surface) = self.run_arg(vm, op, args[0]) else {
                    return;
                };
                let found = args[1].as_obj().and_then(|r| {
                    match self.machines[vm as usize].vm.heap.get(r) {
                        Object::NativeRequest { vm, ordinal } => Some((*vm, *ordinal)),
                        _ => None,
                    }
                });
                let Some(token) = found else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not a request token",
                    );
                    return;
                };
                let Some(sink) = self.reply_sink(vm, op, surface, token.0, token.1, None) else {
                    return;
                };
                if op == lm_abi::OP_VM_REJECT {
                    let built = args[2].as_obj().and_then(|r| {
                        match self.machines[vm as usize].vm.heap.get(r) {
                            Object::NativeFault { code, message, op } => Some(FaultRec {
                                code: *code,
                                message: message.clone(),
                                op: *op,
                            }),
                            _ => None,
                        }
                    });
                    let Some(rec) = built else {
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::TypeMismatch,
                            "the argument is not a fault value",
                        );
                        return;
                    };
                    let pending_op = self.pending_op(sink.target);
                    self.machines[sink.target as usize].set_fault(
                        rec.code,
                        rec.message,
                        pending_op,
                    );
                    self.consume_reply_sink(sink);
                    self.install_value_reply(vm, Value::Unit);
                } else {
                    // The caller's reply installs before policy can
                    // stack a mock run above it.
                    self.consume_reply_sink(sink);
                    self.install_value_reply(vm, Value::Unit);
                    let _ = self.resolve_and_dispatch(
                        stack,
                        sink.target,
                        sink.cursor,
                        DispatchMode::DeferNested,
                    );
                }
            }
            lm_abi::OP_VM_SNAPSHOT_HELD => {
                let Some(target) = self.run_arg(vm, op, args[0]) else {
                    return;
                };
                if target == vm || self.machines[target as usize].active > 0 {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
                    return;
                }
                if !self.expect_holder_owned(vm, op, target) {
                    return;
                }
                self.take_snapshot(vm, op, target, false);
            }
            lm_abi::OP_VM_SNAPSHOT_WAIT_HELD => {
                let Some(target) = self.run_arg(vm, op, args[0]) else {
                    return;
                };
                let Value::Int(fuel) = args[1] else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the fuel argument is not an integer",
                    );
                    return;
                };
                if target == vm || self.machines[target as usize].active > 0 {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
                    return;
                }
                if !self.expect_holder_owned(vm, op, target) {
                    return;
                }
                let result = self.snapshot_wait(target, fuel.max(0) as u64);
                self.install_snapshot_result(vm, op, result);
            }
            lm_abi::OP_VM_SNAPSHOT_SELF => {
                // The performing machine is the root of its own world.
                // The capture runs while `Vm.SnapshotSelf` is pending,
                // so the restored root holds that request
                // (specification 17.6).
                self.take_snapshot(vm, op, vm, true);
            }
            lm_abi::OP_VM_LOAD_SNAPSHOT => {
                // This build has no guest snapshot decoder.
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::InvalidVmState,
                    "Vm.LoadSnapshot is not available in this build",
                );
            }
            lm_abi::OP_VM_RESTORE => self.restore_snapshot(vm, op, args),
            lm_abi::OP_PROC_RECV_WAIT
            | lm_abi::OP_WAIT_WAIT
            | lm_abi::OP_WAIT_CHOOSE
            | lm_abi::OP_WAIT_CANCEL => self.wait_exec(vm, op, args),
            lm_abi::OP_PROC_RUN
            | lm_abi::OP_PROC_SPAWN
            | lm_abi::OP_PROC_SEND
            | lm_abi::OP_PROC_CLOSE
            | lm_abi::OP_PROC_RECV
            | lm_abi::OP_PROC_DONE
            | lm_abi::OP_PROC_PAUSE
            | lm_abi::OP_PROC_RESUME
            | lm_abi::OP_PROC_SNAPSHOT_WAIT => self.proc_exec(vm, op, stored),
            // Every `VmControl` slot of the manifest has an arm above.
            // A slot without one names a manifest this build does not
            // hold, so the caller faults.
            _ => self.fault_caller(
                vm,
                op,
                FaultCode::MalformedState,
                "the operation has no kernel rule",
            ),
        }
    }

    // ------------------------------------------------------------
    // Typed waits.
    // ------------------------------------------------------------
}

/// Intersect a run reservation with its persistent image ceiling.
fn intersect_config(run: VmConfig, image: VmConfig) -> VmConfig {
    VmConfig {
        fuel: run.fuel.min(image.fuel),
        max_frames: run.max_frames.min(image.max_frames),
        max_stack_values: run.max_stack_values.min(image.max_stack_values),
        heap_bytes: run.heap_bytes.min(image.heap_bytes),
        graph: lm_graph::GraphLimits {
            max_objects: run.graph.max_objects.min(image.graph.max_objects),
            max_edges: run.graph.max_edges.min(image.graph.max_edges),
            max_bytes: run.graph.max_bytes.min(image.graph.max_bytes),
            max_work: run.graph.max_work.min(image.graph.max_work),
        },
        max_children: run.max_children.min(image.max_children),
        max_resources: run.max_resources.min(image.max_resources),
        mailbox_limit: run.mailbox_limit.min(image.mailbox_limit),
        snapshot_bytes: run.snapshot_bytes.min(image.snapshot_bytes),
        max_closed_types: run.max_closed_types.min(image.max_closed_types),
        max_type_envs: run.max_type_envs.min(image.max_type_envs),
    }
}
