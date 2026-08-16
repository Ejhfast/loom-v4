//! Restore: build one complete independent world from one image
//! (specification 17.5).
//!
//! Restore is valid on an `EmptyVm` alone. It builds every machine,
//! relocates every handle, gives each restored machine a fresh
//! default-deny policy table, re-roots the pass chains on the
//! restoring machine, and keeps the restored procs behind one world
//! gate until the first `run`, `step`, or `drive` of the root.
//!
//! A failed restore exposes no partial world: the call removes every
//! machine it added and returns the reservations it took.

use super::{Image, ImageBlock, ImageMachine, ImageState, ImageTerminal, RestoreFail};
use crate::machine::{
    Block, Frame, Machine, MachineState, Mailbox, Ownership, Pending, Terminal, VmId,
};
use crate::world::World;
use crate::VmConfig;
use lm_heap::Object;
use lm_value::{ObjRef, Value};

impl World<'_> {
    /// Build one restored world from a checked image.
    ///
    /// `restorer` is the machine that asked, and `target` is its empty
    /// machine, which becomes the restored root. The call returns the
    /// root machine identifier.
    ///
    /// This is the trusted path. The image already passed the writer
    /// or the loader, so the call repeats no structural check
    /// (specification 17.8).
    pub fn restore_image(
        &mut self,
        restorer: VmId,
        target: VmId,
        image: &Image,
    ) -> Result<VmId, RestoreFail> {
        let count = image.machines.len();
        debug_assert!(count > 0, "a checked image holds at least the root");
        let before = self.machines.len();
        let charged = count - 1;
        // The restorer pays for every machine past the root out of its
        // own child budget, so a restored world can never grow past
        // the budget the holder already holds.
        let mut child_configs: Vec<VmConfig> = Vec::with_capacity(charged);
        for _ in 0..charged {
            match self.reserve_child(restorer) {
                Some(config) => child_configs.push(config),
                None => {
                    self.refund_children(restorer, child_configs.len());
                    return Err(RestoreFail::LimitExceeded);
                }
            }
        }
        // The restored limits are the image limits clamped by the
        // limits the restoring machine already runs under. An image is
        // data, so it may claim any budget; the clamp keeps a restored
        // world inside the authority of the machine that built it.
        let ceiling = self.config_of(target);
        let mut ids: Vec<VmId> = Vec::with_capacity(count);
        ids.push(target);
        for (idx, config) in child_configs.iter().enumerate() {
            let limits = clamp(&image.machines[idx + 1], *config);
            let id = self.machines.len() as VmId;
            self.machines.push(Machine::empty_at(
                limits,
                Some(restorer),
                image.machines[idx + 1].generation,
            ));
            ids.push(id);
        }
        // The root keeps its slot, so it takes the clamped limits in
        // place.
        self.machines[target as usize].config = clamp(&image.machines[0], ceiling);
        let gate = self.next_gate();
        let outcome = self.fill_world(image, &ids, restorer, gate);
        if outcome.is_err() {
            // Failure atomicity: drop every machine this call added,
            // return the reservations, and leave the target empty.
            self.machines.truncate(before);
            self.refund_children(restorer, charged);
            self.machines[target as usize] = Machine::empty_at(ceiling, Some(restorer), 0);
            return Err(RestoreFail::LimitExceeded);
        }
        Ok(target)
    }

    /// Fill every restored machine. A failure leaves the caller to
    /// roll the whole world back.
    fn fill_world(
        &mut self,
        image: &Image,
        ids: &[VmId],
        restorer: VmId,
        gate: u32,
    ) -> Result<(), RestoreFail> {
        let generations: Vec<u32> = image.machines.iter().map(|m| m.generation).collect();
        // A snapshot names a type by digest, so the restore maps each
        // digest back to the type slot of the running program.
        let mut types: std::collections::HashMap<[u8; 32], u32> = std::collections::HashMap::new();
        if let Ok(identity) = self.identity() {
            for (slot, hash) in identity.type_hashes.iter().enumerate() {
                types.entry(*hash).or_insert(slot as u32);
            }
        }
        for (ordinal, source) in image.machines.iter().enumerate() {
            let vm = ids[ordinal];
            let refs = self.restore_heap(vm, source, ids)?;
            self.restore_state(vm, source, ids, &generations, &types, &refs, restorer, gate);
        }
        Ok(())
    }

    /// Allocate the captured heap of one machine and relocate every
    /// reference inside it.
    ///
    /// The pass allocates one destination object per captured object,
    /// in canonical order, and patches the children afterwards. The
    /// heap of a restored machine therefore holds the same sharing and
    /// the same cycles as the captured one.
    fn restore_heap(
        &mut self,
        vm: VmId,
        source: &ImageMachine,
        ids: &[VmId],
    ) -> Result<Vec<ObjRef>, RestoreFail> {
        let mut refs: Vec<ObjRef> = Vec::with_capacity(source.objects.len());
        for entry in &source.objects {
            let initial = relocate_machines(&entry.object, ids);
            let cost = initial.cost();
            let heap = &mut self.machines[vm as usize].vm.heap;
            if heap.would_exceed(cost) {
                return Err(RestoreFail::LimitExceeded);
            }
            refs.push(heap.alloc(initial));
        }
        // The second pass patches every child reference. A shape
        // without references keeps the object the first pass built.
        for (ordinal, entry) in source.objects.iter().enumerate() {
            if !entry.object.shape().has_refs {
                continue;
            }
            let patched = entry
                .object
                .remap(|child| refs[child.slot as usize])
                .expect("a shape with references rebuilds");
            let heap = &mut self.machines[vm as usize].vm.heap;
            *heap.get_mut(refs[ordinal]) = patched;
            heap.recharge(refs[ordinal]);
        }
        let heap = &mut self.machines[vm as usize].vm.heap;
        for (ordinal, entry) in source.objects.iter().enumerate() {
            if entry.frozen {
                heap.set_frozen(refs[ordinal]);
            }
        }
        Ok(refs)
    }

    /// Install the non-heap half of one restored machine.
    #[allow(clippy::too_many_arguments)]
    fn restore_state(
        &mut self,
        vm: VmId,
        source: &ImageMachine,
        ids: &[VmId],
        generations: &[u32],
        types: &std::collections::HashMap<[u8; 32], u32>,
        refs: &[ObjRef],
        restorer: VmId,
        gate: u32,
    ) {
        let value = |v: Value| -> Value {
            match v {
                Value::Obj(r) => Value::Obj(refs[r.slot as usize]),
                other => other,
            }
        };
        let m = &mut self.machines[vm as usize];
        m.vm.parent = match source.parent {
            // An internal pass chain re-roots on the restored parent.
            Some(ordinal) => Some(ids[ordinal as usize]),
            // A machine whose parent stayed outside the world re-roots
            // on the restoring machine. Every restored table is
            // default-deny, so the chain carries no authority.
            None => Some(restorer),
        };
        m.vm.state = match source.state {
            ImageState::Empty => MachineState::Empty,
            ImageState::Ready => MachineState::Ready,
            ImageState::Asked => MachineState::Asked,
            ImageState::Blocked => MachineState::Blocked,
            ImageState::Done => MachineState::Done,
            ImageState::Faulted => MachineState::Faulted,
        };
        m.owner = if source.scheduler_owned {
            Ownership::Scheduler
        } else {
            Ownership::Holder
        };
        m.paused = source.paused;
        // Every restored machine starts with a fresh default-deny
        // table (specification 17.5). A restored proc takes the birth
        // grant of specification 18.3 on top of it, exactly as
        // `Proc.Spawn` mints it: the `Proc` group and nothing else.
        // Without it a restored proc could not read its own mailbox,
        // and the restored world would be a copy that cannot run.
        // The grant carries no authority of its own: the chain still
        // resolves through the table of the restoring machine.
        if source.is_proc {
            let group =
                lm_abi::group_by_name("Proc").expect("the manifest declares the Proc group");
            m.table.group[group as usize] = Some(crate::machine::Action::Pass);
        }
        m.generation = source.generation;
        m.children = source.children;
        m.result_ty = source
            .result_type
            .and_then(|digest| types.get(&digest).copied());
        m.gate = gate;
        m.vm.fuel = source.fuel;
        m.vm.next_ordinal = source.next_ordinal;
        m.vm.frames = source
            .frames
            .iter()
            .map(|f| Frame {
                func: f.func,
                block: f.block,
                ip: f.ip,
                base_local: f.base_local,
                base_operand: f.base_operand,
                closure: f.closure.map(|o| refs[o as usize]),
            })
            .collect();
        m.vm.locals = source.locals.iter().map(|v| value(*v)).collect();
        m.vm.operands = source.operands.iter().map(|v| value(*v)).collect();
        m.vm.literals = source
            .literals
            .iter()
            .map(|slot| slot.map(|o| refs[o as usize]))
            .collect();
        m.start_body = source.start_body.map(|o| refs[o as usize]);
        m.vm.pending = source.pending.as_ref().map(|p| Pending {
            op: p.op,
            args: p.args.iter().map(|v| value(*v)).collect(),
            ordinal: p.ordinal,
        });
        m.vm.terminal = match &source.terminal {
            None => None,
            Some(ImageTerminal::Done(v)) => Some(Terminal::Done(value(*v))),
            Some(ImageTerminal::Fault(rec)) => Some(Terminal::Fault(rec.clone())),
        };
        let mut mailbox = Mailbox::new(source.mailbox.limit);
        mailbox.queue = source.mailbox.queue.iter().map(|v| value(*v)).collect();
        mailbox.closed = source.mailbox.closed;
        mailbox.accepted = source.mailbox.accepted;
        mailbox.delivered = source.mailbox.delivered;
        m.vm.mailbox = mailbox;
        // A block names its target by machine ordinal. The generation
        // comes from the restored target, so the two always agree and
        // a restored block never reads as a dead peer.
        m.vm.block = source.block.map(|block| match block {
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
    }

    /// Give back `count` child reservations of one machine.
    fn refund_children(&mut self, vm: VmId, count: usize) {
        let m = &mut self.machines[vm as usize];
        m.children = m.children.saturating_sub(count as u32);
    }
}

/// Clamp the captured limits by the limits of the restoring machine.
fn clamp(source: &ImageMachine, ceiling: VmConfig) -> VmConfig {
    let graph = lm_graph::GraphLimits {
        max_objects: source.limits.max_objects.min(ceiling.graph.max_objects),
        max_edges: source.limits.max_edges.min(ceiling.graph.max_edges),
        max_bytes: source.limits.max_graph_bytes.min(ceiling.graph.max_bytes),
        max_work: source.limits.max_work.min(ceiling.graph.max_work),
    };
    VmConfig {
        fuel: source.limits.fuel.min(ceiling.fuel),
        max_frames: source.limits.max_frames.min(ceiling.max_frames),
        max_stack_values: source.limits.max_stack_values.min(ceiling.max_stack_values),
        heap_bytes: (source.limits.heap_bytes as usize).min(ceiling.heap_bytes),
        graph,
        max_children: source.limits.max_children.min(ceiling.max_children),
        max_resources: source.limits.max_resources.min(ceiling.max_resources),
        mailbox_limit: source.limits.mailbox_limit.min(ceiling.mailbox_limit),
        snapshot_bytes: ceiling.snapshot_bytes,
    }
}

/// Rebuild one captured object with every machine ordinal relocated.
///
/// The object references inside are patched later, so a shape that
/// holds references starts as its empty shell.
fn relocate_machines(object: &Object, ids: &[VmId]) -> Object {
    match object {
        Object::NativeVm { vm } => Object::NativeVm {
            vm: ids[*vm as usize],
        },
        Object::NativeTable { vm } => Object::NativeTable {
            vm: ids[*vm as usize],
        },
        Object::NativeRequest { vm, ordinal } => Object::NativeRequest {
            vm: ids[*vm as usize],
            ordinal: *ordinal,
        },
        Object::NativeCall { vm, ordinal, op } => Object::NativeCall {
            vm: ids[*vm as usize],
            ordinal: *ordinal,
            op: *op,
        },
        Object::NativeHandle { proc, generation } => Object::NativeHandle {
            proc: ids[*proc as usize],
            generation: *generation,
        },
        other if other.shape().has_refs => {
            other.shell().expect("a shape with references has a shell")
        }
        other => other.clone(),
    }
}
