//! The consistent cut and the canonical writer (speciication 17.3).
//!
//! One barrier deines the moment of one snapshot. It stops the root
//! and every reachable machine at an instruction boundary, closes the
//! set over the machine references their stopped state holds, freezes
//! mailbox acceptance at one cut marker, records the states,
//! preflights the host attachments, encodes, and resumes the original
//! world after success and after failure.
//!
//! The algorithm lives here, in `lm-vm`, because the guest operation
//! `Vm.SnapshotHeld` runs inside the driver loop and `lm-vm` depends
//! on no scheduler. `lm_proc::Barrier` is the scheduler-facing entry
//! and calls this one, so the world has exactly one cut algorithm.

use super::{
    codec, Image, ImageBlock, ImageCallback, ImageFrame, ImageInstance, ImageLimits, ImageMachine,
    ImageMailbox, ImageObject, ImagePending, ImagePolicyCursor, ImageRoutedRequest,
    ImageSlotTarget, ImageState, ImageTerminal, ImageVm, ImageWaitEntry, ImageWaitSource,
    SnapshotFail, SnapshotImage,
};
use crate::machine::{
    Block, FrameCapture, MachineState, PolicyCursor, Terminal, VmId, VmImageKey, WaitSource,
};
use crate::world::World;
use crate::FaultCode;
use lm_bytecode::closed::{ClosedType, TypeEnv};
use lm_heap::Object;
use lm_value::{CallbackRef, ObjRef, TypeEnvId, Value, Witness};
use std::collections::{HashMap, VecDeque};

/// Convert one runtime resource ceiling to its portable form.
fn image_limits(config: crate::VmConfig) -> ImageLimits {
    ImageLimits {
        fuel: config.fuel,
        max_frames: config.max_frames,
        max_stack_values: config.max_stack_values,
        heap_bytes: config.heap_bytes as u64,
        max_objects: config.graph.max_objects,
        max_edges: config.graph.max_edges,
        max_graph_bytes: config.graph.max_bytes,
        max_work: config.graph.max_work,
        max_children: config.max_children,
        max_resources: config.max_resources,
        mailbox_limit: config.mailbox_limit,
    }
}

/// What one finished cut recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutReport {
    /// Every machine of the closed set, in ascending identifier order.
    pub set: Vec<VmId>,
    /// The canonical machine order: ordinal `i` names `order[i]`.
    ///
    /// A run cut starts with its distinguished run. A full VM cut
    /// starts with its runs in ascending creation order. Both forms
    /// then use breadth-first reference order.
    pub order: Vec<VmId>,
    /// The one mailbox acceptance cut of this barrier.
    pub cut: u64,
    /// The number of objects the canonical traversal ordered, summed
    /// over the set.
    pub objects: usize,
    /// True when the cut resumed every machine it stopped.
    pub resumed: bool,
}

/// Why one cut did not open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CutError {
    /// Another barrier already holds one machine of the set.
    Overlaps(VmId),
    /// One machine holds a live host attachment (specification 17.4).
    ResourceActive { path: Vec<u32>, kind: String },
    /// A traversal of one machine reached a graph limit.
    Limit(VmId, FaultCode),
    /// The world holds a machine no cut can copy.
    NotCapturable(VmId, String),
    /// The container passed the configured snapshot byte limit.
    TooLarge,
}

impl World {
    /// Run one consistent cut from `root`.
    ///
    /// The call encodes nothing. `capture_snapshot` adds the
    /// capturability rules and the encoding step.
    pub fn run_cut(&mut self, barrier: u32, root: VmId) -> Result<CutReport, CutError> {
        self.run_cut_many(barrier, &[root])
    }

    /// Run one consistent cut from an ordered set of roots.
    fn run_cut_many(&mut self, barrier: u32, roots: &[VmId]) -> Result<CutReport, CutError> {
        let mut set: Vec<VmId> = Vec::new();
        let mut queue: Vec<VmId> = roots.iter().rev().copied().collect();
        // Steps 1 and 2: stop the reachable machines and close the set
        // over the machine references the stopped state holds.
        while let Some(vm) = queue.pop() {
            if set.contains(&vm) {
                continue;
            }
            if let Some(other) = self.barrier_of(vm) {
                if other != barrier {
                    self.release_cut(&set, false);
                    return Err(CutError::Overlaps(vm));
                }
            }
            self.set_barrier(vm, Some(barrier));
            set.push(vm);
            if !self.is_live_machine(vm) {
                continue;
            }
            match self.machine_references(vm) {
                Ok(found) => queue.extend(found),
                Err(code) => {
                    self.release_cut(&set, false);
                    return Err(CutError::Limit(vm, code));
                }
            }
        }
        set.sort_unstable();
        // Step 3: freeze mailbox acceptance for the whole set at one
        // cut marker.
        let cut = self.next_cut();
        for vm in &set {
            self.freeze_mailbox(*vm, true);
        }
        // Steps 4 and 5: record the states and preflight the host
        // attachments.
        let mut objects = 0;
        for vm in set.clone() {
            if !self.is_live_machine(vm) {
                continue;
            }
            if let Some(kind) = self.live_attachment_kind(vm) {
                let path = self.machine_path_many(roots, vm);
                self.release_cut(&set, true);
                return Err(CutError::ResourceActive { path, kind });
            }
            match self.snapshot_preflight(vm) {
                Ok(count) => objects += count,
                Err(code) => {
                    self.release_cut(&set, true);
                    return Err(CutError::Limit(vm, code));
                }
            }
        }
        let order = self.machine_order_many(roots);
        debug_assert_eq!(
            order.len(),
            set.len(),
            "the cut order covers the closed set"
        );
        Ok(CutReport {
            set,
            order,
            cut,
            objects,
            resumed: false,
        })
    }

    /// Resume every machine one cut stopped.
    pub fn release_cut(&mut self, set: &[VmId], thaw: bool) {
        for vm in set {
            if thaw {
                self.freeze_mailbox(*vm, false);
            }
            self.set_barrier(*vm, None);
        }
    }

    /// Capture one machine world as a canonical image.
    ///
    /// The call runs the cut, encodes, and resumes the original world.
    /// A failure leaves the original world unchanged, and no machine
    /// stays stopped.
    pub fn capture_snapshot(
        &mut self,
        barrier: u32,
        root: VmId,
        self_root: bool,
    ) -> Result<SnapshotImage, SnapshotFail> {
        self.capture_selected(barrier, &[root], Some(root), None, self_root, root)
    }

    /// Capture one complete persistent VM image.
    pub(crate) fn capture_vm_snapshot(
        &mut self,
        barrier: u32,
        holder: VmId,
        target: VmImageKey,
    ) -> Result<SnapshotImage, SnapshotFail> {
        let live = self
            .vm_images
            .get(target.image as usize)
            .is_some_and(|image| image.live && image.generation == target.generation);
        if !live {
            return Err(SnapshotFail::Fault(
                FaultCode::InvalidVmState,
                "the VM image handle is stale".to_string(),
            ));
        }
        let roots = self.vm_snapshot_roots(target).map_err(|code| {
            SnapshotFail::Fault(code, "a VM value-slot graph passed a limit".to_string())
        })?;
        self.capture_selected(barrier, &roots, None, Some(target), false, holder)
    }

    /// Find the machine roots of one complete VM image capture.
    pub(crate) fn vm_snapshot_roots(&mut self, target: VmImageKey) -> Result<Vec<VmId>, FaultCode> {
        let mut roots: Vec<VmId> = self
            .machines
            .iter()
            .enumerate()
            .filter_map(|(vm, machine)| {
                (machine.image() == Some(target) && machine.is_live()).then_some(vm as VmId)
            })
            .collect();
        let image = &mut self.vm_images[target.image as usize];
        for slot in image.slots.iter() {
            if let crate::machine::ImageSlotTarget::Process { proc, generation } = slot {
                let live = self.machines.get(*proc as usize).is_some_and(|machine| {
                    machine.generation() == *generation && machine.is_live()
                });
                if live && !roots.contains(proc) {
                    roots.push(*proc);
                }
            }
        }
        let value_roots: Vec<ObjRef> = image
            .slots
            .iter()
            .filter_map(|slot| match slot {
                crate::machine::ImageSlotTarget::Value(Value::Obj(reference)) => Some(*reference),
                _ => None,
            })
            .collect();
        let order =
            lm_graph::snapshot_ordinals(&mut image.heap, &value_roots, &image.config.graph)?;
        for reference in order {
            if let Object::NativeHandle { proc, generation } = image.heap.get(reference) {
                let live = self.machines.get(*proc as usize).is_some_and(|machine| {
                    machine.generation() == *generation && machine.is_live()
                });
                if live && !roots.contains(proc) {
                    roots.push(*proc);
                }
            }
        }
        roots.sort_unstable();
        Ok(roots)
    }

    /// Capture one selected run or one selected persistent VM.
    #[allow(clippy::too_many_arguments)]
    fn capture_selected(
        &mut self,
        barrier: u32,
        roots: &[VmId],
        distinguished: Option<VmId>,
        full_vm: Option<VmImageKey>,
        self_root: bool,
        limit_holder: VmId,
    ) -> Result<SnapshotImage, SnapshotFail> {
        let report = match self.run_cut_many(barrier, roots) {
            Ok(report) => report,
            Err(CutError::ResourceActive { path, kind }) => {
                return Err(SnapshotFail::ResourceActive { path, kind })
            }
            Err(CutError::TooLarge) => return Err(SnapshotFail::LimitExceeded),
            Err(CutError::Overlaps(vm)) => {
                return Err(SnapshotFail::Fault(
                    FaultCode::InvalidVmState,
                    format!("another snapshot holds machine {vm}"),
                ))
            }
            Err(CutError::Limit(vm, code)) => {
                return Err(SnapshotFail::Fault(
                    code,
                    format!("the capture of machine {vm} passed a graph limit"),
                ))
            }
            Err(CutError::NotCapturable(vm, message)) => {
                return Err(SnapshotFail::Fault(
                    FaultCode::InvalidVmState,
                    format!("machine {vm} {message}"),
                ))
            }
        };
        // The encoder needs more than the cut does: a captured machine
        // must sit at a boundary the image can name.
        for vm in report.set.iter().copied() {
            if let Err(message) = self.capturable(vm, distinguished, self_root) {
                self.release_cut(&report.set, true);
                return Err(SnapshotFail::Fault(
                    FaultCode::InvalidVmState,
                    format!("machine {vm} {message}"),
                ));
            }
        }
        // Step 6: encode, now that every preflight succeeded.
        let built = self.build_image(&report, distinguished, full_vm, self_root);
        // Step 7: resume the original world, whatever the encoding
        // answered.
        self.release_cut(&report.set, true);
        let image = built?;
        let limit = self.snapshot_byte_limit(limit_holder);
        // The cut copies a stopped verified world, so the admission
        // invariant holds by construction (specification section 7.2).
        // The constructor stays inside the snapshot module, so no host
        // code can promote an arbitrary image through this path.
        let identity = self.admission_identity()?;
        codec::from_trusted_capture(image, identity, self.loaded_code(), limit)
    }

    /// The admission identity of the program this world runs.
    fn admission_identity(&self) -> Result<super::AdmissionIdentity, SnapshotFail> {
        let identity = self.identity().map_err(|code| {
            SnapshotFail::Fault(code, "the program has no verified identity".to_string())
        })?;
        let base = self.base_identity().map_err(|code| {
            SnapshotFail::Fault(
                code,
                "the base program has no verified identity".to_string(),
            )
        })?;
        Ok(super::AdmissionIdentity {
            base_semantic: base.semantic_hash,
            base_verification: self.base_verification_hash(),
            module_semantic: identity.semantic_hash,
            verification: self.verification_hash(),
            format: super::FORMAT_VERSION,
            abi_version: lm_abi::ABI_VERSION,
            compiler_abi: lm_bytecode::identity::COMPILER_ABI_VERSION,
            verifier_version: lm_verify::VERIFIER_VERSION,
            bundle_digest: self.loaded.bundle().digest(),
        })
    }

    /// The byte limit of one snapshot the machine `vm` asks for.
    fn snapshot_byte_limit(&self, vm: VmId) -> usize {
        self.machines[vm as usize].config.snapshot_bytes
    }

    /// Return one deterministic breadth-first order from many roots.
    fn machine_order_many(&mut self, roots: &[VmId]) -> Vec<VmId> {
        let mut order: Vec<VmId> = Vec::new();
        let mut queue: VecDeque<VmId> = VecDeque::new();
        queue.extend(roots.iter().copied());
        while let Some(vm) = queue.pop_front() {
            if order.contains(&vm) {
                continue;
            }
            order.push(vm);
            if !self.is_live_machine(vm) {
                continue;
            }
            let found = self
                .machine_references(vm)
                .expect("the cut already ordered every captured heap");
            for target in found {
                if !order.contains(&target) {
                    queue.push_back(target);
                }
            }
        }
        order
    }

    /// The bounded machine path from the root to one machine, in
    /// machine ordinals.
    ///
    /// The walk is the canonical breadth-first order, so the path
    /// names machines the way the image would name them. It never
    /// reports a scheduler identifier.
    /// Return one machine path from an ordered set of cut roots.
    fn machine_path_many(&mut self, roots: &[VmId], target: VmId) -> Vec<u32> {
        let mut parent: std::collections::BTreeMap<VmId, VmId> = std::collections::BTreeMap::new();
        let mut order: Vec<VmId> = roots.to_vec();
        let mut queue: VecDeque<VmId> = VecDeque::new();
        queue.extend(roots.iter().copied());
        while let Some(vm) = queue.pop_front() {
            if !self.is_live_machine(vm) {
                continue;
            }
            let Ok(found) = self.machine_references(vm) else {
                continue;
            };
            for next in found {
                if !order.contains(&next) {
                    order.push(next);
                    parent.insert(next, vm);
                    queue.push_back(next);
                }
            }
        }
        let ordinal = |vm: VmId| {
            order
                .iter()
                .position(|machine| *machine == vm)
                .expect("the closed cut contains every path machine") as u32
        };
        let Some(_) = order.iter().position(|machine| *machine == target) else {
            return Vec::new();
        };
        let mut path = vec![ordinal(target)];
        let mut cur = target;
        while let Some(up) = parent.get(&cur) {
            path.push(ordinal(*up));
            cur = *up;
        }
        path.reverse();
        path
    }

    /// Check that one machine is in a state the encoder can copy.
    ///
    /// Specification 17.4 names two conditions that block a copy, and
    /// both are ordinary typed errors. A pending host operation is one
    /// of them, and `run_cut` reports it as `ResourceActive`.
    ///
    /// A stored activation stack blocks no copy. The machines of a
    /// stored stack execute nothing, and each nested control edge
    /// stays in the machine record, so the driver loop rebuilds the
    /// chain after a restore.
    ///
    /// A machine that a live stack holds is still mid flight. Its
    /// activation state lives outside its record, so the copy waits
    /// for the boundary. The root of a receiverless self snapshot is
    /// the one exception (specification 17.6).
    fn capturable(
        &self,
        vm: VmId,
        distinguished: Option<VmId>,
        self_root: bool,
    ) -> Result<(), String> {
        let self_snapshot_root = self_root && distinguished == Some(vm);
        if self_snapshot_root {
            return Ok(());
        }
        if self.state_of(vm) == MachineState::Running {
            return Err("is running, so it is not at a boundary".to_string());
        }
        if self.machines[vm as usize].active as usize > self.suspended_refs(vm) {
            return Err("is in use by a driver".to_string());
        }
        Ok(())
    }

    /// Build one canonical image from the stopped world.
    fn build_image(
        &mut self,
        report: &CutReport,
        distinguished: Option<VmId>,
        full_vm: Option<VmImageKey>,
        self_root: bool,
    ) -> Result<Image, SnapshotFail> {
        let ordinal_of = |vm: VmId| -> Option<u32> {
            report.order.iter().position(|m| *m == vm).map(|i| i as u32)
        };
        let mut image_order: Vec<VmImageKey> = Vec::new();
        if let Some(key) = full_vm {
            self.append_image_key(key, &mut image_order)?;
        }
        for vm in report.order.iter().copied() {
            if let Some(key) = self.machines[vm as usize].image {
                self.append_image_key(key, &mut image_order)?;
            }
            let roots = self.machines[vm as usize].snapshot_roots();
            let limits = self.machines[vm as usize].config.graph;
            let objects = {
                let machine = &mut self.machines[vm as usize];
                lm_graph::snapshot_ordinals(&mut machine.vm.heap, &roots, &limits).map_err(
                    |code| {
                        SnapshotFail::Fault(
                            code,
                            "the VM image walk passed a graph limit".to_string(),
                        )
                    },
                )?
            };
            for reference in objects {
                match self.machines[vm as usize].vm.heap.get(reference) {
                    Object::NativeVm { image, generation }
                    | Object::NativeCodeHandle {
                        image, generation, ..
                    }
                    | Object::NativeSlotChange {
                        image, generation, ..
                    } => {
                        self.append_image_key(
                            VmImageKey {
                                image: *image,
                                generation: *generation,
                            },
                            &mut image_order,
                        )?;
                    }
                    _ => {}
                }
            }
        }
        let image_ordinal = |key: VmImageKey| -> Option<u32> {
            image_order
                .iter()
                .position(|candidate| *candidate == key)
                .map(|index| index as u32)
        };
        let mut vm_images: Vec<ImageVm> = Vec::with_capacity(image_order.len());
        for key in &image_order {
            vm_images.push(self.build_vm_image(*key, &ordinal_of)?);
        }
        let mut funcs: Vec<u32> = Vec::new();
        let mut classes: Vec<u32> = Vec::new();
        // A binding keeps its original target after its slot moves.
        // Admission needs that immutable target in the code manifest.
        for key in &image_order {
            let record = &self.vm_images[key.image as usize];
            for instance in &record.instances {
                for target in &instance.binding_targets {
                    match target {
                        crate::machine::ImageSlotTarget::Function(func)
                            if !funcs.contains(func) =>
                        {
                            funcs.push(*func);
                        }
                        crate::machine::ImageSlotTarget::Class { class, constructor } => {
                            if !classes.contains(class) {
                                classes.push(*class);
                            }
                            if !funcs.contains(constructor) {
                                funcs.push(*constructor);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        for image in &vm_images {
            for target in &image.slots {
                match target {
                    ImageSlotTarget::Function(func) if !funcs.contains(func) => funcs.push(*func),
                    ImageSlotTarget::Class { class, constructor } => {
                        if !classes.contains(class) {
                            classes.push(*class);
                        }
                        if !funcs.contains(constructor) {
                            funcs.push(*constructor);
                        }
                    }
                    _ => {}
                }
            }
            for entry in &image.objects {
                match &entry.object {
                    Object::Instance { class, .. } if !classes.contains(class) => {
                        classes.push(*class);
                    }
                    Object::Closure { func, .. } if !funcs.contains(func) => funcs.push(*func),
                    Object::NativeFault { trace, .. } => {
                        for site in trace {
                            if !funcs.contains(&site.function) {
                                funcs.push(site.function);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut machines: Vec<ImageMachine> = Vec::new();
        for vm in report.order.iter().copied() {
            let machine = self.build_machine(
                vm,
                distinguished == Some(vm),
                self_root,
                &ordinal_of,
                &image_ordinal,
            )?;
            for entry in &machine.objects {
                match &entry.object {
                    Object::Instance { class, .. } => {
                        if !classes.contains(class) {
                            classes.push(*class);
                        }
                    }
                    Object::Closure { func, .. } => {
                        if !funcs.contains(func) {
                            funcs.push(*func);
                        }
                    }
                    Object::NativeFault { trace, .. } => {
                        for site in trace {
                            if !funcs.contains(&site.function) {
                                funcs.push(site.function);
                            }
                        }
                    }
                    _ => {}
                }
            }
            for frame in &machine.frames {
                if !funcs.contains(&frame.func) {
                    funcs.push(frame.func);
                }
            }
            for callback in &machine.callbacks {
                if !funcs.contains(&callback.func) {
                    funcs.push(callback.func);
                }
            }
            // The machine witness names its body function, and a
            // terminal machine keeps neither a frame nor a body
            // closure, so the manifest must carry it too.
            if let Some(func) = machine.body_func {
                if !funcs.contains(&func) {
                    funcs.push(func);
                }
            }
            if let Some(ImageTerminal::Fault(record)) = &machine.terminal {
                for site in &record.trace {
                    if !funcs.contains(&site.function) {
                        funcs.push(site.function);
                    }
                }
            }
            machines.push(machine);
        }
        // Every class the closed type table names joins the manifest,
        // so admission resolves it and proves its definition hash.
        let (types, envs) = self.build_type_tables(&mut vm_images, &mut machines);
        for node in &types {
            if let ClosedType::Class(class) | ClosedType::Inst(class, _) = node {
                if !classes.contains(class) {
                    classes.push(*class);
                }
            }
        }
        funcs.sort_unstable();
        classes.sort_unstable();
        // Keep a separate loaded-code handle while the result type closes.
        // This avoids cloning every class hash for each snapshot.
        let loaded = self.loaded_code();
        let identity = loaded.identity().map_err(|code| {
            SnapshotFail::Fault(code, "the program has no verified identity".to_string())
        })?;
        let funcs: Vec<(u32, [u8; 32])> = funcs
            .into_iter()
            .map(|slot| (slot, identity.func_hashes[slot as usize]))
            .collect();
        let classes: Vec<(u32, [u8; 32])> = classes
            .into_iter()
            .map(|slot| (slot, identity.class_hashes[slot as usize]))
            .collect();
        let semantic = identity.semantic_hash;
        // The header names the selected run result type. A full VM
        // snapshot has no selected run and records zeros.
        let result_type = match distinguished {
            Some(machine) => self.machine_result_digest(machine, &identity.class_hashes)?,
            None => [0u8; 32],
        };
        let mut installations = Vec::new();
        installations
            .try_reserve_exact(self.installations.len())
            .map_err(|_| SnapshotFail::LimitExceeded)?;
        for artifact in &self.installations {
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(artifact.len())
                .map_err(|_| SnapshotFail::LimitExceeded)?;
            bytes.extend_from_slice(artifact.as_slice());
            installations.push(bytes);
        }
        Ok(Image {
            format: super::FORMAT_VERSION,
            abi_version: lm_abi::ABI_VERSION,
            compiler_abi: lm_bytecode::identity::COMPILER_ABI_VERSION,
            verifier_version: lm_verify::VERIFIER_VERSION,
            module_semantic: semantic,
            distinguished: distinguished.and_then(ordinal_of),
            full_vm: full_vm.and_then(image_ordinal),
            result_type,
            funcs,
            classes,
            installations,
            types,
            envs,
            vm_images,
            machines,
        })
    }

    /// Add one live VM image to a canonical key order.
    fn append_image_key(
        &self,
        key: VmImageKey,
        order: &mut Vec<VmImageKey>,
    ) -> Result<(), SnapshotFail> {
        let live = self
            .vm_images
            .get(key.image as usize)
            .is_some_and(|record| record.live && record.generation == key.generation);
        if !live {
            return Err(SnapshotFail::Fault(
                FaultCode::BoundaryViolation,
                "a value names a stale VM image".to_string(),
            ));
        }
        if !order.contains(&key) {
            order.push(key);
        }
        Ok(())
    }

    /// Build one portable image record and its frozen value heap.
    fn build_vm_image(
        &mut self,
        key: VmImageKey,
        ordinal_of: &impl Fn(VmId) -> Option<u32>,
    ) -> Result<ImageVm, SnapshotFail> {
        let record = &self.vm_images[key.image as usize];
        if record.slots.is_empty()
            && record.slot_versions.is_empty()
            && record.heap.live_count() == 0
            && record.heap.slot_count() == 0
            && record.instances.is_empty()
        {
            return Ok(ImageVm {
                limits: image_limits(record.config),
                slots: Vec::new(),
                slot_versions: Vec::new(),
                objects: Vec::new(),
                instances: Vec::new(),
            });
        }
        let roots: Vec<ObjRef> = self.vm_images[key.image as usize]
            .slots
            .iter()
            .filter_map(|target| match target {
                crate::machine::ImageSlotTarget::Value(Value::Obj(reference)) => Some(*reference),
                _ => None,
            })
            .collect();
        let limits = self.vm_images[key.image as usize].config.graph;
        let order = lm_graph::snapshot_ordinals(
            &mut self.vm_images[key.image as usize].heap,
            &roots,
            &limits,
        )
        .map_err(|code| {
            SnapshotFail::Fault(code, "a VM value-slot graph passed a limit".to_string())
        })?;
        let mut ordinals = vec![u32::MAX; self.vm_images[key.image as usize].heap.slot_count()];
        for (ordinal, reference) in order.iter().enumerate() {
            ordinals[reference.slot as usize] = ordinal as u32;
        }
        let map = |reference: ObjRef| ObjRef {
            slot: ordinals[reference.slot as usize],
            generation: 0,
        };
        let map_value = |value: Value| match value {
            Value::Obj(reference) => Value::Obj(map(reference)),
            other => other,
        };
        let record = &self.vm_images[key.image as usize];
        let mut objects = Vec::with_capacity(order.len());
        for reference in &order {
            let source = record.heap.get(*reference);
            if source.shape().boundary == lm_heap::BoundaryPolicy::HolderLocal {
                return Err(SnapshotFail::Fault(
                    FaultCode::BoundaryViolation,
                    "a VM value slot holds a holder-local object".to_string(),
                ));
            }
            let object = match source {
                Object::NativeHandle { proc, generation } => Object::NativeHandle {
                    proc: self.require_ordinal(*proc, ordinal_of)?,
                    generation: *generation,
                },
                other => other
                    .try_clone_remapped(map)
                    .map_err(|_| SnapshotFail::LimitExceeded)?,
            };
            objects.push(ImageObject {
                frozen: record.heap.is_frozen(*reference),
                object,
            });
        }
        let mut slots = Vec::with_capacity(record.slots.len());
        for target in record.slots.iter() {
            slots.push(match target {
                crate::machine::ImageSlotTarget::Empty => ImageSlotTarget::Empty,
                crate::machine::ImageSlotTarget::Function(func) => ImageSlotTarget::Function(*func),
                crate::machine::ImageSlotTarget::Class { class, constructor } => {
                    ImageSlotTarget::Class {
                        class: *class,
                        constructor: *constructor,
                    }
                }
                crate::machine::ImageSlotTarget::Value(value) => {
                    ImageSlotTarget::Value(map_value(*value))
                }
                crate::machine::ImageSlotTarget::Process { proc, generation } => {
                    ImageSlotTarget::Process {
                        proc: self.require_ordinal(*proc, ordinal_of)?,
                        generation: *generation,
                    }
                }
            });
        }
        let mut instances = Vec::new();
        instances
            .try_reserve_exact(record.instances.len())
            .map_err(|_| SnapshotFail::LimitExceeded)?;
        for instance in &record.instances {
            let copy = |source: &[u32]| -> Result<Vec<u32>, SnapshotFail> {
                let mut target = Vec::new();
                target
                    .try_reserve_exact(source.len())
                    .map_err(|_| SnapshotFail::LimitExceeded)?;
                target.extend_from_slice(source);
                Ok(target)
            };
            let interface = match &instance.interface {
                Some(source) => {
                    let mut target = Vec::new();
                    target
                        .try_reserve_exact(source.len())
                        .map_err(|_| SnapshotFail::LimitExceeded)?;
                    target.extend_from_slice(source.as_slice());
                    Some(target)
                }
                None => None,
            };
            instances.push(ImageInstance {
                installation: instance.installation,
                interface,
                semantic_hash: instance.semantic_hash,
                entry: instance.entry,
                funcs: copy(&instance.funcs)?,
                classes: copy(&instance.classes)?,
                slots: copy(&instance.slots)?,
            });
        }
        Ok(ImageVm {
            limits: image_limits(record.config),
            slots,
            slot_versions: record.slot_versions.clone(),
            objects,
            instances,
        })
    }

    /// The canonical digest of the closed result type of one machine.
    ///
    /// A machine that never loaded a body function records zeros.
    fn machine_result_digest(
        &mut self,
        vm: VmId,
        class_hashes: &[[u8; 32]],
    ) -> Result<[u8; 32], SnapshotFail> {
        let record = &self.machines[vm as usize];
        let Some(func) = record.body_func else {
            return Ok([0u8; 32]);
        };
        let witness = record.witness;
        let ret = self.module.funcs[func as usize].ret;
        let module = self.module.clone();
        let closed = self.envs.close(&module, ret, witness).map_err(|_| {
            SnapshotFail::Fault(
                FaultCode::BoundaryLimit,
                "the result type passed the closed type limit".to_string(),
            )
        })?;
        Ok(self.envs.digest(&module, class_hashes, closed))
    }

    /// Build the closed type table and the environment table of one
    /// image, and rewrite every stored witness to an image ordinal.
    ///
    /// The world table holds the types of the whole world, so the
    /// image carries the reachable part alone. The order is the
    /// canonical walk: machine witness, frames, then objects, for each
    /// machine in ordinal order. A closed type takes its ordinal in
    /// post-order, so every child precedes its parent.
    fn build_type_tables(
        &self,
        vm_images: &mut [ImageVm],
        machines: &mut [ImageMachine],
    ) -> (Vec<ClosedType>, Vec<TypeEnv>) {
        let mut env_map: HashMap<u32, u32> = HashMap::new();
        env_map.insert(0, 0);
        let mut world_envs: Vec<TypeEnvId> = vec![TypeEnvId::EMPTY];
        let mut order: Vec<u32> = Vec::new();
        for image in vm_images.iter() {
            for entry in &image.objects {
                if let Object::Instance { env, .. } | Object::Closure { env, .. } = &entry.object {
                    order.push(env.env().0);
                }
            }
        }
        for machine in machines.iter() {
            order.push(machine.witness);
            for frame in &machine.frames {
                order.push(frame.env);
            }
            for callback in &machine.callbacks {
                order.push(callback.env);
            }
            for entry in &machine.objects {
                if let Object::Instance { env, .. } | Object::Closure { env, .. } = &entry.object {
                    order.push(env.env().0);
                }
            }
        }
        for world in order {
            if env_map.contains_key(&world) {
                continue;
            }
            env_map.insert(world, world_envs.len() as u32);
            world_envs.push(TypeEnvId(world));
        }
        // The closed types, in post-order over the environments.
        let mut type_map: HashMap<u32, u32> = HashMap::new();
        let mut types: Vec<ClosedType> = Vec::new();
        for world in &world_envs {
            let entry = self
                .envs
                .env(*world)
                .expect("a captured type environment exists");
            for ty in entry.types.iter().copied() {
                self.push_closed(ty, &mut type_map, &mut types);
            }
        }
        let mut direct_types = Vec::new();
        for image in vm_images.iter() {
            collect_vm_image_empty_types(image, &mut direct_types);
        }
        for machine in machines.iter() {
            collect_machine_empty_types(machine, &mut direct_types);
        }
        for ty in direct_types {
            self.push_closed(ty, &mut type_map, &mut types);
        }
        let mut envs: Vec<TypeEnv> = Vec::with_capacity(world_envs.len());
        for world in &world_envs {
            let entry = self
                .envs
                .env(*world)
                .expect("a captured type environment exists");
            let mapped_types = entry
                .types
                .iter()
                .map(|ty| {
                    type_map
                        .get(ty)
                        .copied()
                        .expect("a captured type has an image ordinal")
                })
                .collect();
            envs.push(TypeEnv {
                types: mapped_types,
                rows: entry.rows.clone(),
            });
        }
        let map_env = |world: u32| -> u32 {
            env_map
                .get(&world)
                .copied()
                .expect("a captured environment has an image ordinal")
        };
        for image in vm_images.iter_mut() {
            for entry in &mut image.objects {
                match &mut entry.object {
                    Object::Instance { env, .. } | Object::Closure { env, .. } => {
                        *env = Witness(TypeEnvId(map_env(env.env().0)));
                    }
                    _ => {}
                }
                remap_object_empty_types(&mut entry.object, &type_map);
            }
            for target in &mut image.slots {
                if let ImageSlotTarget::Value(value) = target {
                    remap_empty_type(value, &type_map);
                }
            }
        }
        for machine in machines.iter_mut() {
            machine.witness = map_env(machine.witness);
            for frame in &mut machine.frames {
                frame.env = map_env(frame.env);
            }
            for callback in &mut machine.callbacks {
                callback.env = map_env(callback.env);
            }
            for entry in &mut machine.objects {
                match &mut entry.object {
                    Object::Instance { env, .. } | Object::Closure { env, .. } => {
                        *env = Witness(TypeEnvId(map_env(env.env().0)));
                    }
                    _ => {}
                }
            }
            remap_machine_empty_types(machine, &type_map);
        }
        (types, envs)
    }

    /// Give one closed type node and its whole subtree an image
    /// ordinal, children first.
    ///
    /// The walk is iterative, so a deep type never grows the Rust
    /// stack. A child index of the world table is always smaller than
    /// its parent, so the walk terminates.
    fn push_closed(&self, root: u32, map: &mut HashMap<u32, u32>, out: &mut Vec<ClosedType>) {
        if map.contains_key(&root) {
            return;
        }
        let mut stack: Vec<(u32, bool)> = vec![(root, false)];
        while let Some((id, expanded)) = stack.pop() {
            if map.contains_key(&id) {
                continue;
            }
            let node = self.envs.ty(id).expect("a captured closed type exists");
            if !expanded {
                stack.push((id, true));
                for child in node.children().into_iter().rev() {
                    if !map.contains_key(&child) {
                        stack.push((child, false));
                    }
                }
                continue;
            }
            let mapped = node.remap(|child| {
                map.get(&child)
                    .copied()
                    .expect("a closed type child has an image ordinal")
            });
            map.insert(id, out.len() as u32);
            out.push(mapped);
        }
    }

    /// Build one captured machine record.
    fn build_machine(
        &mut self,
        vm: VmId,
        is_root: bool,
        self_root: bool,
        ordinal_of: &impl Fn(VmId) -> Option<u32>,
        image_ordinal: &impl Fn(VmImageKey) -> Option<u32>,
    ) -> Result<ImageMachine, SnapshotFail> {
        let limits = self.machines[vm as usize].config.graph;
        let roots = self.machines[vm as usize].snapshot_roots();
        let order = {
            let heap = &mut self.machines[vm as usize].vm.heap;
            lm_graph::snapshot_ordinals(heap, &roots, &limits).map_err(|code| {
                SnapshotFail::Fault(code, "the capture passed a graph limit".to_string())
            })?
        };
        // The ordinal table is one entry per heap slot, so the
        // remapping never searches.
        let mut slot_ordinal: Vec<u32> = vec![u32::MAX; self.heap_of(vm).slot_count()];
        for (ordinal, r) in order.iter().enumerate() {
            slot_ordinal[r.slot as usize] = ordinal as u32;
        }
        let map = |r: ObjRef| -> ObjRef {
            ObjRef {
                slot: slot_ordinal[r.slot as usize],
                generation: 0,
            }
        };
        let callback_order = self.machines[vm as usize].snapshot_callbacks();
        let mut callback_ordinal = vec![u32::MAX; self.machines[vm as usize].callbacks.len()];
        for (ordinal, reference) in callback_order.iter().enumerate() {
            callback_ordinal[reference.slot as usize] = ordinal as u32;
        }
        let map_callback = |reference: CallbackRef| -> CallbackRef {
            CallbackRef {
                slot: callback_ordinal[reference.slot as usize],
                generation: 0,
            }
        };
        let map_value = |v: Value| -> Value {
            match v {
                Value::Obj(r) => Value::Obj(map(r)),
                Value::Callback(reference) => Value::Callback(map_callback(reference)),
                other => other,
            }
        };
        let mut objects: Vec<ImageObject> = Vec::with_capacity(order.len());
        for r in &order {
            let frozen = self.heap_of(vm).is_frozen(*r);
            let source = self.heap_of(vm).get(*r);
            let object = match source {
                Object::NativeVm { image, generation } => {
                    let key = VmImageKey {
                        image: *image,
                        generation: *generation,
                    };
                    Object::NativeVm {
                        image: image_ordinal(key).ok_or_else(|| {
                            SnapshotFail::Fault(
                                FaultCode::BoundaryViolation,
                                "a VM image has no snapshot ordinal".to_string(),
                            )
                        })?,
                        generation: 0,
                    }
                }
                Object::NativeCodeHandle {
                    image,
                    generation,
                    instance,
                    kind,
                    index,
                } => {
                    let key = VmImageKey {
                        image: *image,
                        generation: *generation,
                    };
                    Object::NativeCodeHandle {
                        image: image_ordinal(key).ok_or_else(|| {
                            SnapshotFail::Fault(
                                FaultCode::BoundaryViolation,
                                "a code handle has no VM image ordinal".to_string(),
                            )
                        })?,
                        generation: 0,
                        instance: *instance,
                        kind: *kind,
                        index: *index,
                    }
                }
                Object::NativeSlotChange {
                    image,
                    generation,
                    slot,
                    version,
                    kind,
                    target,
                } => {
                    let key = VmImageKey {
                        image: *image,
                        generation: *generation,
                    };
                    Object::NativeSlotChange {
                        image: image_ordinal(key).ok_or_else(|| {
                            SnapshotFail::Fault(
                                FaultCode::BoundaryViolation,
                                "a slot change has no VM image ordinal".to_string(),
                            )
                        })?,
                        generation: 0,
                        slot: *slot,
                        version: *version,
                        kind: *kind,
                        target: map_value(*target),
                    }
                }
                Object::NativeRun { vm: target } => Object::NativeRun {
                    vm: self.require_ordinal(*target, ordinal_of)?,
                },
                Object::NativeTable { vm: target } => Object::NativeTable {
                    vm: self.require_ordinal(*target, ordinal_of)?,
                },
                Object::NativeRequest {
                    vm: target,
                    ordinal,
                } => Object::NativeRequest {
                    vm: self.require_ordinal(*target, ordinal_of)?,
                    ordinal: *ordinal,
                },
                Object::NativeCall {
                    vm: target,
                    ordinal,
                    op,
                } => Object::NativeCall {
                    vm: self.require_ordinal(*target, ordinal_of)?,
                    ordinal: *ordinal,
                    op: *op,
                },
                Object::NativeHandle { proc, generation } => Object::NativeHandle {
                    proc: self.require_ordinal(*proc, ordinal_of)?,
                    generation: *generation,
                },
                Object::NativeFileHandle { .. } => Object::NativeFileHandle { resource: 0 },
                Object::NativeTcpStream { .. } => Object::NativeTcpStream { resource: 0 },
                Object::NativeTcpListener { .. } => Object::NativeTcpListener { resource: 0 },
                Object::NativeTlsStream { .. } => Object::NativeTlsStream { resource: 0 },
                Object::NativeRawMode { .. } => Object::NativeRawMode { resource: 0 },
                Object::NativeSignalStream { .. } => Object::NativeSignalStream { resource: 0 },
                Object::NativePipeReader { .. } => Object::NativePipeReader { resource: 0 },
                Object::NativePipeWriter { .. } => Object::NativePipeWriter { resource: 0 },
                Object::NativeChild { .. } => Object::NativeChild { resource: 0 },
                Object::NativeUdpSocket { .. } => Object::NativeUdpSocket { resource: 0 },
                Object::NativeHostResource { kind, .. } => Object::NativeHostResource {
                    kind: *kind,
                    resource: 0,
                },
                Object::NativeResourceHandle { surface, .. } => Object::NativeResourceHandle {
                    surface: self.require_ordinal(*surface, ordinal_of)?,
                    resource: 0,
                },
                Object::NativeWait { owner, token } => Object::NativeWait {
                    owner: self.require_ordinal(*owner, ordinal_of)?,
                    token: *token,
                },
                // A captured world states the container of a nested
                // image, because the world leaves this process. The
                // live handle names an image of this world alone, so
                // the writer resolves it here. This is the one call
                // that writes the container of an in-process capture.
                Object::NativeSnapshotRef { image: slot } => {
                    let Some(held) = self.image_at(*slot) else {
                        return Err(SnapshotFail::Fault(
                            FaultCode::MalformedState,
                            "a snapshot value names no admitted image".to_string(),
                        ));
                    };
                    Object::NativeSnapshot(held.bytes()?.clone())
                }
                other => other
                    .try_clone_remapped(map)
                    .map_err(|_| SnapshotFail::LimitExceeded)?,
            };
            objects.push(ImageObject { frozen, object });
        }
        let mut record =
            self.machine_record(vm, &map_value, &callback_order, ordinal_of, image_ordinal)?;
        record.objects = objects;
        if is_root {
            // The restored root is holder-controlled (specification
            // 17.5), and a self snapshot restores with its request
            // pending (17.6).
            record.scheduler_owned = false;
            record.paused = false;
            if self_root {
                record.state = ImageState::Asked;
            }
        }
        Ok(record)
    }

    fn require_ordinal(
        &self,
        vm: VmId,
        ordinal_of: &impl Fn(VmId) -> Option<u32>,
    ) -> Result<u32, SnapshotFail> {
        ordinal_of(vm).ok_or_else(|| {
            SnapshotFail::Fault(
                FaultCode::BoundaryViolation,
                format!("machine {vm} is named by a handle the closed set does not hold"),
            )
        })
    }

    /// Read the non-heap half of one captured machine.
    fn machine_record(
        &self,
        vm: VmId,
        map_value: &impl Fn(Value) -> Value,
        callback_order: &[CallbackRef],
        ordinal_of: &impl Fn(VmId) -> Option<u32>,
        image_ordinal: &impl Fn(VmImageKey) -> Option<u32>,
    ) -> Result<ImageMachine, SnapshotFail> {
        let record = &self.machines[vm as usize];
        let m = &record.vm;
        let state = match m.state {
            MachineState::Empty => ImageState::Empty,
            MachineState::Ready | MachineState::Running => ImageState::Ready,
            MachineState::Asked => ImageState::Asked,
            MachineState::Blocked => ImageState::Blocked,
            MachineState::Done => ImageState::Done,
            MachineState::Faulted => ImageState::Faulted,
            MachineState::Waiting => {
                return Err(SnapshotFail::Fault(
                    FaultCode::BoundaryViolation,
                    "a waiting machine holds a live host attachment".to_string(),
                ))
            }
        };
        let frames = m
            .frames
            .iter()
            .map(|f| ImageFrame {
                func: f.func,
                block: f.block,
                ip: f.ip,
                base_local: f.base_local,
                base_operand: f.base_operand,
                closure: f.closure.map(FrameCapture::value).map(map_value),
                // The world environment identifier. `build_type_tables`
                // rewrites it to an image ordinal.
                env: f.env.0,
            })
            .collect();
        let callbacks = callback_order
            .iter()
            .map(|reference| {
                let descriptor = record.callback(*reference).map_err(|_| {
                    SnapshotFail::Fault(
                        FaultCode::BoundaryViolation,
                        "an active callback reference is stale".to_string(),
                    )
                })?;
                Ok(ImageCallback {
                    func: descriptor.func,
                    captures: descriptor
                        .captures
                        .iter()
                        .map(|value| map_value(*value))
                        .collect(),
                    env: descriptor.env.0,
                    owner_depth: descriptor.owner_depth,
                })
            })
            .collect::<Result<Vec<_>, SnapshotFail>>()?;
        let block = match m.block {
            None => None,
            Some(Block::Receive) => Some(ImageBlock::Receive),
            Some(Block::Send { target, .. }) => Some(ImageBlock::Send {
                target: self.require_ordinal(target, ordinal_of)?,
            }),
            Some(Block::Done { target, .. }) => Some(ImageBlock::Done {
                target: self.require_ordinal(target, ordinal_of)?,
            }),
            Some(Block::Wait { token }) => Some(ImageBlock::Wait { token }),
            Some(Block::Snapshot {
                target,
                remaining,
                retry,
                ..
            }) => Some(ImageBlock::Snapshot {
                target: self.require_ordinal(target, ordinal_of)?,
                remaining,
                retry,
            }),
        };
        let waits = m
            .waits
            .iter()
            .map(|(token, entry)| {
                let source = match &entry.source {
                    WaitSource::Receive => ImageWaitSource::Receive,
                    WaitSource::Drive { target } => ImageWaitSource::Drive {
                        target: self.require_ordinal(*target, ordinal_of)?,
                    },
                    WaitSource::Choice { first, second } => ImageWaitSource::Choice {
                        first: *first,
                        second: *second,
                    },
                    WaitSource::Any { roots } => ImageWaitSource::Any {
                        roots: roots.to_vec(),
                    },
                    WaitSource::Operation { .. } => {
                        return Err(SnapshotFail::Fault(
                            FaultCode::MalformedState,
                            "a host wait source reached snapshot encoding".to_string(),
                        ));
                    }
                };
                Ok(ImageWaitEntry {
                    token: *token,
                    source,
                    linked: entry.linked,
                })
            })
            .collect::<Result<Vec<_>, SnapshotFail>>()?;
        let parent = m.parent.and_then(&ordinal_of);
        let nested = m
            .nested
            .map(|target| self.require_ordinal(target, ordinal_of))
            .transpose()?;
        let routed = m
            .routed
            .map(|route| {
                let target = self.require_ordinal(route.target, ordinal_of)?;
                let cursor = match route.cursor {
                    PolicyCursor::Table(table) => match ordinal_of(table) {
                        Some(table) => ImagePolicyCursor::Table(table),
                        None => ImagePolicyCursor::Binding,
                    },
                    PolicyCursor::Root => ImagePolicyCursor::Root,
                };
                Ok(ImageRoutedRequest { target, cursor })
            })
            .transpose()?;
        Ok(ImageMachine {
            parent,
            image: record.image.and_then(image_ordinal),
            state,
            scheduler_owned: record.owner == crate::machine::Ownership::Scheduler,
            paused: record.paused,
            // The flag names machines from class and closure proc launches.
            is_proc: record.is_proc,
            body_func: record.body_func,
            // The world environment identifier. `build_type_tables`
            // rewrites it to an image ordinal.
            witness: record.witness.0,
            generation: record.generation,
            fuel: m.fuel,
            next_ordinal: m.next_ordinal,
            next_wait: m.next_wait,
            waits,
            children: record.children,
            limits: image_limits(record.config),
            objects: Vec::new(),
            callbacks,
            frames,
            locals: m.locals.iter().map(|v| map_value(*v)).collect(),
            operands: m.operands.iter().map(|v| map_value(*v)).collect(),
            literals: m
                .literals
                .iter()
                .map(|slot| {
                    slot.map(|r| match map_value(Value::Obj(r)) {
                        Value::Obj(r) => r.slot,
                        _ => unreachable!("a literal maps to an object"),
                    })
                })
                .collect(),
            start_body: record.start_body.map(|r| match map_value(Value::Obj(r)) {
                Value::Obj(r) => r.slot,
                _ => unreachable!("a proc body maps to an object"),
            }),
            pending: m.pending.as_ref().map(|p| ImagePending {
                op: p.op,
                args: p.args.iter().map(|v| map_value(*v)).collect(),
                ordinal: p.ordinal,
            }),
            nested,
            routed,
            terminal: match &m.terminal {
                None => None,
                Some(Terminal::Done(value)) => Some(ImageTerminal::Done(map_value(*value))),
                Some(Terminal::Fault(rec)) => Some(ImageTerminal::Fault(rec.clone())),
            },
            mailbox: ImageMailbox {
                limit: m.mailbox.limit,
                queue: m.mailbox.queue.iter().map(|v| map_value(*v)).collect(),
                closed: m.mailbox.closed,
                accepted: m.mailbox.accepted,
                delivered: m.mailbox.delivered,
            },
            block,
        })
    }
}

fn collect_empty_type(value: Value, out: &mut Vec<u32>) {
    if let Value::EmptyCase { ty, .. } = value {
        out.push(ty);
    }
}

fn for_object_values(object: &Object, out: &mut Vec<u32>) {
    match object {
        Object::Instance { fields, .. }
        | Object::List { items: fields, .. }
        | Object::Tuple { items: fields } => {
            for value in fields {
                collect_empty_type(*value, out);
            }
        }
        Object::Map { entries, .. } => {
            for entry in entries {
                if !entry.is_live() {
                    continue;
                }
                collect_empty_type(entry.key, out);
                collect_empty_type(entry.value, out);
            }
        }
        Object::Closure { captures, .. } => {
            for value in captures {
                collect_empty_type(*value, out);
            }
        }
        Object::DynValue { value, ty } => {
            out.push(*ty);
            collect_empty_type(*value, out);
        }
        _ => {}
    }
}

fn collect_vm_image_empty_types(image: &ImageVm, out: &mut Vec<u32>) {
    for entry in &image.objects {
        for_object_values(&entry.object, out);
    }
    for target in &image.slots {
        if let ImageSlotTarget::Value(value) = target {
            collect_empty_type(*value, out);
        }
    }
}

fn collect_machine_empty_types(machine: &ImageMachine, out: &mut Vec<u32>) {
    for entry in &machine.objects {
        for_object_values(&entry.object, out);
    }
    for value in machine.locals.iter().chain(machine.operands.iter()) {
        collect_empty_type(*value, out);
    }
    for callback in &machine.callbacks {
        for value in &callback.captures {
            collect_empty_type(*value, out);
        }
    }
    if let Some(pending) = &machine.pending {
        for value in &pending.args {
            collect_empty_type(*value, out);
        }
    }
    if let Some(ImageTerminal::Done(value)) = machine.terminal {
        collect_empty_type(value, out);
    }
    for value in &machine.mailbox.queue {
        collect_empty_type(*value, out);
    }
}

fn remap_empty_type(value: &mut Value, map: &HashMap<u32, u32>) {
    if let Value::EmptyCase { ty, .. } = value {
        *ty = *map
            .get(ty)
            .expect("a captured empty case has an image type ordinal");
    }
}

fn remap_object_empty_types(object: &mut Object, map: &HashMap<u32, u32>) {
    match object {
        Object::Instance { fields, .. }
        | Object::List { items: fields, .. }
        | Object::Tuple { items: fields } => {
            for value in fields {
                remap_empty_type(value, map);
            }
        }
        Object::Map { entries, .. } => {
            for entry in entries {
                if !entry.is_live() {
                    continue;
                }
                remap_empty_type(&mut entry.key, map);
                remap_empty_type(&mut entry.value, map);
            }
        }
        Object::Closure { captures, .. } => {
            for value in captures {
                remap_empty_type(value, map);
            }
        }
        Object::DynValue { value, ty } => {
            remap_empty_type(value, map);
            *ty = *map
                .get(ty)
                .expect("a dynamic package has an image type ordinal");
        }
        _ => {}
    }
}

fn remap_machine_empty_types(machine: &mut ImageMachine, map: &HashMap<u32, u32>) {
    for entry in &mut machine.objects {
        remap_object_empty_types(&mut entry.object, map);
    }
    for value in machine.locals.iter_mut().chain(machine.operands.iter_mut()) {
        remap_empty_type(value, map);
    }
    for callback in &mut machine.callbacks {
        for value in &mut callback.captures {
            remap_empty_type(value, map);
        }
    }
    if let Some(pending) = &mut machine.pending {
        for value in &mut pending.args {
            remap_empty_type(value, map);
        }
    }
    if let Some(ImageTerminal::Done(value)) = &mut machine.terminal {
        remap_empty_type(value, map);
    }
    for value in &mut machine.mailbox.queue {
        remap_empty_type(value, map);
    }
}
