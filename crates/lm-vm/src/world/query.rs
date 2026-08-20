//! Read-only queries over the machine world.
//!
//! One part of the `World` surface. `world/mod.rs` holds the
//! state these methods read.

use super::*;

impl World {
    /// The next mailbox cut marker of this world.
    pub fn next_cut(&mut self) -> u64 {
        self.cut += 1;
        self.cut
    }

    /// The next world-gate marker of this world.
    pub fn next_gate(&mut self) -> u32 {
        self.gate += 1;
        self.gate
    }

    /// The latest world gate marker.
    pub(crate) fn gate_marker(&self) -> u32 {
        self.gate
    }

    /// Commit one prepared world gate marker.
    pub(crate) fn set_gate_marker(&mut self, gate: u32) {
        self.gate = gate;
    }

    /// Record that one restore committed a machine into this world.
    pub(crate) fn mark_restored(&mut self) {
        self.restored_any = true;
    }

    /// True after one restore committed a machine into this world.
    ///
    /// A test reads it to state that a restore turns the boundary
    /// check on.
    pub fn restored_any(&self) -> bool {
        self.restored_any
    }

    /// Reserve one restored gate record before restore commit.
    pub(crate) fn prepare_gate_group(&mut self) -> Result<(), FaultCode> {
        self.gate_groups
            .try_reserve(1)
            .map_err(|_| FaultCode::HostFault)
    }

    /// Install one prepared restored gate record.
    pub(crate) fn install_gate_group(&mut self, id: u32, members: Vec<VmId>) {
        self.gate_groups.push(GateGroup { id, members });
    }

    /// The world gate one machine sits behind, or zero.
    pub fn gate_of(&self, vm: VmId) -> u32 {
        self.machines[vm as usize].gate
    }

    /// Open the world gate of one machine, and of every machine
    /// behind the same gate.
    ///
    /// The first `run`, `step`, or `drive` of a restored root calls
    /// it, so a restored world starts as one world, never as a set of
    /// procs that drift apart before the holder resumes them.
    pub fn open_gate(&mut self, vm: VmId) {
        let gate = self.machines[vm as usize].gate;
        if gate == 0 {
            return;
        }
        let Some(at) = self.gate_groups.iter().position(|group| group.id == gate) else {
            self.machines[vm as usize].gate = 0;
            if let Some(key) = self.task_key(vm) {
                self.emit_ready(key);
            }
            return;
        };
        let group = self.gate_groups.swap_remove(at);
        for member in group.members {
            if self
                .machines
                .get(member as usize)
                .is_none_or(|machine| machine.gate != gate)
            {
                continue;
            }
            self.machines[member as usize].gate = 0;
            if let Some(key) = self.task_key(member) {
                if member == 0 || self.scheduler_procs.contains(key) {
                    self.emit_ready(key);
                }
            }
        }
    }

    /// The number of whole-image structural checks this world ran.
    pub fn snapshot_checks(&self) -> u64 {
        self.checks
    }

    /// Record one whole-image structural check.
    pub(crate) fn record_snapshot_check(&mut self) {
        self.checks = self.checks.saturating_add(1);
    }

    /// Remember one admitted image of this world.
    /// The cache answers the external byte path alone. An in-process
    /// capture names its image by slot, so it never reaches this.
    pub fn trust_image(&mut self, image: &crate::snapshot::SnapshotImage) {
        let Ok(hash) = image.hash() else {
            return;
        };
        if self.trusted_index.contains_key(&hash) {
            return;
        }
        let bytes = image.resident_bytes();
        let limit = self.budget.limits.max_cached_image_bytes;
        if bytes > limit {
            return;
        }
        while self
            .trusted_bytes
            .checked_add(bytes)
            .is_none_or(|total| total > limit)
        {
            let Some((evicted, _, removed)) = self.trusted.pop_back() else {
                break;
            };
            self.trusted_index.remove(&evicted);
            self.trusted_bytes = self.trusted_bytes.saturating_sub(removed);
        }
        self.trusted_index.insert(hash, image.clone());
        self.trusted.push_front((hash, image.clone(), bytes));
        self.trusted_bytes += bytes;
    }

    /// The admitted image with this container hash.
    pub(super) fn trusted_image(&self, hash: &[u8; 32]) -> Option<crate::snapshot::SnapshotImage> {
        self.trusted_index.get(hash).cloned()
    }

    /// Install one external snapshot container into this world.
    ///
    /// This is the external byte path of specification 17.8. It
    /// decodes and admits the bytes once and remembers the admitted
    /// image, so a later restore of the same bytes repeats nothing.
    /// The trusted in-process path is `capture_snapshot`, and the two
    /// never share an entry point.
    pub fn load_snapshot_bytes(
        &mut self,
        bytes: &[u8],
    ) -> Result<crate::snapshot::SnapshotImage, crate::snapshot::ImageError> {
        let limits = crate::snapshot::LoadLimits::default();
        self.record_snapshot_check();
        let image = crate::snapshot::codec::load_external(bytes, &self.base_loaded, limits)?;
        self.trust_image(&image);
        Ok(image)
    }

    /// The number of machines a barrier may reach.
    pub fn machine_ids(&self) -> Vec<VmId> {
        (0..self.machines.len() as VmId).collect()
    }

    /// True when one machine holds a loaded or terminal state, so a
    /// barrier must stop it.
    pub fn is_live_machine(&self, vm: VmId) -> bool {
        self.machines[vm as usize].vm.state != MachineState::Empty
    }

    /// Every machine one machine names in its reachable state.
    ///
    /// Native run and control shapes name machines. VM image handles
    /// name the separate image registry.
    ///
    /// A nested edge and a routed request also name machines. The walk
    /// reports both records.
    ///
    /// The walk starts at the snapshot roots, which cover the frame
    /// closures, the locals, the operands, the pending arguments, the
    /// terminal result, the accepted mailbox queue, the proc body, and
    /// the interned literals. It excludes the policy table, because
    /// specification 17.2 excludes policy tables from a snapshot. A
    /// machine that only a table-held mock closure names is therefore
    /// not part of the world.
    ///
    /// Heap references use canonical object order. The nested edge and
    /// routed target follow in that order.
    ///
    /// The image ordinals read this order. They never depend on a
    /// scheduler identifier.
    pub fn machine_references(&mut self, vm: VmId) -> Result<Vec<VmId>, FaultCode> {
        let roots = self.machines[vm as usize].snapshot_roots();
        let limits = self.machines[vm as usize].config.graph;
        let order = {
            let m = &mut self.machines[vm as usize];
            lm_graph::snapshot_ordinals(&mut m.vm.heap, &roots, &limits)?
        };
        let heap = &self.machines[vm as usize].vm.heap;
        let mut out: Vec<VmId> = Vec::new();
        let mut images: Vec<VmImageKey> = self.machines[vm as usize].image.into_iter().collect();
        for r in order {
            let object = heap.get(r);
            if let Object::NativeVm { image, generation } = object {
                let key = VmImageKey {
                    image: *image,
                    generation: *generation,
                };
                if !images.contains(&key) {
                    images.push(key);
                }
            }
            let target = match object {
                Object::NativeRun { vm }
                | Object::NativeTable { vm }
                | Object::NativeRequest { vm, .. }
                | Object::NativeCall { vm, .. } => Some(*vm),
                Object::NativeHandle { proc, .. } => Some(*proc),
                Object::NativeResourceHandle { surface, .. } => Some(*surface),
                _ => None,
            };
            if let Some(target) = target {
                if !out.contains(&target) {
                    out.push(target);
                }
            }
        }
        for key in images {
            let Some(image) = self.vm_images.get(key.image as usize) else {
                continue;
            };
            if !image.live || image.generation != key.generation {
                continue;
            }
            for target in &image.slots {
                if let crate::machine::ImageSlotTarget::Process { proc, .. } = target {
                    if !out.contains(proc) {
                        out.push(*proc);
                    }
                }
            }
            let roots: Vec<ObjRef> = image
                .slots
                .iter()
                .filter_map(|target| match target {
                    crate::machine::ImageSlotTarget::Value(Value::Obj(reference)) => {
                        Some(*reference)
                    }
                    _ => None,
                })
                .collect();
            let limits = image.config.graph;
            let order = {
                let image = &mut self.vm_images[key.image as usize];
                lm_graph::snapshot_ordinals(&mut image.heap, &roots, &limits)?
            };
            let image = &self.vm_images[key.image as usize];
            for reference in order {
                if let Object::NativeHandle { proc, .. } = image.heap.get(reference) {
                    if !out.contains(proc) {
                        out.push(*proc);
                    }
                }
            }
        }
        for target in [
            self.machines[vm as usize].vm.nested,
            self.machines[vm as usize]
                .vm
                .routed
                .map(|route| route.target),
        ]
        .into_iter()
        .flatten()
        {
            if !out.contains(&target) {
                out.push(target);
            }
        }
        for entry in self.machines[vm as usize].vm.waits.values() {
            if let crate::machine::WaitSource::Drive { target } = entry.source {
                if !out.contains(&target) {
                    out.push(target);
                }
            }
        }
        if let Some(Block::Snapshot { target, .. }) = self.machines[vm as usize].vm.block {
            if !out.contains(&target) {
                out.push(target);
            }
        }
        Ok(out)
    }

    /// Every admitted image slot one machine names in its reachable
    /// state.
    ///
    /// A live heap names an image through one shape. The walk is the
    /// walk `machine_references` uses, so the two report the same
    /// reachable object set.
    pub(crate) fn image_references(&mut self, vm: VmId) -> Result<Vec<u32>, FaultCode> {
        let roots = self.machines[vm as usize].snapshot_roots();
        let limits = self.machines[vm as usize].config.graph;
        let order = {
            let m = &mut self.machines[vm as usize];
            lm_graph::snapshot_ordinals(&mut m.vm.heap, &roots, &limits)?
        };
        let heap = &self.machines[vm as usize].vm.heap;
        let mut out: Vec<u32> = Vec::new();
        for r in order {
            if let Object::NativeSnapshotRef { image } = heap.get(r) {
                if !out.contains(image) {
                    out.push(*image);
                }
            }
        }
        Ok(out)
    }

    /// The slot generation of one machine.
    pub fn generation_of(&self, vm: VmId) -> u32 {
        self.machines[vm as usize].generation
    }

    /// Split access to two distinct machines.
    ///
    /// `transfer` routes an equal pair to the one-heap copy, so this
    /// call always receives two machines.
    pub(super) fn two(&mut self, a: VmId, b: VmId) -> (&mut Machine, &mut Machine) {
        debug_assert_ne!(a, b, "a boundary transfer needs two machines");
        let (a, b) = (a as usize, b as usize);
        if a < b {
            let (left, right) = self.machines.split_at_mut(b);
            (&mut left[a], &mut right[0])
        } else {
            let (left, right) = self.machines.split_at_mut(a);
            (&mut right[0], &mut left[b])
        }
    }

    /// The number of live host resources one machine holds.
    pub fn resource_count(&self, vm: VmId) -> usize {
        self.machines[vm as usize].resources.live_count()
    }

    /// The number of child machines one machine reserved.
    pub fn child_count(&self, vm: VmId) -> u32 {
        self.machines[vm as usize].children
    }

    /// Preflight one machine for a snapshot.
    ///
    /// The check reads the resource registry and the guest graph, as
    /// specification 25.5 requires. A live host attachment on either
    /// side blocks the copy. On success the call returns the number of
    /// objects the canonical snapshot traversal ordered.
    ///
    /// The walk reads the snapshot roots, so it covers exactly the
    /// objects the encoder writes.
    pub fn snapshot_preflight(&mut self, vm: VmId) -> Result<usize, FaultCode> {
        if self.machines[vm as usize]
            .resources
            .live_attachment()
            .is_some()
        {
            return Err(FaultCode::BoundaryViolation);
        }
        let order = self.snapshot_object_order(vm)?;
        let heap = &self.machines[vm as usize].vm.heap;
        for reference in &order {
            let resource = match heap.get(*reference) {
                Object::NativeFileHandle { resource }
                | Object::NativeResourceHandle { resource, .. }
                | Object::NativeTcpStream { resource }
                | Object::NativeTcpListener { resource } => Some(*resource),
                _ => None,
            };
            if resource.is_some_and(|resource| self.bound_resources.contains_key(&resource)) {
                return Err(FaultCode::BoundaryViolation);
            }
        }
        Ok(order.len())
    }

    pub(super) fn snapshot_object_order(&mut self, vm: VmId) -> Result<Vec<ObjRef>, FaultCode> {
        let roots = self.machines[vm as usize].snapshot_roots();
        let limits = self.machines[vm as usize].config.graph;
        let machine = &mut self.machines[vm as usize];
        lm_graph::snapshot_ordinals(&mut machine.vm.heap, &roots, &limits)
    }

    /// The kind name of one live host attachment this machine holds.
    pub fn live_attachment_kind(&mut self, vm: VmId) -> Option<String> {
        let registered = self.machines[vm as usize]
            .resources
            .live_attachment()
            .map(|record| match record.kind {
                crate::ResourceKind::PendingOperation => {
                    format!("a pending {}", lm_abi::op_name(record.op))
                }
                crate::ResourceKind::File => "file handle".to_string(),
                crate::ResourceKind::TcpStream => "TCP stream".to_string(),
                crate::ResourceKind::TcpListener => "TCP listener".to_string(),
                crate::ResourceKind::TlsStream => "TLS stream".to_string(),
            });
        if registered.is_some() {
            return registered;
        }
        let order = self.snapshot_object_order(vm).ok()?;
        let heap = &self.machines[vm as usize].vm.heap;
        order.into_iter().find_map(|reference| {
            let resource = match heap.get(reference) {
                Object::NativeFileHandle { resource }
                | Object::NativeResourceHandle { resource, .. }
                | Object::NativeTcpStream { resource }
                | Object::NativeTcpListener { resource } => *resource,
                Object::NativeTlsStream { resource } => *resource,
                _ => return None,
            };
            self.bound_resources
                .get(&resource)
                .map(|bound| match bound.kind {
                    crate::ResourceKind::File => "file handle".to_string(),
                    crate::ResourceKind::TcpStream => "TCP stream".to_string(),
                    crate::ResourceKind::TcpListener => "TCP listener".to_string(),
                    crate::ResourceKind::TlsStream => "TLS stream".to_string(),
                    crate::ResourceKind::PendingOperation => "pending operation".to_string(),
                })
        })
    }

    /// The number of live activation references to one machine.
    pub fn active_of(&self, vm: VmId) -> u32 {
        self.machines[vm as usize].active
    }

    /// The verified semantic identity of the loaded program.
    pub fn identity(&self) -> Result<&lm_bytecode::identity::ModuleIdentity, FaultCode> {
        self.loaded.identity()
    }

    /// The semantic identity of the module that started this world.
    pub(crate) fn base_identity(
        &self,
    ) -> Result<&lm_bytecode::identity::ModuleIdentity, FaultCode> {
        self.base_loaded.identity()
    }

    /// Store one admitted image in the world table and name its slot.
    ///
    /// A guest snapshot value names a slot here. The table therefore
    /// holds the admitted world of every snapshot a guest still names,
    /// and nothing else.
    pub(crate) fn intern_image(&mut self, image: crate::snapshot::SnapshotImage) -> u32 {
        match self.image_free.pop() {
            Some(slot) => {
                self.images[slot as usize] = Some(image);
                slot
            }
            None => {
                self.images.push(Some(image));
                (self.images.len() - 1) as u32
            }
        }
    }

    /// The admitted image one slot names.
    pub(crate) fn image_at(&self, slot: u32) -> Option<crate::snapshot::SnapshotImage> {
        self.images.get(slot as usize).and_then(|e| e.clone())
    }

    /// The verification hash of the loaded program.
    ///
    /// Snapshot capture and restore both name the program by this
    /// hash. The module computes it once (`LoadedModule`).
    pub(crate) fn verification_hash(&self) -> [u8; 32] {
        self.loaded.verification_hash()
    }

    /// The verification hash of the module that started this world.
    pub(crate) fn base_verification_hash(&self) -> [u8; 32] {
        self.base_loaded.verification_hash()
    }

    /// The current verified aggregate code.
    pub(crate) fn loaded_code(&self) -> LoadedModule {
        self.loaded.clone()
    }

    /// The loaded program.
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// The resource limits of one machine.
    pub fn config_of(&self, vm: VmId) -> VmConfig {
        self.machines[vm as usize].config
    }

    /// Transfer one value from `src` into `dst` through the graph
    /// engine.
    ///
    /// The transfer mode accepts scalars, first-class operation
    /// values, and deeply frozen graphs of every sendable shape. It
    /// preserves cycles and sharing. A holder-local shape or a
    /// mutable object faults `UnsendableValue`, and a graph past the
    /// published limits faults `BoundaryLimit`.
    /// Transfer several values from `src` into `dst`.
    ///
    /// Each result stays rooted in the destination while the next
    /// value crosses. A destination collection during a later copy
    /// frees every object its roots do not reach, and a copied value
    /// that no machine field holds yet is one of those.
    pub(crate) fn transfer_all(
        &mut self,
        src: VmId,
        dst: VmId,
        values: &[Value],
    ) -> Result<Vec<Value>, FaultCode> {
        let mut moved: Vec<Value> = Vec::with_capacity(values.len());
        let mut result = Ok(());
        for value in values {
            match self.transfer(src, dst, *value) {
                Ok(value) => {
                    if let Some(r) = value.as_obj() {
                        self.machines[dst as usize].vm.heap.push_host_root(r);
                    }
                    moved.push(value);
                }
                Err(code) => {
                    result = Err(code);
                    break;
                }
            }
        }
        // Unroot in LIFO order. The caller stores the results in a
        // machine field before the next allocation.
        for value in moved.iter().rev() {
            if let Some(r) = value.as_obj() {
                self.machines[dst as usize].vm.heap.pop_host_root(r);
            }
        }
        result?;
        Ok(moved)
    }

    pub(crate) fn transfer(
        &mut self,
        src: VmId,
        dst: VmId,
        value: Value,
    ) -> Result<Value, FaultCode> {
        if let Some(result) = scalar_copy(value) {
            return result;
        }
        // A restored world can name one machine on both sides of a
        // crossing. The rule is the same rule, so the call runs the
        // one-heap copy there and never splits one machine in two.
        if src == dst {
            return self.boundary_copy(src, dst, value);
        }
        // The copy allocates in the destination, so the destination
        // limits govern the walk.
        let limits = self.machines[dst as usize].config.graph;
        let (src_m, dst_m) = self.two(src, dst);
        // The destination roots are read before the heap is borrowed:
        // a destination collection during the copy needs them.
        let dst_roots = dst_m.gc_roots(&[]);
        lm_graph::transfer(
            &mut src_m.vm.heap,
            &mut dst_m.vm.heap,
            &dst_roots,
            value,
            &limits,
        )
    }
}
