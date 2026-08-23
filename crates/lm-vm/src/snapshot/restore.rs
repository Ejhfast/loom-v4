//! Restore one admitted image into a machine world.
//!
//! Preparation builds all restored state outside the live machine
//! table. Commit installs that state without a fallible operation.

use super::{
    ImageBlock, ImageMachine, ImagePolicyCursor, ImageState, ImageTerminal, ImageWaitSource,
    RestoreFail, SnapshotImage,
};
use crate::machine::{
    Action, Block, CallbackDescriptor, CallbackSlot, FaultRec, Frame, FrameCapture,
    ImageSlotTarget as RuntimeSlotTarget, Machine, MachineState, Mailbox, Ownership, Pending,
    PolicyCursor, RoutedRequest, Terminal, VmId, VmImageKey, WaitEntry, WaitSource,
};
use crate::world::{InstalledInstance, VmImageRecord, World};
use crate::{LoadedModule, VmConfig};
use lm_bytecode::closed::TypeImportPlan;
use lm_heap::{Heap, Object, SharedBytes};
use lm_value::{ObjRef, TypeEnvId, Value, Witness};
use std::sync::Arc;

/// One complete restore that is ready for commit.
pub(crate) struct RestorePlan {
    target: Option<VmId>,
    restorer: VmId,
    machines: Vec<Machine>,
    types: TypeImportPlan,
    child_charge: u32,
    gate: u32,
    gate_members: Vec<VmId>,
    image_records: Vec<(u32, VmImageRecord)>,
    image_appended: usize,
    image_replacement: Option<(u32, VmImageRecord)>,
    image_config_update: Option<(u32, VmConfig)>,
    loaded: Option<LoadedModule>,
    installations: Vec<SharedBytes>,
}

/// Portable image records prepared before restore commit.
struct PreparedImages {
    keys: Vec<VmImageKey>,
    records: Vec<(u32, VmImageRecord)>,
    appended: usize,
    replacement: Option<(u32, VmImageRecord)>,
    config_update: Option<(u32, VmConfig)>,
}

/// One prepared full VM restore and its reserved registry entries.
pub(crate) struct VmRestorePlan {
    restore: RestorePlan,
    pub(crate) image: VmImageKey,
    target: Option<VmId>,
}

impl World {
    /// Restore one admitted image and return its root identifier.
    pub fn restore_image(
        &mut self,
        restorer: VmId,
        target: VmId,
        admitted: &SnapshotImage,
    ) -> Result<VmId, RestoreFail> {
        let plan = self.prepare_restore(restorer, target, admitted)?;
        Ok(self
            .commit_restore(plan)
            .expect("a typed restore has one distinguished machine"))
    }

    /// Build one restore without changing semantic world state.
    pub(crate) fn prepare_restore(
        &mut self,
        restorer: VmId,
        target: VmId,
        admitted: &SnapshotImage,
    ) -> Result<RestorePlan, RestoreFail> {
        let image = admitted.world();
        if image.distinguished != Some(0) || image.full_vm.is_some() {
            return Err(RestoreFail::OtherProgram);
        }
        let target_image = self
            .machines
            .get(target as usize)
            .and_then(|machine| machine.image);
        let reused_image = target_image.and_then(|_| image.machines[0].image);
        self.prepare_restore_inner(
            restorer,
            Some(target),
            target_image,
            reused_image,
            true,
            admitted,
        )
    }

    /// Prepare one full VM restore without exposing a partial image.
    pub(crate) fn prepare_vm_restore(
        &mut self,
        restorer: VmId,
        admitted: &SnapshotImage,
    ) -> Result<VmRestorePlan, RestoreFail> {
        let image = admitted.world();
        let Some(source_image) = image.full_vm else {
            return Err(RestoreFail::OtherProgram);
        };
        if image.distinguished.is_some() {
            return Err(RestoreFail::OtherProgram);
        }
        let target_image = self
            .new_vm_image(restorer)
            .ok_or(RestoreFail::LimitExceeded)?;
        let target = if image.machines.is_empty() {
            None
        } else {
            match self.prepare_run_target(restorer, target_image) {
                Some(target) => Some(target),
                None => {
                    self.rollback_vm_image(target_image);
                    return Err(RestoreFail::LimitExceeded);
                }
            }
        };
        match self.prepare_restore_inner(
            restorer,
            target,
            Some(target_image),
            Some(source_image),
            false,
            admitted,
        ) {
            Ok(restore) => Ok(VmRestorePlan {
                restore,
                image: target_image,
                target,
            }),
            Err(error) => {
                if let Some(target) = target {
                    self.rollback_run_target(restorer, target);
                }
                self.rollback_vm_image(target_image);
                Err(error)
            }
        }
    }

    /// Cancel one prepared full VM restore.
    pub(crate) fn discard_vm_restore(&mut self, restorer: VmId, plan: VmRestorePlan) {
        if let Some(target) = plan.target {
            self.rollback_run_target(restorer, target);
        }
        self.rollback_vm_image(plan.image);
    }

    /// Commit one prepared full VM restore.
    pub(crate) fn commit_vm_restore(&mut self, plan: VmRestorePlan) -> VmImageKey {
        let image = plan.image;
        let _ = self.commit_restore(plan.restore);
        image
    }

    /// Build one restore for a selected machine or a complete VM.
    #[allow(clippy::too_many_arguments)]
    fn prepare_restore_inner(
        &mut self,
        restorer: VmId,
        target: Option<VmId>,
        target_image: Option<VmImageKey>,
        reused_image: Option<u32>,
        attach_target_image: bool,
        admitted: &SnapshotImage,
    ) -> Result<RestorePlan, RestoreFail> {
        let identity = admitted.identity();
        let base = self
            .base_identity()
            .map_err(|_| RestoreFail::OtherProgram)?;
        if identity.base_semantic != base.semantic_hash
            || identity.base_verification != self.base_verification_hash()
            || identity.bundle_digest != self.loaded.bundle().digest()
        {
            return Err(RestoreFail::OtherProgram);
        }
        let aggregate_matches = identity.module_semantic
            == self
                .identity()
                .map_err(|_| RestoreFail::OtherProgram)?
                .semantic_hash
            && identity.verification == self.verification_hash();
        let current_is_base = self.verification_hash() == self.base_verification_hash();
        if !aggregate_matches && !current_is_base {
            return Err(RestoreFail::OtherProgram);
        }
        let loaded = (!aggregate_matches).then(|| admitted.loaded().clone());
        if aggregate_matches
            && (self.installations.len() != admitted.world().installations.len()
                || self
                    .installations
                    .iter()
                    .zip(&admitted.world().installations)
                    .any(|(left, right)| left.as_slice() != right.as_slice()))
        {
            return Err(RestoreFail::OtherProgram);
        }
        if self.machines.get(restorer as usize).is_none()
            || target.is_some_and(|target| {
                restorer == target
                    || self.machines.get(target as usize).is_none()
                    || self.machines[target as usize].vm.state != MachineState::Empty
            })
        {
            return Err(RestoreFail::LimitExceeded);
        }
        // Every restored machine takes the aggregate heap ledger, so
        // the ledger must hold the storage of the root machine first.
        // A caller reaches this method through `new_child` today, and
        // that call attaches the ledger. This method is public, so it
        // repeats the step instead of depending on the caller.
        if !self.share_heap_budget() {
            return Err(RestoreFail::LimitExceeded);
        }

        let image = admitted.world();
        let count = image.machines.len();
        if (count == 0 && target.is_some()) || (count != 0 && target.is_none()) {
            return Err(RestoreFail::OtherProgram);
        }
        let added = count.saturating_sub(usize::from(target.is_some()));
        if !self.has_machine_room(added) {
            return Err(RestoreFail::LimitExceeded);
        }
        let child_charge = u32::try_from(added).map_err(|_| RestoreFail::LimitExceeded)?;
        let restorer_record = &self.machines[restorer as usize];
        let charged = restorer_record
            .children
            .checked_add(child_charge)
            .ok_or(RestoreFail::LimitExceeded)?;
        if charged > restorer_record.config.max_children {
            return Err(RestoreFail::LimitExceeded);
        }
        self.machines
            .try_reserve_exact(added)
            .map_err(|_| RestoreFail::LimitExceeded)?;
        let gate = self
            .gate_marker()
            .checked_add(1)
            .ok_or(RestoreFail::LimitExceeded)?;
        let active_added = image
            .machines
            .iter()
            .filter(|machine| {
                machine.scheduler_owned
                    && !machine.paused
                    && !matches!(machine.state, ImageState::Done | ImageState::Faulted)
            })
            .count();
        self.prepare_scheduler_procs(self.machines.len() + added, active_added)
            .map_err(|_| RestoreFail::LimitExceeded)?;
        self.prepare_gate_group()
            .map_err(|_| RestoreFail::LimitExceeded)?;

        let image = admitted.world();
        let installation_base = if loaded.is_some() {
            u32::try_from(self.installations.len()).map_err(|_| RestoreFail::LimitExceeded)?
        } else {
            0
        };
        let mut artifact_table = try_vec(image.installations.len())?;
        let mut installations = try_vec(if loaded.is_some() {
            image.installations.len()
        } else {
            0
        })?;
        if loaded.is_some() {
            self.installations
                .try_reserve_exact(image.installations.len())
                .map_err(|_| RestoreFail::LimitExceeded)?;
            for artifact in &image.installations {
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(artifact.len())
                    .map_err(|_| RestoreFail::LimitExceeded)?;
                bytes.extend_from_slice(artifact);
                let bytes = SharedBytes::from(bytes);
                artifact_table.push(bytes.clone());
                installations.push(bytes);
            }
        } else {
            artifact_table.extend(self.installations.iter().cloned());
        }
        if let Some(code) = &loaded {
            let count = code.module().slots.len();
            for image in &mut self.vm_images {
                if image.live && image.slots.len() < count {
                    let additional = count - image.slots.len();
                    Arc::make_mut(&mut image.slots)
                        .try_reserve_exact(additional)
                        .map_err(|_| RestoreFail::LimitExceeded)?;
                }
            }
        }

        let types = self
            .envs
            .prepare_import(&image.types, &image.envs)
            .map_err(|_| RestoreFail::LimitExceeded)?;
        let env_map = types.env_map();
        let type_map = types.type_map();

        let mut ids = try_vec(count)?;
        if let Some(target) = target {
            ids.push(target);
        }
        for offset in 0..added {
            let raw = self
                .machines
                .len()
                .checked_add(offset)
                .ok_or(RestoreFail::LimitExceeded)?;
            ids.push(u32::try_from(raw).map_err(|_| RestoreFail::LimitExceeded)?);
        }

        let prepared_images = self.prepare_image_import(
            target_image,
            reused_image,
            target
                .map(|target| self.config_of(target))
                .or_else(|| {
                    target_image.and_then(|key| {
                        self.vm_images
                            .get(key.image as usize)
                            .map(|image| image.config)
                    })
                })
                .unwrap_or_else(|| self.config_of(restorer)),
            image,
            &ids,
            env_map,
            type_map,
            installation_base,
            &artifact_table,
        )?;

        let ceiling = target
            .map(|target| self.config_of(target))
            .or_else(|| {
                target_image.and_then(|key| {
                    self.vm_images
                        .get(key.image as usize)
                        .map(|image| image.config)
                })
            })
            .unwrap_or_else(|| self.config_of(restorer));
        let mut configs = try_vec(count)?;
        for source in &image.machines {
            configs.push(clamp(source, ceiling));
        }
        let mut child_counts = try_vec(count)?;
        child_counts.resize(count, 0u32);
        for source in &image.machines {
            if let Some(parent) = source.parent {
                let slot = child_counts
                    .get_mut(parent as usize)
                    .ok_or(RestoreFail::LimitExceeded)?;
                *slot = slot.checked_add(1).ok_or(RestoreFail::LimitExceeded)?;
            }
        }
        for ((source, config), children) in image
            .machines
            .iter()
            .zip(configs.iter())
            .zip(child_counts.iter())
        {
            check_effective_limits(source, *config, *children)?;
        }

        let mut generations = try_vec(count)?;
        generations.extend(image.machines.iter().map(|machine| machine.generation));
        let mut machines = try_vec(count)?;
        for (ordinal, source) in image.machines.iter().enumerate() {
            let mut machine = self.empty_machine(configs[ordinal], None, source.generation);
            let refs = restore_heap(
                &mut machine,
                source,
                &ids,
                &prepared_images.keys,
                env_map,
                type_map,
            )?;
            restore_state(
                &mut machine,
                source,
                &ids,
                &generations,
                env_map,
                type_map,
                &refs,
                restorer,
                gate,
                child_counts[ordinal],
            )?;
            machine.image = source
                .image
                .map(|image| prepared_images.keys[image as usize]);
            machines.push(machine);
        }
        if attach_target_image {
            machines[0].image = target_image;
        }

        Ok(RestorePlan {
            target,
            restorer,
            machines,
            types,
            child_charge,
            gate,
            gate_members: ids,
            image_records: prepared_images.records,
            image_appended: prepared_images.appended,
            image_replacement: prepared_images.replacement,
            image_config_update: prepared_images.config_update,
            loaded,
            installations,
        })
    }

    /// Plan portable VM image records without changing the registry.
    #[allow(clippy::too_many_arguments)]
    fn prepare_image_import(
        &mut self,
        target_key: Option<VmImageKey>,
        reused_source: Option<u32>,
        ceiling: VmConfig,
        image: &crate::snapshot::Image,
        ids: &[VmId],
        env_map: &[TypeEnvId],
        type_map: &[u32],
        installation_base: u32,
        artifacts: &[SharedBytes],
    ) -> Result<PreparedImages, RestoreFail> {
        if reused_source.is_some() != target_key.is_some() {
            return Err(RestoreFail::OtherProgram);
        }
        if reused_source.is_some_and(|source| source as usize >= image.vm_images.len()) {
            return Err(RestoreFail::OtherProgram);
        }
        if image.vm_images.len() == 1 && reused_source == Some(0) {
            let source = &image.vm_images[0];
            let key = target_key.ok_or(RestoreFail::OtherProgram)?;
            let config = clamp_image(&source.limits, ceiling);
            let target = self
                .vm_images
                .get(key.image as usize)
                .ok_or(RestoreFail::OtherProgram)?;
            let reuses_pristine_image = target.live
                && target.generation == key.generation
                && same_image_slots(&target.slots, &source.slots)
                && target.slot_versions == source.slot_versions
                && target.heap.live_count() == 0
                && target.heap.slot_count() == 0
                && target.instances.is_empty()
                && source.objects.is_empty()
                && source.instances.is_empty();
            if reuses_pristine_image {
                let mut keys = try_vec(1)?;
                keys.push(key);
                return Ok(PreparedImages {
                    keys,
                    records: Vec::new(),
                    appended: 0,
                    replacement: None,
                    config_update: Some((key.image, config)),
                });
            }
        }
        let new_count = image
            .vm_images
            .len()
            .saturating_sub(usize::from(reused_source.is_some()));
        let live = self
            .vm_images
            .len()
            .saturating_sub(self.vm_image_free.len());
        if live
            .checked_add(new_count)
            .is_none_or(|total| total > self.vm_image_limit())
        {
            return Err(RestoreFail::LimitExceeded);
        }
        let reused_slots = new_count.min(self.vm_image_free.len());
        let appended = new_count - reused_slots;
        self.vm_images
            .try_reserve_exact(appended)
            .map_err(|_| RestoreFail::LimitExceeded)?;
        let mut keys = try_vec(image.vm_images.len())?;
        let mut free = self.vm_image_free.iter().rev().copied();
        let mut next = self.vm_images.len();
        for ordinal in 0..image.vm_images.len() {
            if Some(ordinal as u32) == reused_source {
                keys.push(target_key.ok_or(RestoreFail::LimitExceeded)?);
                continue;
            }
            let (slot, generation) = match free.next() {
                Some(slot) => (slot, self.vm_images[slot as usize].generation),
                None => {
                    let slot = u32::try_from(next).map_err(|_| RestoreFail::LimitExceeded)?;
                    next = next.checked_add(1).ok_or(RestoreFail::LimitExceeded)?;
                    (slot, 0)
                }
            };
            let key = VmImageKey {
                image: slot,
                generation,
            };
            keys.push(key);
        }
        let mut records = try_vec(new_count)?;
        let mut replacement = None;
        for (ordinal, source) in image.vm_images.iter().enumerate() {
            let key = keys[ordinal];
            let config = clamp_image(&source.limits, ceiling);
            let mut heap = self.empty_image_heap(config);
            let refs = restore_objects(&mut heap, &source.objects, ids, &keys, env_map, type_map)?;
            let mut slots = try_vec(source.slots.len())?;
            for target in &source.slots {
                slots.push(match target {
                    super::ImageSlotTarget::Empty => RuntimeSlotTarget::Empty,
                    super::ImageSlotTarget::Function(func) => RuntimeSlotTarget::Function(*func),
                    super::ImageSlotTarget::Class { class, constructor } => {
                        RuntimeSlotTarget::Class {
                            class: *class,
                            constructor: *constructor,
                        }
                    }
                    super::ImageSlotTarget::Value(value) => {
                        RuntimeSlotTarget::Value(relocate_value(*value, &refs, type_map))
                    }
                    super::ImageSlotTarget::Process { proc, generation } => {
                        RuntimeSlotTarget::Process {
                            proc: ids[*proc as usize],
                            generation: *generation,
                        }
                    }
                });
            }
            let mut instances = try_vec(source.instances.len())?;
            for instance in &source.instances {
                let artifact = artifacts
                    .get(instance.installation as usize)
                    .ok_or(RestoreFail::OtherProgram)?;
                let module =
                    lm_bytecode::decode_with_bundle(artifact.as_slice(), self.loaded.bundle())
                        .map_err(|_| RestoreFail::OtherProgram)?;
                let mut exports = try_vec(module.exports.len())?;
                for export in &module.exports {
                    if export.kind != lm_bytecode::ExportKind::Function {
                        continue;
                    }
                    let function = *instance
                        .funcs
                        .get(export.def as usize)
                        .ok_or(RestoreFail::OtherProgram)?;
                    let mut name = String::new();
                    name.try_reserve_exact(export.name.len())
                        .map_err(|_| RestoreFail::LimitExceeded)?;
                    name.push_str(&export.name);
                    exports.push((name, function));
                }
                let mut funcs = try_vec(instance.funcs.len())?;
                funcs.extend_from_slice(&instance.funcs);
                let mut classes = try_vec(instance.classes.len())?;
                classes.extend_from_slice(&instance.classes);
                let mut instance_slots = try_vec(instance.slots.len())?;
                instance_slots.extend_from_slice(&instance.slots);
                let mut binding_targets = try_vec(module.slots.len())?;
                for slot in &module.slots {
                    let target = match slot.initial {
                        Some(lm_bytecode::SlotTarget::Function(function)) => {
                            RuntimeSlotTarget::Function(
                                *funcs
                                    .get(function as usize)
                                    .ok_or(RestoreFail::OtherProgram)?,
                            )
                        }
                        Some(lm_bytecode::SlotTarget::Class { class, constructor }) => {
                            RuntimeSlotTarget::Class {
                                class: *classes
                                    .get(class as usize)
                                    .ok_or(RestoreFail::OtherProgram)?,
                                constructor: *funcs
                                    .get(constructor as usize)
                                    .ok_or(RestoreFail::OtherProgram)?,
                            }
                        }
                        None => RuntimeSlotTarget::Empty,
                    };
                    binding_targets.push(target);
                }
                instances.push(InstalledInstance {
                    installation: installation_base
                        .checked_add(instance.installation)
                        .ok_or(RestoreFail::LimitExceeded)?,
                    artifact: artifact.clone(),
                    interface: instance.interface.clone().map(SharedBytes::from),
                    semantic_hash: instance.semantic_hash,
                    entry: instance.entry,
                    funcs,
                    classes,
                    slots: instance_slots,
                    binding_targets,
                    exports,
                });
            }
            let record = VmImageRecord {
                generation: key.generation,
                live: true,
                config,
                slots: Arc::new(slots),
                slot_versions: source.slot_versions.clone(),
                heap,
                instances,
            };
            if Some(ordinal as u32) == reused_source {
                replacement = Some((key.image, record));
            } else {
                records.push((key.image, record));
            }
        }
        Ok(PreparedImages {
            keys,
            records,
            appended,
            replacement,
            config_update: None,
        })
    }

    /// Commit one prepared restore.
    ///
    /// The commit marks the world. A restored machine holds values a
    /// container stated, so every later VM boundary of this world
    /// checks the type of the value that crosses it.
    pub(crate) fn commit_restore(&mut self, plan: RestorePlan) -> Option<VmId> {
        let RestorePlan {
            target,
            restorer,
            machines,
            types,
            child_charge,
            gate,
            gate_members,
            image_records,
            image_appended,
            image_replacement,
            image_config_update,
            loaded,
            installations,
        } = plan;
        if let Some(loaded) = loaded {
            self.loaded = loaded;
            self.module = self.loaded.module_store();
            self.dispatch = self.loaded.dispatch_store();
            self.core = self.loaded.core_layout();
            let slot_count = self.module.slots.len();
            for image in &mut self.vm_images {
                if image.live {
                    Arc::make_mut(&mut image.slots).resize(slot_count, RuntimeSlotTarget::Empty);
                }
            }
        }
        self.installations.extend(installations);
        if let Some((slot, record)) = image_replacement {
            self.vm_images[slot as usize] = record;
        }
        if let Some((slot, config)) = image_config_update {
            let heap = self.empty_image_heap(config);
            let record = &mut self.vm_images[slot as usize];
            record.config = config;
            record.heap = heap;
        }
        let reused = image_records.len().saturating_sub(image_appended);
        for (index, (slot, record)) in image_records.into_iter().enumerate() {
            if index < reused {
                let free = self
                    .vm_image_free
                    .pop()
                    .expect("a prepared VM image uses one free entry");
                debug_assert_eq!(free, slot);
                self.vm_images[slot as usize] = record;
            } else {
                debug_assert_eq!(slot as usize, self.vm_images.len());
                self.vm_images.push(record);
            }
        }
        self.envs.commit_import(types);
        self.mark_restored();
        self.set_gate_marker(gate);
        self.machines[restorer as usize].children += child_charge;
        let mut machines = machines.into_iter();
        if let Some(target) = target {
            self.machines[target as usize] = machines
                .next()
                .expect("a prepared selected restore holds its first machine");
        }
        self.machines.extend(machines);
        for vm in gate_members.iter().copied() {
            let machine = &self.machines[vm as usize];
            if machine.owner == Ownership::Scheduler
                && !machine.paused
                && !matches!(machine.vm.state, MachineState::Done | MachineState::Faulted)
            {
                self.activate_scheduler_proc_prepared(vm);
            }
        }
        self.install_gate_group(gate, gate_members);
        target
    }
}

/// Check live state against its effective target limits.
fn check_effective_limits(
    source: &ImageMachine,
    config: VmConfig,
    children: u32,
) -> Result<(), RestoreFail> {
    if source.frames.len() > config.max_frames as usize {
        return Err(RestoreFail::LimitExceeded);
    }
    let stack = source
        .locals
        .len()
        .checked_add(source.operands.len())
        .ok_or(RestoreFail::LimitExceeded)?;
    if stack > config.max_stack_values as usize || children > config.max_children {
        return Err(RestoreFail::LimitExceeded);
    }
    let mailbox_limit = source.mailbox.limit.min(config.mailbox_limit);
    if source.mailbox.queue.len() > mailbox_limit as usize {
        return Err(RestoreFail::LimitExceeded);
    }
    Ok(())
}

/// Restore one heap with full object costs from its first allocation.
fn restore_heap(
    machine: &mut Machine,
    source: &ImageMachine,
    ids: &[VmId],
    image_keys: &[VmImageKey],
    env_map: &[TypeEnvId],
    type_map: &[u32],
) -> Result<Vec<ObjRef>, RestoreFail> {
    restore_objects(
        &mut machine.vm.heap,
        &source.objects,
        ids,
        image_keys,
        env_map,
        type_map,
    )
}

/// Restore one canonical object table into one empty heap.
fn restore_objects(
    heap: &mut Heap,
    objects: &[super::ImageObject],
    ids: &[VmId],
    image_keys: &[VmImageKey],
    env_map: &[TypeEnvId],
    type_map: &[u32],
) -> Result<Vec<ObjRef>, RestoreFail> {
    let mut bytes = 0usize;
    for entry in objects {
        bytes = bytes
            .checked_add(entry.object.cost())
            .ok_or(RestoreFail::LimitExceeded)?;
    }
    if heap.would_exceed_batch(bytes, objects.len()) {
        return Err(RestoreFail::LimitExceeded);
    }

    let mut refs = try_vec(objects.len())?;
    for ordinal in 0..objects.len() {
        refs.push(ObjRef {
            slot: u32::try_from(ordinal).map_err(|_| RestoreFail::LimitExceeded)?,
            generation: 0,
        });
    }
    for (ordinal, entry) in objects.iter().enumerate() {
        let mut object = entry
            .object
            .try_clone_remapped(|child| refs[child.slot as usize])
            .map_err(|_| RestoreFail::LimitExceeded)?;
        relocate_metadata(&mut object, ids, image_keys, env_map, type_map);
        let reference = heap
            .try_alloc(object)
            .map_err(|_| RestoreFail::LimitExceeded)?;
        if reference != refs[ordinal] {
            return Err(RestoreFail::LimitExceeded);
        }
        if entry.frozen {
            heap.set_frozen(reference);
        }
    }
    Ok(refs)
}

fn relocate_value(value: Value, refs: &[ObjRef], type_map: &[u32]) -> Value {
    match value {
        Value::Obj(reference) => Value::Obj(refs[reference.slot as usize]),
        Value::EmptyCase { ty, arm } => Value::EmptyCase {
            ty: type_map[ty as usize],
            arm,
        },
        other => other,
    }
}

/// Install the non-heap state of one detached machine.
#[allow(clippy::too_many_arguments)]
fn restore_state(
    machine: &mut Machine,
    source: &ImageMachine,
    ids: &[VmId],
    generations: &[u32],
    env_map: &[TypeEnvId],
    type_map: &[u32],
    refs: &[ObjRef],
    restorer: VmId,
    gate: u32,
    children: u32,
) -> Result<(), RestoreFail> {
    let object_value = |value: Value| match value {
        Value::Obj(reference) => Value::Obj(refs[reference.slot as usize]),
        Value::EmptyCase { ty, arm } => Value::EmptyCase {
            ty: type_map[ty as usize],
            arm,
        },
        other => other,
    };

    let mut callbacks = try_vec(source.callbacks.len())?;
    for callback in &source.callbacks {
        let mut captures = try_vec(callback.captures.len())?;
        captures.extend(callback.captures.iter().copied().map(object_value));
        callbacks.push(CallbackSlot {
            generation: 0,
            descriptor: Some(CallbackDescriptor {
                func: callback.func,
                captures,
                env: env_map[callback.env as usize],
                owner_depth: callback.owner_depth,
            }),
        });
    }
    let mut frames = try_vec(source.frames.len())?;
    for frame in &source.frames {
        let closure = match frame.closure.map(object_value) {
            Some(value) => Some(FrameCapture::from_value(value).ok_or(RestoreFail::OtherProgram)?),
            None => None,
        };
        frames.push(Frame {
            func: frame.func,
            block: frame.block,
            ip: frame.ip,
            base_local: frame.base_local,
            base_operand: frame.base_operand,
            closure,
            env: env_map[frame.env as usize],
        });
    }
    let mut locals = try_vec(source.locals.len())?;
    locals.extend(source.locals.iter().copied().map(object_value));
    let mut operands = try_vec(source.operands.len())?;
    operands.extend(source.operands.iter().copied().map(object_value));
    let mut literals = try_vec(source.literals.len())?;
    literals.extend(
        source
            .literals
            .iter()
            .map(|slot| slot.map(|ordinal| refs[ordinal as usize])),
    );
    let pending = match &source.pending {
        Some(record) => {
            let mut args = try_vec(record.args.len())?;
            args.extend(record.args.iter().copied().map(object_value));
            Some(Pending {
                op: record.op,
                args,
                ordinal: record.ordinal,
            })
        }
        None => None,
    };
    let terminal = match &source.terminal {
        None => None,
        Some(ImageTerminal::Done(value)) => Some(Terminal::Done(object_value(*value))),
        Some(ImageTerminal::Fault(record)) => {
            let mut message = String::new();
            message
                .try_reserve_exact(record.message.len())
                .map_err(|_| RestoreFail::LimitExceeded)?;
            message.push_str(&record.message);
            let mut trace = try_vec(record.trace.len())?;
            trace.extend_from_slice(&record.trace);
            Some(Terminal::Fault(FaultRec {
                code: record.code,
                message,
                op: record.op,
                trace,
            }))
        }
    };
    let mailbox_limit = source.mailbox.limit.min(machine.config.mailbox_limit);
    let mut queue = std::collections::VecDeque::new();
    queue
        .try_reserve(source.mailbox.queue.len())
        .map_err(|_| RestoreFail::LimitExceeded)?;
    queue.extend(source.mailbox.queue.iter().copied().map(object_value));

    machine.vm.parent = source
        .parent
        .map(|ordinal| ids[ordinal as usize])
        .or(Some(restorer));
    machine.vm.state = match source.state {
        ImageState::Empty => MachineState::Empty,
        ImageState::Ready => MachineState::Ready,
        ImageState::Asked => MachineState::Asked,
        ImageState::Blocked => MachineState::Blocked,
        ImageState::Done => MachineState::Done,
        ImageState::Faulted => MachineState::Faulted,
    };
    machine.owner = if source.scheduler_owned {
        Ownership::Scheduler
    } else {
        Ownership::Holder
    };
    machine.paused = source.paused;
    if source.is_proc {
        let group = machine
            .table
            .bundle()
            .group_by_name("Proc")
            .expect("the standard bundle declares the Proc group");
        machine.table.set_group(group, Some(Action::Pass));
    }
    machine.children = children;
    machine.is_proc = source.is_proc;
    machine.body_func = source.body_func;
    machine.witness = env_map[source.witness as usize];
    machine.gate = gate;
    machine.vm.fuel = source.fuel.min(machine.config.fuel);
    machine.vm.next_ordinal = source.next_ordinal;
    machine.vm.next_wait = source.next_wait;
    machine.vm.waits = source
        .waits
        .iter()
        .map(|entry| {
            let source = match entry.source {
                ImageWaitSource::Receive => WaitSource::Receive,
                ImageWaitSource::Drive { target } => WaitSource::Drive {
                    target: ids[target as usize],
                },
                ImageWaitSource::Choice { first, second } => WaitSource::Choice { first, second },
            };
            (
                entry.token,
                WaitEntry {
                    source,
                    linked: entry.linked,
                },
            )
        })
        .collect();
    machine.vm.frames = frames;
    machine.callbacks = callbacks;
    machine.vm.locals = locals;
    machine.vm.operands = operands;
    machine.vm.literals = literals;
    machine.start_body = source.start_body.map(|ordinal| refs[ordinal as usize]);
    machine.vm.pending = pending;
    machine.vm.nested = source.nested.map(|ordinal| ids[ordinal as usize]);
    machine.vm.routed = source.routed.map(|route| RoutedRequest {
        target: ids[route.target as usize],
        cursor: match route.cursor {
            ImagePolicyCursor::Table(table) => PolicyCursor::Table(ids[table as usize]),
            ImagePolicyCursor::Binding => PolicyCursor::Table(restorer),
            ImagePolicyCursor::Root => PolicyCursor::Root,
        },
    });
    machine.vm.terminal = terminal;
    machine.vm.mailbox = Mailbox {
        limit: mailbox_limit,
        queue,
        closed: source.mailbox.closed,
        frozen: false,
        accepted: source.mailbox.accepted,
        delivered: source.mailbox.delivered,
    };
    machine.vm.block = source.block.map(|block| match block {
        ImageBlock::Receive => Block::Receive,
        ImageBlock::Send { target } => Block::Send {
            target: ids[target as usize],
            generation: generations[target as usize],
        },
        ImageBlock::Done { target } => Block::Done {
            target: ids[target as usize],
            generation: generations[target as usize],
        },
        ImageBlock::Wait { token } => Block::Wait { token },
        ImageBlock::Snapshot {
            target,
            remaining,
            retry,
        } => Block::Snapshot {
            target: ids[target as usize],
            generation: generations[target as usize],
            remaining,
            retry,
        },
    });
    if matches!(machine.vm.state, MachineState::Done | MachineState::Faulted) {
        machine.compact_terminal_proc();
    }
    Ok(())
}

/// Create a vector with a fallible exact reservation.
fn try_vec<T>(count: usize) -> Result<Vec<T>, RestoreFail> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| RestoreFail::LimitExceeded)?;
    Ok(values)
}

/// Test whether two slot tables hold the same static targets.
fn same_image_slots(left: &[RuntimeSlotTarget], right: &[super::ImageSlotTarget]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| match (left, right) {
                (RuntimeSlotTarget::Empty, super::ImageSlotTarget::Empty) => true,
                (RuntimeSlotTarget::Function(left), super::ImageSlotTarget::Function(right)) => {
                    left == right
                }
                (
                    RuntimeSlotTarget::Class {
                        class: left_class,
                        constructor: left_constructor,
                    },
                    super::ImageSlotTarget::Class {
                        class: right_class,
                        constructor: right_constructor,
                    },
                ) => left_class == right_class && left_constructor == right_constructor,
                _ => false,
            })
}

/// Clamp captured limits by the target ceiling.
fn clamp(source: &ImageMachine, ceiling: VmConfig) -> VmConfig {
    let graph = lm_graph::GraphLimits {
        max_objects: source.limits.max_objects.min(ceiling.graph.max_objects),
        max_edges: source.limits.max_edges.min(ceiling.graph.max_edges),
        max_bytes: source.limits.max_graph_bytes.min(ceiling.graph.max_bytes),
        max_work: source.limits.max_work.min(ceiling.graph.max_work),
    };
    let source_heap = usize::try_from(source.limits.heap_bytes).unwrap_or(usize::MAX);
    VmConfig {
        fuel: source.limits.fuel.min(ceiling.fuel),
        max_frames: source.limits.max_frames.min(ceiling.max_frames),
        max_stack_values: source.limits.max_stack_values.min(ceiling.max_stack_values),
        heap_bytes: source_heap.min(ceiling.heap_bytes),
        graph,
        max_children: source.limits.max_children.min(ceiling.max_children),
        max_resources: source.limits.max_resources.min(ceiling.max_resources),
        mailbox_limit: source.limits.mailbox_limit.min(ceiling.mailbox_limit),
        snapshot_bytes: ceiling.snapshot_bytes,
        max_closed_types: ceiling.max_closed_types,
        max_type_envs: ceiling.max_type_envs,
    }
}

/// Clamp one portable VM image ceiling by its receiving world.
fn clamp_image(source: &crate::snapshot::ImageLimits, ceiling: VmConfig) -> VmConfig {
    let graph = lm_graph::GraphLimits {
        max_objects: source.max_objects.min(ceiling.graph.max_objects),
        max_edges: source.max_edges.min(ceiling.graph.max_edges),
        max_bytes: source.max_graph_bytes.min(ceiling.graph.max_bytes),
        max_work: source.max_work.min(ceiling.graph.max_work),
    };
    let heap_bytes = usize::try_from(source.heap_bytes).unwrap_or(usize::MAX);
    VmConfig {
        fuel: source.fuel.min(ceiling.fuel),
        max_frames: source.max_frames.min(ceiling.max_frames),
        max_stack_values: source.max_stack_values.min(ceiling.max_stack_values),
        heap_bytes: heap_bytes.min(ceiling.heap_bytes),
        graph,
        max_children: source.max_children.min(ceiling.max_children),
        max_resources: source.max_resources.min(ceiling.max_resources),
        mailbox_limit: source.mailbox_limit.min(ceiling.mailbox_limit),
        snapshot_bytes: ceiling.snapshot_bytes,
        max_closed_types: ceiling.max_closed_types,
        max_type_envs: ceiling.max_type_envs,
    }
}

/// Relocate the world-local metadata of one restored object.
fn relocate_metadata(
    object: &mut Object,
    ids: &[VmId],
    image_keys: &[VmImageKey],
    env_map: &[TypeEnvId],
    type_map: &[u32],
) {
    let remap = |value: &mut Value| {
        if let Value::EmptyCase { ty, .. } = value {
            *ty = type_map[*ty as usize];
        }
    };
    match object {
        Object::Instance { fields, .. }
        | Object::List { items: fields, .. }
        | Object::Tuple { items: fields } => fields.iter_mut().for_each(remap),
        Object::Map { entries, .. } => {
            for entry in entries {
                if !entry.is_live() {
                    continue;
                }
                remap(&mut entry.key);
                remap(&mut entry.value);
            }
        }
        Object::Closure { captures, .. } => captures.iter_mut().for_each(remap),
        Object::DynValue { value, ty } => {
            remap(value);
            *ty = type_map[*ty as usize];
        }
        Object::NativeSlotChange { target, .. } => remap(target),
        _ => {}
    }
    match object {
        Object::Instance { env, .. } | Object::Closure { env, .. } => {
            *env = Witness(env_map[env.env().0 as usize]);
        }
        Object::NativeVm { image, generation } => {
            let key = image_keys[*image as usize];
            *image = key.image;
            *generation = key.generation;
        }
        Object::NativeCodeHandle {
            image, generation, ..
        }
        | Object::NativeSlotChange {
            image, generation, ..
        } => {
            let key = image_keys[*image as usize];
            *image = key.image;
            *generation = key.generation;
        }
        Object::NativeRun { vm }
        | Object::NativeTable { vm }
        | Object::NativeRequest { vm, .. }
        | Object::NativeCall { vm, .. } => {
            *vm = ids[*vm as usize];
        }
        Object::NativeHandle { proc, .. } => {
            *proc = ids[*proc as usize];
        }
        Object::NativeResourceHandle { surface, .. } => {
            *surface = ids[*surface as usize];
        }
        Object::NativeWait { owner, .. } => {
            *owner = ids[*owner as usize];
        }
        _ => {}
    }
}
