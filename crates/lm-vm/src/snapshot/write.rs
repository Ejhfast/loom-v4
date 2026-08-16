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
    codec, Image, ImageBlock, ImageFrame, ImageLimits, ImageMachine, ImageMailbox, ImageObject,
    ImagePending, ImageState, ImageTerminal, SnapshotFail, SnapshotImage,
};
use crate::machine::{Block, MachineState, Terminal, VmId};
use crate::world::World;
use crate::FaultCode;
use lm_bytecode::closed::{ClosedType, TypeEnv};
use lm_heap::Object;
use lm_value::{ObjRef, TypeEnvId, Value, Witness};
use std::collections::{HashMap, VecDeque};

/// What one finished cut recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutReport {
    /// Every machine of the closed set, in ascending identifier order.
    pub set: Vec<VmId>,
    /// The canonical machine order: ordinal `i` names `order[i]`.
    ///
    /// The order is a breadth-first walk from the root over the
    /// machine references of the captured state. It never reads a
    /// scheduler identifier, so two worlds with equal shapes produce
    /// equal ordinals.
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

impl World<'_> {
    /// Run one consistent cut from `root`.
    ///
    /// The call encodes nothing. `capture_snapshot` adds the
    /// capturability rules and the encoding step.
    pub fn run_cut(&mut self, barrier: u32, root: VmId) -> Result<CutReport, CutError> {
        let mut set: Vec<VmId> = Vec::new();
        let mut queue: Vec<VmId> = vec![root];
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
                let path = self.machine_path(root, vm);
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
        let order = self.machine_order(root);
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
        let report = match self.run_cut(barrier, root) {
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
            if let Err(message) = self.capturable(vm, root, self_root) {
                self.release_cut(&report.set, true);
                return Err(SnapshotFail::Fault(
                    FaultCode::InvalidVmState,
                    format!("machine {vm} {message}"),
                ));
            }
        }
        // Step 6: encode, now that every preflight succeeded.
        let built = self.build_image(&report, self_root);
        // Step 7: resume the original world, whatever the encoding
        // answered.
        self.release_cut(&report.set, true);
        let image = built?;
        let limit = self.snapshot_byte_limit(root);
        // The cut copies a stopped verified world, so the admission
        // invariant holds by construction (specification section 7.2).
        // The constructor stays inside the snapshot module, so no host
        // code can promote an arbitrary image through this path.
        let identity = self.admission_identity()?;
        codec::from_trusted_capture(image, identity, limit)
    }

    /// The admission identity of the program this world runs.
    fn admission_identity(&self) -> Result<super::AdmissionIdentity, SnapshotFail> {
        let identity = self.identity().map_err(|code| {
            SnapshotFail::Fault(code, "the program has no verified identity".to_string())
        })?;
        Ok(super::AdmissionIdentity {
            module_semantic: identity.semantic_hash,
            verification: lm_bytecode::identity::verification_hash(self.module()),
            format: super::FORMAT_VERSION,
            abi_version: lm_abi::ABI_VERSION,
            compiler_abi: lm_bytecode::identity::COMPILER_ABI_VERSION,
            verifier_version: lm_verify::VERIFIER_VERSION,
        })
    }

    /// The byte limit of one snapshot the machine `vm` asks for.
    fn snapshot_byte_limit(&self, vm: VmId) -> usize {
        self.machines[vm as usize].config.snapshot_bytes
    }

    /// The canonical machine order of one closed set.
    ///
    /// The walk is breadth-first from the root over the machine
    /// references of each captured heap, in canonical object order. It
    /// reads no machine identifier, so the ordinals depend on the
    /// world shape alone.
    fn machine_order(&mut self, root: VmId) -> Vec<VmId> {
        let mut order: Vec<VmId> = Vec::new();
        let mut queue: VecDeque<VmId> = VecDeque::new();
        queue.push_back(root);
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
    pub(crate) fn machine_path(&mut self, root: VmId, target: VmId) -> Vec<u32> {
        let mut parent: std::collections::BTreeMap<VmId, VmId> = std::collections::BTreeMap::new();
        let mut order: Vec<VmId> = vec![root];
        let mut queue: VecDeque<VmId> = VecDeque::new();
        queue.push_back(root);
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
        let ordinal = |vm: VmId| order.iter().position(|m| *m == vm).unwrap_or(0) as u32;
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
    /// A machine that a driver holds mid flight has activation state
    /// outside its own record, and a snapshot copies machine state
    /// alone. Two shapes are still copyable:
    ///
    /// - the root of a receiverless self snapshot. Its activation
    ///   belongs to the original world, and the restored root holds
    ///   the pending request instead (specification 17.6);
    /// - a machine that blocked with its own one-activation stack
    ///   stored. That stack holds one machine and one stop mode, and
    ///   the scheduler rebuilds it when the block clears.
    fn capturable(&self, vm: VmId, root: VmId, self_root: bool) -> Result<(), String> {
        let self_snapshot_root = self_root && vm == root;
        if self_snapshot_root {
            return Ok(());
        }
        match self.state_of(vm) {
            MachineState::Waiting => return Err("holds a pending host operation".to_string()),
            MachineState::Running => {
                return Err("is running, so it is not at a boundary".to_string())
            }
            _ => {}
        }
        let stack = self.suspended_len(vm);
        if stack > 1 {
            return Err("holds a nested suspended activation stack".to_string());
        }
        if self.machines[vm as usize].active as usize > stack {
            return Err("is in use by a driver".to_string());
        }
        Ok(())
    }

    /// Build one canonical image from the stopped world.
    fn build_image(&mut self, report: &CutReport, self_root: bool) -> Result<Image, SnapshotFail> {
        let root = report.order[0];
        let ordinal_of = |vm: VmId| -> Option<u32> {
            report.order.iter().position(|m| *m == vm).map(|i| i as u32)
        };
        let mut funcs: Vec<u32> = Vec::new();
        let mut classes: Vec<u32> = Vec::new();
        let mut machines: Vec<ImageMachine> = Vec::new();
        for (idx, vm) in report.order.iter().copied().enumerate() {
            let machine = self.build_machine(vm, idx == 0, self_root, &ordinal_of)?;
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
                    _ => {}
                }
            }
            for frame in &machine.frames {
                if !funcs.contains(&frame.func) {
                    funcs.push(frame.func);
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
            machines.push(machine);
        }
        // Every class the closed type table names joins the manifest,
        // so admission resolves it and proves its definition hash.
        let (types, envs) = self.build_type_tables(&mut machines);
        for node in &types {
            if let ClosedType::Class(class) | ClosedType::Inst(class, _) = node {
                if !classes.contains(class) {
                    classes.push(*class);
                }
            }
        }
        funcs.sort_unstable();
        classes.sort_unstable();
        let identity = self.identity().map_err(|code| {
            SnapshotFail::Fault(code, "the program has no verified identity".to_string())
        })?;
        let funcs: Vec<(u32, [u8; 32])> = funcs
            .into_iter()
            .map(|slot| (slot, identity.func_hashes[slot as usize]))
            .collect();
        let class_hashes: Vec<[u8; 32]> = identity.class_hashes.clone();
        let classes: Vec<(u32, [u8; 32])> = classes
            .into_iter()
            .map(|slot| (slot, class_hashes[slot as usize]))
            .collect();
        let semantic = identity.semantic_hash;
        // The header names the result type of the root machine, so a
        // tool reads it without walking the machine records. The
        // loader derives the same digest from the machine witness and
        // proves the two agree.
        let result_type = self.machine_result_digest(root, &class_hashes);
        Ok(Image {
            format: super::FORMAT_VERSION,
            abi_version: lm_abi::ABI_VERSION,
            compiler_abi: lm_bytecode::identity::COMPILER_ABI_VERSION,
            verifier_version: lm_verify::VERIFIER_VERSION,
            module_semantic: semantic,
            result_type,
            funcs,
            classes,
            types,
            envs,
            machines,
        })
    }

    /// The canonical digest of the closed result type of one machine.
    ///
    /// A machine that never loaded a body function records zeros.
    fn machine_result_digest(&mut self, vm: VmId, class_hashes: &[[u8; 32]]) -> [u8; 32] {
        let record = &self.machines[vm as usize];
        let Some(func) = record.body_func else {
            return [0u8; 32];
        };
        let witness = record.witness;
        let ret = self.module.funcs[func as usize].ret;
        let module = self.module;
        let Ok(closed) = self.envs.close(module, ret, witness) else {
            return [0u8; 32];
        };
        self.envs.digest(module, class_hashes, closed)
    }

    /// Build the closed type table and the environment table of one
    /// image, and rewrite every stored witness to an image ordinal.
    ///
    /// The world table holds the types of the whole world, so the
    /// image carries the reachable part alone. The order is the
    /// canonical walk: machine witness, frames, then objects, for each
    /// machine in ordinal order. A closed type takes its ordinal in
    /// post-order, so every child precedes its parent.
    fn build_type_tables(&self, machines: &mut [ImageMachine]) -> (Vec<ClosedType>, Vec<TypeEnv>) {
        let mut env_map: HashMap<u32, u32> = HashMap::new();
        env_map.insert(0, 0);
        let mut world_envs: Vec<TypeEnvId> = vec![TypeEnvId::EMPTY];
        let mut order: Vec<u32> = Vec::new();
        for machine in machines.iter() {
            order.push(machine.witness);
            for frame in &machine.frames {
                order.push(frame.env);
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
            let Some(entry) = self.envs.env(*world) else {
                continue;
            };
            for ty in entry.types.clone() {
                self.push_closed(ty, &mut type_map, &mut types);
            }
        }
        let envs: Vec<TypeEnv> = world_envs
            .iter()
            .map(|world| match self.envs.env(*world) {
                Some(entry) => TypeEnv {
                    types: entry
                        .types
                        .iter()
                        .map(|ty| type_map.get(ty).copied().unwrap_or(0))
                        .collect(),
                    rows: entry.rows.clone(),
                },
                None => TypeEnv::default(),
            })
            .collect();
        let map_env = |world: u32| -> u32 { env_map.get(&world).copied().unwrap_or(0) };
        for machine in machines.iter_mut() {
            machine.witness = map_env(machine.witness);
            for frame in &mut machine.frames {
                frame.env = map_env(frame.env);
            }
            for entry in &mut machine.objects {
                match &mut entry.object {
                    Object::Instance { env, .. } | Object::Closure { env, .. } => {
                        *env = Witness(TypeEnvId(map_env(env.env().0)));
                    }
                    _ => {}
                }
            }
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
            let Some(node) = self.envs.ty(id) else {
                continue;
            };
            if !expanded {
                stack.push((id, true));
                for child in node.children().into_iter().rev() {
                    if !map.contains_key(&child) {
                        stack.push((child, false));
                    }
                }
                continue;
            }
            let mapped = node.remap(|child| map.get(&child).copied().unwrap_or(0));
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
        let map_value = |v: Value| -> Value {
            match v {
                Value::Obj(r) => Value::Obj(map(r)),
                other => other,
            }
        };
        let mut objects: Vec<ImageObject> = Vec::with_capacity(order.len());
        for r in &order {
            let frozen = self.heap_of(vm).is_frozen(*r);
            let source = self.heap_of(vm).get(*r).clone();
            let object = match source {
                Object::NativeVm { vm: target } => Object::NativeVm {
                    vm: self.require_ordinal(target, ordinal_of)?,
                },
                Object::NativeTable { vm: target } => Object::NativeTable {
                    vm: self.require_ordinal(target, ordinal_of)?,
                },
                Object::NativeRequest {
                    vm: target,
                    ordinal,
                } => Object::NativeRequest {
                    vm: self.require_ordinal(target, ordinal_of)?,
                    ordinal,
                },
                Object::NativeCall {
                    vm: target,
                    ordinal,
                    op,
                } => Object::NativeCall {
                    vm: self.require_ordinal(target, ordinal_of)?,
                    ordinal,
                    op,
                },
                Object::NativeHandle { proc, generation } => Object::NativeHandle {
                    proc: self.require_ordinal(proc, ordinal_of)?,
                    generation,
                },
                other => other.remap(map).unwrap_or(other),
            };
            objects.push(ImageObject { frozen, object });
        }
        let mut record = self.machine_record(vm, &map_value, ordinal_of)?;
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
        ordinal_of: &impl Fn(VmId) -> Option<u32>,
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
                closure: f.closure.map(|r| match map_value(Value::Obj(r)) {
                    Value::Obj(r) => r.slot,
                    _ => unreachable!("a closure maps to an object"),
                }),
                // The world environment identifier. `build_type_tables`
                // rewrites it to an image ordinal.
                env: f.env.0,
            })
            .collect();
        let block = match m.block {
            None => None,
            Some(Block::Receive) => Some(ImageBlock::Receive),
            Some(Block::Send { target, .. }) => Some(ImageBlock::Send {
                target: self.require_ordinal(target, ordinal_of)?,
            }),
            Some(Block::Done { target, .. }) => Some(ImageBlock::Done {
                target: self.require_ordinal(target, ordinal_of)?,
            }),
        };
        let parent = m.parent.and_then(&ordinal_of);
        Ok(ImageMachine {
            parent,
            state,
            scheduler_owned: record.owner == crate::machine::Ownership::Scheduler,
            paused: record.paused,
            // The flag names the machines that `Proc.Spawn` launched,
            // so a restore mints exactly the grant the launch minted.
            is_proc: record.is_proc,
            body_func: record.body_func,
            // The world environment identifier. `build_type_tables`
            // rewrites it to an image ordinal.
            witness: record.witness.0,
            generation: record.generation,
            fuel: m.fuel,
            next_ordinal: m.next_ordinal,
            children: record.children,
            limits: ImageLimits {
                fuel: record.config.fuel,
                max_frames: record.config.max_frames,
                max_stack_values: record.config.max_stack_values,
                heap_bytes: record.config.heap_bytes as u64,
                max_objects: record.config.graph.max_objects,
                max_edges: record.config.graph.max_edges,
                max_graph_bytes: record.config.graph.max_bytes,
                max_work: record.config.graph.max_work,
                max_children: record.config.max_children,
                max_resources: record.config.max_resources,
                mailbox_limit: record.config.mailbox_limit,
            },
            objects: Vec::new(),
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
