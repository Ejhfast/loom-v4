//! Restore one admitted image into a machine world.
//!
//! Preparation builds all restored state outside the live machine
//! table. Commit installs that state without a fallible operation.

use super::{
    ImageBlock, ImageMachine, ImagePolicyCursor, ImageState, ImageTerminal, RestoreFail,
    SnapshotImage,
};
use crate::machine::{
    Action, Block, FaultRec, Frame, Machine, MachineState, Mailbox, Ownership, Pending,
    PolicyCursor, RoutedRequest, Terminal, VmId,
};
use crate::world::World;
use crate::VmConfig;
use lm_bytecode::closed::TypeImportPlan;
use lm_heap::Object;
use lm_value::{ObjRef, TypeEnvId, Value, Witness};

/// One complete restore that is ready for commit.
pub(crate) struct RestorePlan {
    target: VmId,
    restorer: VmId,
    machines: Vec<Machine>,
    types: TypeImportPlan,
    child_charge: u32,
    gate: u32,
    gate_members: Vec<VmId>,
}

impl World<'_> {
    /// Restore one admitted image and return its root identifier.
    pub fn restore_image(
        &mut self,
        restorer: VmId,
        target: VmId,
        admitted: &SnapshotImage,
    ) -> Result<VmId, RestoreFail> {
        let plan = self.prepare_restore(restorer, target, admitted)?;
        Ok(self.commit_restore(plan))
    }

    /// Build one restore without changing semantic world state.
    pub(crate) fn prepare_restore(
        &mut self,
        restorer: VmId,
        target: VmId,
        admitted: &SnapshotImage,
    ) -> Result<RestorePlan, RestoreFail> {
        let running = self.identity().map_err(|_| RestoreFail::OtherProgram)?;
        let identity = admitted.identity();
        if identity.module_semantic != running.semantic_hash
            || identity.verification != lm_bytecode::identity::verification_hash(self.module())
        {
            return Err(RestoreFail::OtherProgram);
        }
        if restorer == target
            || self.machines.get(target as usize).is_none()
            || self.machines.get(restorer as usize).is_none()
            || self.machines[target as usize].vm.state != MachineState::Empty
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
        if count == 0 {
            return Err(RestoreFail::OtherProgram);
        }
        let added = count - 1;
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

        let types = self
            .envs
            .prepare_import(&image.types, &image.envs)
            .map_err(|_| RestoreFail::LimitExceeded)?;
        let env_map = types.env_map();

        let mut ids = try_vec(count)?;
        ids.push(target);
        for offset in 0..added {
            let raw = self
                .machines
                .len()
                .checked_add(offset)
                .ok_or(RestoreFail::LimitExceeded)?;
            ids.push(u32::try_from(raw).map_err(|_| RestoreFail::LimitExceeded)?);
        }

        let ceiling = self.config_of(target);
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
            let refs = restore_heap(&mut machine, source, &ids, env_map)?;
            restore_state(
                &mut machine,
                source,
                &ids,
                &generations,
                env_map,
                &refs,
                restorer,
                gate,
                child_counts[ordinal],
            )?;
            machines.push(machine);
        }

        Ok(RestorePlan {
            target,
            restorer,
            machines,
            types,
            child_charge,
            gate,
            gate_members: ids,
        })
    }

    /// Install one prepared restore without an allocation.
    /// Commit one prepared restore.
    ///
    /// The commit marks the world. A restored machine holds values a
    /// container stated, so every later VM boundary of this world
    /// checks the type of the value that crosses it.
    pub(crate) fn commit_restore(&mut self, plan: RestorePlan) -> VmId {
        let RestorePlan {
            target,
            restorer,
            machines,
            types,
            child_charge,
            gate,
            gate_members,
        } = plan;
        self.envs.commit_import(types);
        self.mark_restored();
        self.set_gate_marker(gate);
        self.machines[restorer as usize].children += child_charge;
        let mut machines = machines.into_iter();
        self.machines[target as usize] = machines
            .next()
            .expect("a prepared restore holds its root machine");
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
    env_map: &[TypeEnvId],
) -> Result<Vec<ObjRef>, RestoreFail> {
    let mut bytes = 0usize;
    for entry in &source.objects {
        bytes = bytes
            .checked_add(entry.object.cost())
            .ok_or(RestoreFail::LimitExceeded)?;
    }
    if machine
        .vm
        .heap
        .would_exceed_batch(bytes, source.objects.len())
    {
        return Err(RestoreFail::LimitExceeded);
    }

    let mut refs = try_vec(source.objects.len())?;
    for ordinal in 0..source.objects.len() {
        refs.push(ObjRef {
            slot: u32::try_from(ordinal).map_err(|_| RestoreFail::LimitExceeded)?,
            generation: 0,
        });
    }
    for (ordinal, entry) in source.objects.iter().enumerate() {
        let mut object = entry
            .object
            .try_clone_remapped(|child| refs[child.slot as usize])
            .map_err(|_| RestoreFail::LimitExceeded)?;
        relocate_metadata(&mut object, ids, env_map);
        let reference = machine
            .vm
            .heap
            .try_alloc(object)
            .map_err(|_| RestoreFail::LimitExceeded)?;
        if reference != refs[ordinal] {
            return Err(RestoreFail::LimitExceeded);
        }
        if entry.frozen {
            machine.vm.heap.set_frozen(reference);
        }
    }
    Ok(refs)
}

/// Install the non-heap state of one detached machine.
#[allow(clippy::too_many_arguments)]
fn restore_state(
    machine: &mut Machine,
    source: &ImageMachine,
    ids: &[VmId],
    generations: &[u32],
    env_map: &[TypeEnvId],
    refs: &[ObjRef],
    restorer: VmId,
    gate: u32,
    children: u32,
) -> Result<(), RestoreFail> {
    let object_value = |value: Value| match value {
        Value::Obj(reference) => Value::Obj(refs[reference.slot as usize]),
        other => other,
    };

    let mut frames = try_vec(source.frames.len())?;
    for frame in &source.frames {
        frames.push(Frame {
            func: frame.func,
            block: frame.block,
            ip: frame.ip,
            base_local: frame.base_local,
            base_operand: frame.base_operand,
            closure: frame.closure.map(|ordinal| refs[ordinal as usize]),
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
            Some(Terminal::Fault(FaultRec {
                code: record.code,
                message,
                op: record.op,
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
        let group = lm_abi::group_by_name("Proc").expect("the manifest declares the Proc group");
        machine.table.group[group as usize] = Some(Action::Pass);
    }
    machine.children = children;
    machine.is_proc = source.is_proc;
    machine.body_func = source.body_func;
    machine.witness = env_map[source.witness as usize];
    machine.gate = gate;
    machine.vm.fuel = source.fuel.min(machine.config.fuel);
    machine.vm.next_ordinal = source.next_ordinal;
    machine.vm.frames = frames;
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

/// Relocate the world-local metadata of one restored object.
fn relocate_metadata(object: &mut Object, ids: &[VmId], env_map: &[TypeEnvId]) {
    match object {
        Object::Instance { env, .. } | Object::Closure { env, .. } => {
            *env = Witness(env_map[env.env().0 as usize]);
        }
        Object::NativeVm { vm }
        | Object::NativeTable { vm }
        | Object::NativeRequest { vm, .. }
        | Object::NativeCall { vm, .. } => {
            *vm = ids[*vm as usize];
        }
        Object::NativeHandle { proc, .. } => {
            *proc = ids[*proc as usize];
        }
        _ => {}
    }
}
