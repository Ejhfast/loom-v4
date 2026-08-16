//! Snapshot image admission
//! (`docs/specs/snapshot-image-admission.md` sections 5, 7, and 10).
//!
//! Admission is the one promotion from editable `Image` data to the
//! immutable `SnapshotImage` that restore accepts. It proves one rule:
//!
//! > An `Image` becomes `SnapshotImage` only when its structure
//! > resolves and every live declared type is accurate.
//!
//! "Declared type" means the type the verifier proves at a saved
//! program point, never a type label the image carries. Admission
//! derives every type from the exact verified module:
//!
//! - the slot types of a stopped frame come from the verifier
//!   dataflow, with the substitution the call site applied;
//! - the field types of an instance come from its class layout under
//!   the type arguments of the position that names it;
//! - the type of a machine handle, a proc handle, a call token, and a
//!   nested snapshot comes from the target it names.
//!
//! The graph walk visits `(machine, object, resolved type)` triples,
//! so one shared object is proved under every type that reaches it.
//! The walk is iterative and bounded, and it charges one aggregate
//! `AdmissionBudget`.

use super::{
    codec, image_roots, AdmissionIdentity, Image, ImageBlock, ImageError, ImageMachine,
    ImageReason, ImageState, ImageTerminal, LoadLimits, SnapshotImage, FORMAT_VERSION,
};
use crate::LoadedModule;
use lm_bytecode::identity::{ModuleIdentity, COMPILER_ABI_VERSION};
use lm_bytecode::BcType;
use lm_heap::Object;
use lm_value::Value;
use lm_verify::{FramePoint, FrameSlots, ResolvedTypes};
use std::collections::{HashMap, HashSet};

/// The work one admission may perform.
///
/// The budget is one aggregate ledger for the whole image. It charges
/// every resolved type, every graph pair, and every nested container,
/// so a compact container can never expand into unbounded checking
/// work.
///
/// The default limit is conservative. Worklist item 10 sizes it beside
/// the decode budget and shares one ledger with nested containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionBudget {
    limit: u64,
    used: u64,
    byte_limit: usize,
}

/// The default aggregate admission work limit, in units.
///
/// One unit covers one checked value, one visited graph pair, or one
/// resolved frame slot.
pub const DEFAULT_ADMISSION_UNITS: u64 = 1 << 24;

impl AdmissionBudget {
    /// One budget with an exact work limit.
    pub fn new(limit: u64) -> AdmissionBudget {
        AdmissionBudget {
            limit,
            used: 0,
            byte_limit: LoadLimits::default().max_bytes,
        }
    }

    /// The units this budget already spent.
    pub fn used(&self) -> u64 {
        self.used
    }

    /// The units that remain.
    pub fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.used)
    }

    /// The largest container the sealed image may encode to.
    pub fn byte_limit(&self) -> usize {
        self.byte_limit
    }

    /// Set the container byte limit of a sealed image.
    pub fn with_byte_limit(mut self, bytes: usize) -> AdmissionBudget {
        self.byte_limit = bytes;
        self
    }

    /// Charge `units` of work. The call fails once the ledger runs out.
    fn charge(&mut self, units: u64) -> Result<(), ImageError> {
        self.used = self.used.saturating_add(units);
        if self.used > self.limit {
            return Err(ImageError::admission(
                ImageReason::Budget,
                format!(
                    "the admission work passed the budget of {} units",
                    self.limit
                ),
            ));
        }
        Ok(())
    }
}

impl Default for AdmissionBudget {
    fn default() -> AdmissionBudget {
        AdmissionBudget::new(DEFAULT_ADMISSION_UNITS)
    }
}

/// Admit one editable image against one exact verified module.
///
/// The call consumes the image, so no caller keeps a mutable handle on
/// the admitted state. Success returns the sealed `SnapshotImage` with
/// its canonical bytes, its container hash, and its admission
/// identity.
pub fn admit(
    image: Image,
    loaded: &LoadedModule,
    budget: &mut AdmissionBudget,
) -> Result<SnapshotImage, ImageError> {
    let identity = prove(&image, loaded, budget)?;
    codec::seal_admitted(image, identity, budget.byte_limit())
}

/// Prove the admission rule over one image.
///
/// The call answers with the admission identity the image passed
/// against. `load_external` uses it to seal the bytes it already
/// holds, so the container is never encoded twice.
pub(super) fn prove(
    image: &Image,
    loaded: &LoadedModule,
    budget: &mut AdmissionBudget,
) -> Result<AdmissionIdentity, ImageError> {
    let identity = loaded.identity().map_err(|_| {
        ImageError::admission(ImageReason::Code, "the program has no verified identity")
    })?;
    let module = loaded.module();
    check_identity(image, identity)?;
    let types = ResolvedTypes::new(module).map_err(|e| {
        ImageError::admission(
            ImageReason::Code,
            format!("the program has no resolved type view: {e}"),
        )
    })?;
    let mut admit = Admit {
        image,
        module,
        identity,
        types,
        types_by_digest: {
            let mut map: HashMap<[u8; 32], u32> = HashMap::new();
            for (slot, hash) in identity.type_hashes.iter().enumerate() {
                map.entry(*hash).or_insert(slot as u32);
            }
            map
        },
        result_of: Vec::new(),
        mailbox_of: Vec::new(),
        frames_of: Vec::new(),
    };
    admit.run(budget)?;
    Ok(AdmissionIdentity {
        module_semantic: identity.semantic_hash,
        verification: lm_bytecode::identity::verification_hash(module),
        format: image.format,
        abi_version: image.abi_version,
        compiler_abi: image.compiler_abi,
        verifier_version: image.verifier_version,
    })
}

fn fail<T>(reason: ImageReason, detail: impl Into<String>) -> Result<T, ImageError> {
    Err(ImageError::admission(reason, detail))
}

/// Prove that the image names this exact verified program.
///
/// An admission identity mismatch rejects. The versions travel inside
/// the image, so an edited image states them again and admission reads
/// them again.
fn check_identity(image: &Image, identity: &ModuleIdentity) -> Result<(), ImageError> {
    if image.format != FORMAT_VERSION {
        return fail(
            ImageReason::Version,
            format!(
                "the image states format version {} and this build admits {FORMAT_VERSION}",
                image.format
            ),
        );
    }
    if image.abi_version != lm_abi::ABI_VERSION
        || image.compiler_abi != COMPILER_ABI_VERSION
        || image.verifier_version != lm_verify::VERIFIER_VERSION
    {
        return fail(
            ImageReason::Version,
            "the image names another ABI, compiler, or verifier version",
        );
    }
    if image.module_semantic != identity.semantic_hash {
        return fail(
            ImageReason::Code,
            "the image names another program than the loaded one",
        );
    }
    Ok(())
}

/// The pending `(machine, object, resolved type)` triples of the graph
/// walk.
type Work = Vec<(u32, u32, u32)>;

/// The state of one admission pass.
struct Admit<'m> {
    image: &'m Image,
    module: &'m lm_bytecode::Module,
    identity: &'m ModuleIdentity,
    types: ResolvedTypes<'m>,
    /// The module type slot of every semantic type digest.
    ///
    /// A snapshot names a type by digest, because a numeric type slot
    /// belongs to one linked program.
    types_by_digest: HashMap<[u8; 32], u32>,
    /// The resolved result type of every captured machine.
    result_of: Vec<Option<u32>>,
    /// The resolved mailbox message type of every captured proc.
    mailbox_of: Vec<Option<u32>>,
    /// The resolved slot types of every frame of every machine.
    frames_of: Vec<Vec<FrameSlots>>,
}

impl Admit<'_> {
    fn run(&mut self, budget: &mut AdmissionBudget) -> Result<(), ImageError> {
        if self.image.machines.is_empty() {
            return fail(ImageReason::State, "a snapshot world holds no machine");
        }
        self.check_code_manifest()?;
        self.check_root_result_type()?;
        for vm in 0..self.image.machines.len() {
            self.check_references(vm as u32)?;
            self.check_state(vm as u32)?;
        }
        self.check_parent_forest()?;
        self.check_world()?;
        self.resolve_frames()?;
        self.resolve_relations();
        let mut work: Work = Vec::new();
        for vm in 0..self.image.machines.len() {
            self.check_types(vm as u32, budget, &mut work)?;
        }
        self.walk(budget, &mut work)?;
        // The canonical order runs last. Every earlier rule states a
        // property of one position, so a diagnostic names that
        // position instead of the traversal an edit moved.
        for (vm, machine) in self.image.machines.iter().enumerate() {
            self.check_order(machine, vm as u32)?;
        }
        Ok(())
    }

    fn machine(&self, vm: u32) -> &ImageMachine {
        &self.image.machines[vm as usize]
    }

    /// The resolved type entry of one universe index.
    fn ty(&self, idx: u32) -> Result<BcType, ImageError> {
        self.types.ty(idx).ok_or_else(|| {
            ImageError::admission(ImageReason::Type, "a resolved type left the type universe")
        })
    }

    // ----------------------------------------------------------
    // Structural resolution.
    // ----------------------------------------------------------

    /// Every named function and class exists and carries its verified
    /// definition hash.
    fn check_code_manifest(&self) -> Result<(), ImageError> {
        let mut last: Option<u32> = None;
        for (slot, hash) in &self.image.funcs {
            if *slot as usize >= self.module.funcs.len() {
                return fail(
                    ImageReason::Code,
                    format!("the image names function slot {slot}, which the program has not"),
                );
            }
            if last.is_some_and(|l| *slot <= l) {
                return fail(ImageReason::Code, "the function manifest is not ascending");
            }
            last = Some(*slot);
            if self.identity.func_hashes[*slot as usize] != *hash {
                return fail(
                    ImageReason::Code,
                    format!("function slot {slot} carries another definition hash"),
                );
            }
        }
        let mut last: Option<u32> = None;
        for (slot, hash) in &self.image.classes {
            if *slot as usize >= self.module.classes.len() {
                return fail(
                    ImageReason::Code,
                    format!("the image names class slot {slot}, which the program has not"),
                );
            }
            if last.is_some_and(|l| *slot <= l) {
                return fail(ImageReason::Code, "the class manifest is not ascending");
            }
            last = Some(*slot);
            if self.identity.class_hashes[*slot as usize] != *hash {
                return fail(
                    ImageReason::Code,
                    format!("class slot {slot} carries another definition hash"),
                );
            }
        }
        Ok(())
    }

    fn func_named(&self, slot: u32) -> bool {
        self.image
            .funcs
            .binary_search_by_key(&slot, |(s, _)| *s)
            .is_ok()
    }

    fn class_named(&self, slot: u32) -> bool {
        self.image
            .classes
            .binary_search_by_key(&slot, |(s, _)| *s)
            .is_ok()
    }

    /// The header names the result type of the root machine, and the
    /// machine record names it again. One image states one type.
    fn check_root_result_type(&self) -> Result<(), ImageError> {
        let root = self.image.machines[0].result_type.unwrap_or([0u8; 32]);
        if root != self.image.result_type {
            return fail(
                ImageReason::State,
                "the header and the root machine name two result types",
            );
        }
        Ok(())
    }

    /// Every ordinal of one machine names an entry that exists.
    ///
    /// The decoder stores references as data, and an editor can write
    /// any ordinal, so admission proves every one of them before any
    /// later rule follows a reference.
    fn check_references(&self, vm: u32) -> Result<(), ImageError> {
        let m = self.machine(vm);
        let objects = m.objects.len() as u32;
        let machines = self.image.machines.len() as u32;
        let at = |what: &str| format!("machine {vm}: {what}");
        let object_ref = |value: &Value, what: &str| -> Result<(), ImageError> {
            match value {
                Value::Obj(r) if r.slot >= objects => fail(
                    ImageReason::Reference,
                    at(&format!(
                        "{what} names object ordinal {} of {objects}",
                        r.slot
                    )),
                ),
                _ => Ok(()),
            }
        };
        if let Some(parent) = m.parent {
            if parent >= machines {
                return fail(
                    ImageReason::Reference,
                    at("the parent ordinal names no captured machine"),
                );
            }
        }
        for (ordinal, entry) in m.objects.iter().enumerate() {
            let mut children = Vec::new();
            entry.object.children(&mut children);
            for child in &children {
                if child.slot >= objects {
                    return fail(
                        ImageReason::Reference,
                        at(&format!(
                            "object {ordinal} names object ordinal {} of {objects}",
                            child.slot
                        )),
                    );
                }
            }
            let target = match entry.object {
                Object::NativeVm { vm } | Object::NativeTable { vm } => Some(vm),
                Object::NativeRequest { vm, .. } | Object::NativeCall { vm, .. } => Some(vm),
                Object::NativeHandle { proc, .. } => Some(proc),
                _ => None,
            };
            if let Some(target) = target {
                if target >= machines {
                    return fail(
                        ImageReason::Reference,
                        at(&format!(
                            "object {ordinal} names machine ordinal {target} of {machines}"
                        )),
                    );
                }
            }
            // The shape table fixes the frozen state of a born-frozen
            // object, so a mutable one of that shape is not a state
            // the runtime can hold.
            if entry.object.shape().born_frozen && !entry.frozen {
                return fail(
                    ImageReason::State,
                    at(&format!(
                        "object {ordinal} is a {} without the frozen bit",
                        entry.object.shape().name
                    )),
                );
            }
            match &entry.object {
                Object::Instance { class, fields } => {
                    if *class as usize >= self.module.classes.len() || !self.class_named(*class) {
                        return fail(
                            ImageReason::Code,
                            at(&format!(
                                "object {ordinal} names class slot {class}, which the manifest \
                                 omits"
                            )),
                        );
                    }
                    let want = self.module.classes[*class as usize].fields.len();
                    if fields.len() != want {
                        return fail(
                            ImageReason::Layout,
                            at(&format!(
                                "object {ordinal} holds {} fields and the layout of class \
                                 {class} has {want}",
                                fields.len()
                            )),
                        );
                    }
                }
                Object::Closure { func, captures } => {
                    if *func as usize >= self.module.funcs.len() || !self.func_named(*func) {
                        return fail(
                            ImageReason::Code,
                            at(&format!(
                                "object {ordinal} names function slot {func}, which the manifest \
                                 omits"
                            )),
                        );
                    }
                    let want = self.module.funcs[*func as usize].captures.len();
                    if captures.len() != want {
                        return fail(
                            ImageReason::Layout,
                            at(&format!(
                                "object {ordinal} holds {} captures and function {func} declares \
                                 {want}",
                                captures.len()
                            )),
                        );
                    }
                }
                _ => {}
            }
        }
        for (idx, frame) in m.frames.iter().enumerate() {
            if frame.func as usize >= self.module.funcs.len() || !self.func_named(frame.func) {
                return fail(
                    ImageReason::Code,
                    at(&format!(
                        "frame {idx} names function slot {}, which the manifest omits",
                        frame.func
                    )),
                );
            }
            let code = &self.module.funcs[frame.func as usize];
            if frame.block as usize >= code.blocks.len() {
                return fail(
                    ImageReason::Layout,
                    at(&format!(
                        "frame {idx} names block {}, which its function has not",
                        frame.block
                    )),
                );
            }
            // A machine stops between instructions, so the program
            // counter names the next instruction of the block. Every
            // block ends with a terminator, so it never reaches the
            // end.
            if frame.ip as usize >= code.blocks[frame.block as usize].len() {
                return fail(
                    ImageReason::Layout,
                    at(&format!(
                        "frame {idx} holds a program counter past its block"
                    )),
                );
            }
            if let Some(closure) = frame.closure {
                if closure >= objects {
                    return fail(
                        ImageReason::Reference,
                        at(&format!("frame {idx} names no capture context object")),
                    );
                }
            }
        }
        for (idx, value) in m.locals.iter().enumerate() {
            object_ref(value, &format!("local {idx}"))?;
        }
        for (idx, value) in m.operands.iter().enumerate() {
            object_ref(value, &format!("operand {idx}"))?;
        }
        if let Some(pending) = &m.pending {
            if pending.op >= lm_abi::OP_COUNT {
                return fail(
                    ImageReason::Code,
                    at("the pending request names no manifest operation"),
                );
            }
            for (idx, value) in pending.args.iter().enumerate() {
                object_ref(value, &format!("pending argument {idx}"))?;
            }
        }
        if let Some(ImageTerminal::Done(value)) = &m.terminal {
            object_ref(value, "the terminal value")?;
        }
        for (idx, value) in m.mailbox.queue.iter().enumerate() {
            object_ref(value, &format!("mailbox message {idx}"))?;
        }
        if m.literals.len() > self.module.strings.len() {
            return fail(
                ImageReason::Reference,
                at("the literal table is longer than the module string pool"),
            );
        }
        for (idx, literal) in m.literals.iter().enumerate() {
            let Some(ordinal) = literal else { continue };
            if *ordinal >= objects {
                return fail(
                    ImageReason::Reference,
                    at(&format!("literal {idx} names no object")),
                );
            }
            match &m.objects[*ordinal as usize].object {
                Object::Str(text) if *text == self.module.strings[idx] => {}
                _ => {
                    return fail(
                        ImageReason::Reference,
                        at(&format!("literal {idx} does not hold its pooled string")),
                    )
                }
            }
        }
        if let Some(body) = m.start_body {
            if body >= objects {
                return fail(ImageReason::Reference, at("the proc body names no object"));
            }
        }
        if let Some(ImageBlock::Send { target } | ImageBlock::Done { target }) = m.block {
            if target >= machines {
                return fail(
                    ImageReason::Reference,
                    at("a block names no captured machine"),
                );
            }
        }
        Ok(())
    }

    /// Prove the state rules of one captured machine.
    fn check_state(&self, vm: u32) -> Result<(), ImageError> {
        let m = self.machine(vm);
        let at = |what: &str| format!("machine {vm}: {what}");
        // The frame chain. Local bases follow the declared local counts
        // exactly, and the arenas end where the last frame ends.
        let mut want_local = 0u64;
        let mut last_operand = 0u64;
        for (idx, frame) in m.frames.iter().enumerate() {
            if frame.base_local as u64 != want_local {
                return fail(
                    ImageReason::Layout,
                    at(&format!("frame {idx} does not start at its local base")),
                );
            }
            if (frame.base_operand as u64) < last_operand {
                return fail(
                    ImageReason::Layout,
                    at(&format!("frame {idx} lowers the operand base")),
                );
            }
            last_operand = frame.base_operand as u64;
            want_local += self.module.funcs[frame.func as usize].local_count() as u64;
        }
        if m.locals.len() as u64 != want_local {
            return fail(
                ImageReason::Layout,
                at("the local arena does not match the frame chain"),
            );
        }
        if (m.operands.len() as u64) < last_operand {
            return fail(
                ImageReason::Layout,
                at("the operand arena ends below the last frame base"),
            );
        }
        if m.locals.len() + m.operands.len() > m.limits.max_stack_values as usize {
            return fail(
                ImageReason::Layout,
                at("the arenas together pass the declared stack limit"),
            );
        }
        if m.frames.len() > m.limits.max_frames as usize {
            return fail(ImageReason::Layout, at("the frame count passes its limit"));
        }
        // Operands belong to frames. A machine with no frame therefore
        // carries no operand, so a frameless operand arena holds values
        // the operand proof never reaches. Reject it rather than leave
        // it unproven.
        if m.frames.is_empty() && !m.operands.is_empty() {
            return fail(
                ImageReason::Layout,
                at("a machine with no frame holds operands"),
            );
        }
        if let Some(body) = m.start_body {
            if !matches!(m.objects[body as usize].object, Object::Closure { .. }) {
                return fail(ImageReason::Reference, at("the proc body is not a closure"));
            }
        }
        // The capture context of a frame is the closure the frame runs,
        // so it names exactly the function of that frame.
        for (idx, frame) in m.frames.iter().enumerate() {
            let Some(closure) = frame.closure else {
                continue;
            };
            match m.objects[closure as usize].object {
                Object::Closure { func, .. } if func == frame.func => {}
                _ => {
                    return fail(
                        ImageReason::Reference,
                        at(&format!(
                            "frame {idx} names a capture context that is not its own closure"
                        )),
                    )
                }
            }
        }
        // The state rules of specification 14.3 and 17.6.
        match m.state {
            ImageState::Empty => {
                if !m.frames.is_empty() || m.pending.is_some() || m.terminal.is_some() {
                    return fail(
                        ImageReason::State,
                        at("an empty machine holds execution state"),
                    );
                }
            }
            ImageState::Ready => {
                if m.frames.is_empty() {
                    return fail(ImageReason::State, at("a ready machine holds no frame"));
                }
                if m.pending.is_some() {
                    return fail(
                        ImageReason::State,
                        at("a ready machine holds a pending request"),
                    );
                }
                if m.terminal.is_some() {
                    return fail(
                        ImageReason::State,
                        at("a ready machine holds a terminal result"),
                    );
                }
            }
            ImageState::Asked | ImageState::Blocked => {
                if m.frames.is_empty() {
                    return fail(ImageReason::State, at("a stopped machine holds no frame"));
                }
                if m.pending.is_none() {
                    return fail(
                        ImageReason::State,
                        at("an asked or blocked machine holds no pending request"),
                    );
                }
                if m.terminal.is_some() {
                    return fail(
                        ImageReason::State,
                        at("an asked or blocked machine holds a terminal result"),
                    );
                }
            }
            ImageState::Done | ImageState::Faulted => {
                if m.pending.is_some() {
                    return fail(
                        ImageReason::State,
                        at("a terminal machine holds a pending request"),
                    );
                }
                // A machine reaches a terminal only by returning its
                // last frame, so a terminal machine holds none. The
                // frameless-operand rule above then forces its arenas
                // empty as well.
                if !m.frames.is_empty() {
                    return fail(ImageReason::State, at("a terminal machine holds a frame"));
                }
            }
        }
        match (&m.state, &m.terminal) {
            (ImageState::Done, Some(ImageTerminal::Done(_))) => {}
            (ImageState::Faulted, Some(ImageTerminal::Fault(_))) => {}
            (ImageState::Done | ImageState::Faulted, _) => {
                return fail(
                    ImageReason::State,
                    at("a terminal machine does not store its result"),
                )
            }
            (_, Some(_)) => {
                return fail(
                    ImageReason::State,
                    at("a live machine stores a terminal result"),
                )
            }
            _ => {}
        }
        // A block record exists exactly when the machine is blocked,
        // and its kind matches the pending proc operation.
        match (m.state, m.block) {
            (ImageState::Blocked, Some(block)) => {
                let op = m
                    .pending
                    .as_ref()
                    .expect("a blocked machine has a request")
                    .op;
                let ok = match block {
                    ImageBlock::Receive => op == lm_abi::OP_PROC_RECV,
                    ImageBlock::Send { .. } => op == lm_abi::OP_PROC_SEND,
                    ImageBlock::Done { .. } => op == lm_abi::OP_PROC_DONE,
                };
                if !ok {
                    return fail(
                        ImageReason::State,
                        at("the block record does not match the pending operation"),
                    );
                }
            }
            (ImageState::Blocked, None) => {
                return fail(
                    ImageReason::State,
                    at("a blocked machine holds no block record"),
                )
            }
            (_, Some(_)) => {
                return fail(
                    ImageReason::State,
                    at("a machine that is not blocked holds a block record"),
                )
            }
            _ => {}
        }
        // The pending request names a legal operation for this state.
        if let Some(pending) = &m.pending {
            if lm_abi::op(pending.op).suspends() {
                return fail(
                    ImageReason::State,
                    at("a pending host attachment has no bytes to copy"),
                );
            }
            if pending.ordinal >= m.next_ordinal {
                return fail(
                    ImageReason::State,
                    at("the pending request ordinal is not below the next ordinal"),
                );
            }
        }
        // The mailbox rules of specification 18.5.
        if m.mailbox.queue.len() > m.mailbox.limit as usize {
            return fail(
                ImageReason::Mailbox,
                at("the accepted queue is longer than the mailbox limit"),
            );
        }
        // Only a proc holds an accepted message. A non-proc machine
        // keeps a closed empty mailbox, so a queued message on one has
        // no mailbox type to prove against, and it would sit unchecked.
        if !m.is_proc && !m.mailbox.queue.is_empty() {
            return fail(
                ImageReason::Mailbox,
                at("a machine that is not a proc holds an accepted message"),
            );
        }
        if m.mailbox.delivered > m.mailbox.accepted {
            return fail(
                ImageReason::Mailbox,
                at("the mailbox delivered more messages than it accepted"),
            );
        }
        // The world gate and the paused state.
        if m.paused && m.scheduler_owned {
            return fail(
                ImageReason::State,
                at("a paused proc is not scheduler-owned"),
            );
        }
        if vm == 0 && (m.scheduler_owned || m.paused) {
            return fail(
                ImageReason::State,
                at("the restored root is holder-controlled"),
            );
        }
        Ok(())
    }

    /// Prove that the parent graph is a forest.
    ///
    /// Every machine names at most one parent, so the parent pointers
    /// form a functional graph. A cycle in it makes the runtime policy
    /// walk of `resolve_policy` loop forever, because that walk follows
    /// the parent chain with no bound. The walk below is iterative, so
    /// it never grows the Rust stack.
    fn check_parent_forest(&self) -> Result<(), ImageError> {
        let n = self.image.machines.len();
        // 0 unvisited, 1 on the current path, 2 settled.
        let mut colour = vec![0u8; n];
        for start in 0..n {
            if colour[start] != 0 {
                continue;
            }
            let mut path: Vec<usize> = Vec::new();
            let mut cur = start;
            loop {
                match colour[cur] {
                    0 => {
                        colour[cur] = 1;
                        path.push(cur);
                        match self.image.machines[cur].parent {
                            Some(parent) => {
                                let parent = parent as usize;
                                if parent == cur {
                                    return fail(
                                        ImageReason::State,
                                        format!("machine {cur} is its own parent"),
                                    );
                                }
                                cur = parent;
                            }
                            None => break,
                        }
                    }
                    1 => {
                        return fail(
                            ImageReason::State,
                            format!("the parent chain through machine {cur} forms a cycle"),
                        );
                    }
                    _ => break,
                }
            }
            for node in path {
                colour[node] = 2;
            }
        }
        Ok(())
    }

    /// Prove the structural rules that need the whole world.
    fn check_world(&self) -> Result<(), ImageError> {
        for (vm, machine) in self.image.machines.iter().enumerate() {
            for (ordinal, entry) in machine.objects.iter().enumerate() {
                // Every handle names a captured machine at its
                // generation.
                if let Object::NativeHandle { proc, generation } = entry.object {
                    let target = &self.image.machines[proc as usize];
                    if target.generation != generation {
                        return fail(
                            ImageReason::Reference,
                            format!(
                                "machine {vm} object {ordinal} names machine {proc} at generation \
                                 {generation}, and that machine holds {}",
                                target.generation
                            ),
                        );
                    }
                }
                // A request or call token names a machine that holds
                // exactly that pending request.
                let (target, request, op) = match entry.object {
                    Object::NativeRequest { vm, ordinal } => (vm, ordinal, None),
                    Object::NativeCall { vm, ordinal, op } => (vm, ordinal, Some(op)),
                    _ => continue,
                };
                let target = &self.image.machines[target as usize];
                // A stale token is legal: the machine answered the
                // request already. The rule is that a live token
                // agrees.
                if target.state == ImageState::Asked {
                    let pending = target
                        .pending
                        .as_ref()
                        .expect("an asked machine holds its request");
                    if pending.ordinal == request && op.is_some_and(|op| op != pending.op) {
                        return fail(
                            ImageReason::Reference,
                            format!(
                                "machine {vm} object {ordinal} names another operation than the \
                                 pending request it points at"
                            ),
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Prove that the stored heap is the canonical traversal of its
    /// roots.
    ///
    /// The walk is iterative, so a deep image never grows the Rust
    /// stack. The check also proves that every stored object is
    /// reachable and that no object is missing.
    fn check_order(&self, machine: &ImageMachine, vm: u32) -> Result<(), ImageError> {
        let count = machine.objects.len();
        let mut seen = vec![false; count];
        let mut next = 0usize;
        let roots = image_roots(machine);
        let mut stack: Vec<u32> = roots.iter().rev().copied().collect();
        let mut children: Vec<lm_value::ObjRef> = Vec::new();
        while let Some(r) = stack.pop() {
            let idx = r as usize;
            if seen[idx] {
                continue;
            }
            if idx != next {
                return fail(
                    ImageReason::Order,
                    format!(
                        "machine {vm}: the traversal reaches object {idx} where the canonical \
                         order needs {next}"
                    ),
                );
            }
            seen[idx] = true;
            next += 1;
            children.clear();
            machine.objects[idx].object.children(&mut children);
            stack.extend(children.iter().rev().map(|c| c.slot));
        }
        if next != count {
            return fail(
                ImageReason::Order,
                format!(
                    "machine {vm}: {} stored objects are unreachable",
                    count - next
                ),
            );
        }
        Ok(())
    }

    // ----------------------------------------------------------
    // Resolved types.
    // ----------------------------------------------------------

    /// Resolve the slot types of every frame of every machine.
    ///
    /// The verifier proves a generic body once, with its type
    /// variables opaque. The substitution of one activation comes from
    /// the call site the frame below stopped inside, so the whole
    /// chain resolves from its bottom frame upward.
    fn resolve_frames(&mut self) -> Result<(), ImageError> {
        let mut out: Vec<Vec<FrameSlots>> = Vec::with_capacity(self.image.machines.len());
        for (vm, machine) in self.image.machines.iter().enumerate() {
            let points: Vec<FramePoint> = machine
                .frames
                .iter()
                .enumerate()
                .map(|(idx, frame)| FramePoint {
                    func: frame.func,
                    block: frame.block,
                    ip: frame.ip,
                    // The top frame of a machine with no pending
                    // request stopped before the instruction its
                    // counter names. Every other frame stopped inside
                    // the instruction before it.
                    before_counter: idx + 1 == machine.frames.len() && machine.pending.is_none(),
                })
                .collect();
            let slots = self.types.resolve_chain(&points).map_err(|e| {
                ImageError::admission(ImageReason::Type, format!("machine {vm}: {e}"))
            })?;
            out.push(slots);
        }
        self.frames_of = out;
        Ok(())
    }

    /// Resolve the result type and the mailbox type of every machine.
    ///
    /// A machine handle, a proc handle, and a call token take their
    /// type from the target they name, and a target can sit anywhere in
    /// the world. This pass therefore runs before the graph walk, so a
    /// handle can name a machine the walk has not reached
    /// (specification section 5.5).
    fn resolve_relations(&mut self) {
        let mut result_of: Vec<Option<u32>> = Vec::with_capacity(self.image.machines.len());
        let mut mailbox_of: Vec<Option<u32>> = Vec::with_capacity(self.image.machines.len());
        for machine in self.image.machines.iter() {
            result_of.push(self.machine_result_type(machine));
            mailbox_of.push(self.mailbox_type(machine));
        }
        self.result_of = result_of;
        self.mailbox_of = mailbox_of;
    }

    /// The declared terminal result type of one captured machine.
    ///
    /// The body function of a machine states that type. A proc keeps
    /// its body closure, and `Proc.Spawn` types its handle from exactly
    /// that closure, so the two can never disagree. A machine that
    /// loaded a body without keeping the closure records the type as a
    /// digest instead.
    ///
    /// The result type of a proc is not the recorded digest: a proc
    /// runs its constructor first, and the record then names the proc
    /// instance type.
    fn machine_result_type(&self, machine: &ImageMachine) -> Option<u32> {
        if let Some(ordinal) = machine.start_body {
            if let Object::Closure { func, .. } = machine.objects.get(ordinal as usize)?.object {
                let ret = self.types.result_type(func)?;
                return self.types.is_resolved(ret).then_some(ret);
            }
        }
        machine
            .result_type
            .and_then(|digest| self.types_by_digest.get(&digest).copied())
    }

    /// The mailbox message type of one captured proc.
    ///
    /// The proc class fixes the type. The stored proc body declares the
    /// proc instance as its first parameter, and the entry frame
    /// declares it after the constructor returns. `None` means the type
    /// does not follow from the image, and a proc that carries a
    /// message then has no governing type.
    fn mailbox_type(&self, machine: &ImageMachine) -> Option<u32> {
        let func = match machine.start_body {
            Some(ordinal) => match machine.objects.get(ordinal as usize)?.object {
                Object::Closure { func, .. } => Some(func),
                _ => None,
            },
            None => machine.frames.first().map(|f| f.func),
        }?;
        let instance = *self.types.params(func)?.first()?;
        self.types.proc_mailbox(instance)
    }

    /// The semantic type digest of one resolved type.
    ///
    /// A digest exists for a module type entry alone. A type the
    /// verifier created by substitution has no serialized name, so a
    /// position that needs one has no evidence.
    fn type_digest(&self, ty: u32) -> Option<[u8; 32]> {
        self.identity.type_hashes.get(ty as usize).copied()
    }

    // ----------------------------------------------------------
    // Type accuracy.
    // ----------------------------------------------------------

    /// Seed the graph walk from every typed position of one machine.
    fn check_types(
        &self,
        vm: u32,
        budget: &mut AdmissionBudget,
        work: &mut Work,
    ) -> Result<(), ImageError> {
        let machine = self.machine(vm);
        let at = |what: &str| format!("machine {vm}: {what}");
        // Every local slot. A slot the verifier proves initialized
        // holds a value of its proved type. A slot it proves
        // uninitialized holds the uninitialized marker, or a value one
        // path of a merge left behind; the declared slot type bounds
        // that value, and no verified read reaches it.
        for (idx, frame) in machine.frames.iter().enumerate() {
            let slots = &self.frames_of[vm as usize][idx];
            budget.charge(slots.locals.len() as u64)?;
            for (slot, proved) in slots.locals.iter().enumerate() {
                let position = frame.base_local as usize + slot;
                let Some(value) = machine.locals.get(position) else {
                    return fail(
                        ImageReason::Layout,
                        at("the local arena is shorter than the frame chain"),
                    );
                };
                match proved {
                    Some(ty) => {
                        self.check_value(vm, *value, *ty, &at(&format!("local {slot}")), work)?;
                    }
                    None => {
                        if *value != Value::Uninit {
                            self.check_value(
                                vm,
                                *value,
                                slots.declared[slot],
                                &at(&format!("local {slot}")),
                                work,
                            )?;
                        }
                    }
                }
            }
        }
        self.check_operands(vm, budget, work)?;
        self.check_pending_args(vm, budget, work)?;
        self.check_terminal(vm, work)?;
        self.check_mailbox(vm, budget, work)?;
        // A capture context is the closure the frame runs, so the walk
        // proves it at the function type of that frame.
        for frame in &machine.frames {
            let Some(closure) = frame.closure else {
                continue;
            };
            let ty = self.types.fn_type(frame.func).ok_or_else(|| {
                ImageError::admission(
                    ImageReason::Type,
                    at("a frame function has no function type"),
                )
            })?;
            work.push((vm, closure, ty));
        }
        // The proc body is a closure the runtime calls with the proc
        // instance. Its own function type proves its captures.
        if let Some(body) = machine.start_body {
            if let Object::Closure { func, .. } = machine.objects[body as usize].object {
                let ty = self.types.fn_type(func).ok_or_else(|| {
                    ImageError::admission(
                        ImageReason::Type,
                        at("the proc body has no function type"),
                    )
                })?;
                work.push((vm, body, ty));
            }
        }
        Ok(())
    }

    /// Prove the operand types of every stopped frame.
    ///
    /// A frame stops at one of two points. The top frame of a machine
    /// with no pending request stops before the instruction its program
    /// counter names. Every other frame, and the top frame of a machine
    /// with a pending request, stopped inside the instruction before
    /// the counter: a call moved its arguments into the callee locals,
    /// and a perform moved them into the pending record. In both cases
    /// the retained operands are the bottom of the stack the verifier
    /// proved before that instruction.
    ///
    /// A terminal machine holds no frame, so the rule reaches nothing
    /// there.
    fn check_operands(
        &self,
        vm: u32,
        budget: &mut AdmissionBudget,
        work: &mut Work,
    ) -> Result<(), ImageError> {
        let machine = self.machine(vm);
        for (idx, frame) in machine.frames.iter().enumerate() {
            let top = idx + 1 == machine.frames.len();
            // The operand region this frame retains.
            let end = match machine.frames.get(idx + 1) {
                Some(next) => next.base_operand as usize,
                None => machine.operands.len(),
            };
            let start = frame.base_operand as usize;
            if end < start || end > machine.operands.len() {
                return fail(
                    ImageReason::Layout,
                    format!("machine {vm}: frame {idx} owns no operand region"),
                );
            }
            let types = &self.frames_of[vm as usize][idx].operands;
            let want = end - start;
            let stopped_before_the_counter = top && machine.pending.is_none();
            if types.len() < want || (stopped_before_the_counter && types.len() != want) {
                return fail(
                    ImageReason::Layout,
                    format!(
                        "machine {vm}: frame {idx} holds {want} operands and the program point \
                         proves {}",
                        types.len()
                    ),
                );
            }
            budget.charge(want as u64)?;
            for (offset, ty) in types.iter().take(want).enumerate() {
                self.check_value(
                    vm,
                    machine.operands[start + offset],
                    *ty,
                    &format!("machine {vm}: frame {idx} operand {offset}"),
                    work,
                )?;
            }
        }
        Ok(())
    }

    /// Prove the argument types of one pending perform.
    ///
    /// A perform pops its arguments off the operand stack into the
    /// pending record, so the top frame stopped inside the perform. The
    /// stack the verifier proved just before the perform is the
    /// retained operands at the bottom, which `check_operands` already
    /// proved, and the popped arguments at the top. This rule proves
    /// the top.
    ///
    /// The count is load-bearing: the number of operands the perform
    /// consumed is fixed by the operation, so a stack that is not the
    /// retained operands plus the recorded arguments does not agree
    /// with the proved program point. The rule reads no manifest
    /// parameter type, so it holds for a machine control operation as
    /// well as a fixed one.
    fn check_pending_args(
        &self,
        vm: u32,
        budget: &mut AdmissionBudget,
        work: &mut Work,
    ) -> Result<(), ImageError> {
        let machine = self.machine(vm);
        let Some(pending) = &machine.pending else {
            return Ok(());
        };
        let Some(top) = machine.frames.last() else {
            return fail(
                ImageReason::State,
                format!("machine {vm}: a pending request holds no frame"),
            );
        };
        let types = &self.frames_of[vm as usize][machine.frames.len() - 1].operands;
        let retained = machine
            .operands
            .len()
            .checked_sub(top.base_operand as usize)
            .ok_or_else(|| {
                ImageError::admission(
                    ImageReason::Layout,
                    format!("machine {vm}: the pending frame owns no operand region"),
                )
            })?;
        let argc = pending.args.len();
        // The proved stack is exactly the retained operands plus the
        // recorded arguments.
        if types.len() != retained + argc {
            return fail(
                ImageReason::State,
                format!(
                    "machine {vm}: the pending request holds {argc} arguments and the program \
                     point proves {}",
                    types.len().saturating_sub(retained)
                ),
            );
        }
        budget.charge(argc as u64)?;
        for (offset, ty) in types.iter().skip(retained).enumerate() {
            self.check_value(
                vm,
                pending.args[offset],
                *ty,
                &format!("machine {vm}: pending argument {offset}"),
                work,
            )?;
        }
        Ok(())
    }

    /// A stored terminal value carries the exact declared result type
    /// of its machine.
    ///
    /// A terminal machine keeps no frame, so the recorded digest is the
    /// only record of that type. The unit value takes no exception: a
    /// consumer reads the stored value at the declared result type,
    /// whatever that type is.
    fn check_terminal(&self, vm: u32, work: &mut Work) -> Result<(), ImageError> {
        let machine = self.machine(vm);
        let Some(ImageTerminal::Done(value)) = &machine.terminal else {
            return Ok(());
        };
        // A terminal machine keeps no frame, so the recorded digest is
        // the one record of the type its stored result carries.
        let Some(digest) = machine.result_type else {
            return fail(
                ImageReason::State,
                format!("machine {vm}: a terminal value carries no result type to prove"),
            );
        };
        let Some(ty) = self.types_by_digest.get(&digest).copied() else {
            return fail(
                ImageReason::Code,
                format!("machine {vm}: the result type names no type of this program"),
            );
        };
        self.check_value(
            vm,
            *value,
            ty,
            &format!("machine {vm}: the terminal value"),
            work,
        )
    }

    /// Every accepted message carries the mailbox type of its proc.
    ///
    /// The class table fixes that type, so the rule never reads it from
    /// the image. A proc that carries a message and has no derivable
    /// mailbox type holds values with no governing type, so admission
    /// rejects it rather than schedule an unproven message.
    fn check_mailbox(
        &self,
        vm: u32,
        budget: &mut AdmissionBudget,
        work: &mut Work,
    ) -> Result<(), ImageError> {
        let machine = self.machine(vm);
        if machine.mailbox.queue.is_empty() {
            return Ok(());
        }
        let Some(message) = self.mailbox_of[vm as usize] else {
            return fail(
                ImageReason::Mailbox,
                format!(
                    "machine {vm}: a proc whose mailbox type cannot be proven holds an accepted \
                     message"
                ),
            );
        };
        budget.charge(machine.mailbox.queue.len() as u64)?;
        for (idx, value) in machine.mailbox.queue.iter().enumerate() {
            self.check_value(
                vm,
                *value,
                message,
                &format!("machine {vm}: accepted message {idx}"),
                work,
            )?;
        }
        Ok(())
    }

    /// Check one value against one resolved type.
    ///
    /// A heap value joins the graph walk at that type, so one object is
    /// proved under every type that reaches it.
    fn check_value(
        &self,
        vm: u32,
        value: Value,
        ty: u32,
        what: &str,
        work: &mut Work,
    ) -> Result<(), ImageError> {
        let declared = self.ty(ty)?;
        let wrong = |found: &str| -> Result<(), ImageError> {
            fail(
                ImageReason::Layout,
                format!("{what} holds {found} where another type is declared"),
            )
        };
        match value {
            // The uninitialized marker is not a value. It is legal
            // where the verifier or the layout proves that no value
            // exists, and every one of those positions handles it
            // before this call.
            Value::Uninit => wrong("the uninitialized marker"),
            Value::Unit => match declared {
                BcType::Unit => Ok(()),
                _ => wrong("the unit value"),
            },
            Value::Bool(_) => match declared {
                BcType::Bool => Ok(()),
                _ => wrong("a boolean"),
            },
            Value::Int(_) => match declared {
                BcType::Int => Ok(()),
                _ => wrong("an integer"),
            },
            Value::Op(slot) => match declared {
                BcType::Op(op, _) if op == slot => Ok(()),
                _ => wrong("an operation value"),
            },
            Value::Obj(r) => {
                if r.slot as usize >= self.machine(vm).objects.len() {
                    return fail(
                        ImageReason::Reference,
                        format!("{what} names no object of its machine"),
                    );
                }
                work.push((vm, r.slot, ty));
                Ok(())
            }
        }
    }

    /// Walk every `(machine, object, resolved type)` triple the typed
    /// positions reach.
    ///
    /// The walk is iterative and bounded. One object can require a
    /// proof under several types, so the visited key carries the type.
    fn walk(&self, budget: &mut AdmissionBudget, work: &mut Work) -> Result<(), ImageError> {
        let mut visited: HashSet<(u32, u32, u32)> = HashSet::new();
        while let Some((vm, object, ty)) = work.pop() {
            if !visited.insert((vm, object, ty)) {
                continue;
            }
            budget.charge(1)?;
            self.check_object(vm, object, ty, work)?;
        }
        Ok(())
    }

    /// Prove one object against one resolved type.
    #[allow(clippy::too_many_lines)]
    fn check_object(
        &self,
        vm: u32,
        object: u32,
        ty: u32,
        work: &mut Work,
    ) -> Result<(), ImageError> {
        let declared = self.ty(ty)?;
        let payload = &self.image.machines[vm as usize].objects[object as usize].object;
        let at = format!("machine {vm} object {object}");
        let wrong = |what: &str| -> Result<(), ImageError> {
            fail(
                ImageReason::Layout,
                format!("{at} is a {what} where another type is declared"),
            )
        };
        let plain = |ok: bool| -> Result<(), ImageError> {
            if ok {
                Ok(())
            } else {
                wrong(payload.shape().name)
            }
        };
        match declared {
            BcType::Str => plain(matches!(payload, Object::Str(_))),
            BcType::StringBuilder => plain(matches!(payload, Object::StrBuilder(_))),
            BcType::ByteBuffer => plain(matches!(payload, Object::ByteBuf(_))),
            BcType::Fault => plain(matches!(payload, Object::NativeFault { .. })),
            BcType::Request => plain(matches!(payload, Object::NativeRequest { .. })),
            BcType::PolicyTable => plain(matches!(payload, Object::NativeTable { .. })),
            BcType::Digest => plain(matches!(payload, Object::NativeDigest(_))),
            // An empty machine handle names a machine with no loaded
            // program. The lifecycle state is part of the type.
            BcType::EmptyVm => match payload {
                Object::NativeVm { vm: target } => {
                    if self.image.machines[*target as usize].state == ImageState::Empty {
                        Ok(())
                    } else {
                        fail(
                            ImageReason::Layout,
                            format!("{at} names machine {target}, which holds a loaded program"),
                        )
                    }
                }
                _ => wrong(payload.shape().name),
            },
            // A machine handle carries the result type of the machine
            // it names. The type resolves from the target, never from
            // the image.
            BcType::Vm(want) => match payload {
                Object::NativeVm { vm: target } => {
                    let Some(found) = self.result_of[*target as usize] else {
                        return fail(
                            ImageReason::Type,
                            format!("{at} names machine {target}, which records no result type"),
                        );
                    };
                    if self.types.is_subtype(found, want) {
                        Ok(())
                    } else {
                        fail(
                            ImageReason::Layout,
                            format!("{at} names machine {target}, which returns another type"),
                        )
                    }
                }
                _ => wrong(payload.shape().name),
            },
            // A proc handle carries the mailbox type and the result
            // type of the proc it names.
            BcType::Handle(message, result) => match payload {
                Object::NativeHandle { proc, .. } => {
                    let Some(found) = self.mailbox_of[*proc as usize] else {
                        return fail(
                            ImageReason::Type,
                            format!(
                                "{at} names machine {proc}, whose mailbox type is not provable"
                            ),
                        );
                    };
                    if found != message {
                        return fail(
                            ImageReason::Layout,
                            format!("{at} names machine {proc}, which accepts another type"),
                        );
                    }
                    let Some(returns) = self.result_of[*proc as usize] else {
                        return fail(
                            ImageReason::Type,
                            format!("{at} names machine {proc}, which records no result type"),
                        );
                    };
                    if self.types.is_subtype(returns, result) {
                        Ok(())
                    } else {
                        fail(
                            ImageReason::Layout,
                            format!("{at} names machine {proc}, which returns another type"),
                        )
                    }
                }
                _ => wrong(payload.shape().name),
            },
            // A call token carries the argument view and the reply type
            // of the exact operation it names.
            BcType::PendingCall(view, reply) => match payload {
                Object::NativeCall { op, .. } => {
                    let Some((want_view, want_reply)) = self.types.pending_call_types(*op) else {
                        return fail(
                            ImageReason::Type,
                            format!("{at} names an operation with no call type"),
                        );
                    };
                    if want_view == view && want_reply == reply {
                        Ok(())
                    } else {
                        fail(
                            ImageReason::Layout,
                            format!("{at} names an operation with another call type"),
                        )
                    }
                }
                _ => wrong(payload.shape().name),
            },
            // A nested snapshot stays opaque. Admission proves that its
            // container is well formed and that its declared root
            // result type matches. The nested body passes its own
            // admission at its own restore.
            BcType::SnapshotImage => match payload {
                Object::NativeSnapshot(bytes) => {
                    self.nested_result_type(bytes, &at)?;
                    Ok(())
                }
                _ => wrong(payload.shape().name),
            },
            BcType::Snapshot(want) => match payload {
                Object::NativeSnapshot(bytes) => {
                    let found = self.nested_result_type(bytes, &at)?;
                    let Some(digest) = self.type_digest(want) else {
                        return fail(
                            ImageReason::Type,
                            format!("{at} holds a snapshot type with no serialized name"),
                        );
                    };
                    if found == digest {
                        Ok(())
                    } else {
                        fail(
                            ImageReason::Layout,
                            format!("{at} holds a nested world with another root result type"),
                        )
                    }
                }
                _ => wrong(payload.shape().name),
            },
            // A closure carries the captures its function declares. The
            // declared function type must fit the position that reached
            // it, so the captures follow from the function alone.
            BcType::Fn(_, _, _, _) => match payload {
                Object::Closure { func, captures } => {
                    let declared_fn = self.types.fn_type(*func).ok_or_else(|| {
                        ImageError::admission(
                            ImageReason::Type,
                            format!("{at} names a function with no function type"),
                        )
                    })?;
                    if !self.types.is_subtype(declared_fn, ty) {
                        return fail(
                            ImageReason::Layout,
                            format!("{at} is a closure of another function type"),
                        );
                    }
                    let types = self.types.captures(*func).ok_or_else(|| {
                        ImageError::admission(
                            ImageReason::Type,
                            format!("{at} names a function with no capture list"),
                        )
                    })?;
                    for (idx, (value, capture)) in captures.iter().zip(types.iter()).enumerate() {
                        // A capture type of a closure a generic body
                        // created still holds that body's variables,
                        // and the closure value carries no
                        // substitution. Admission has no evidence for
                        // such a capture, so it rejects.
                        if !self.types.is_resolved(*capture) {
                            return fail(
                                ImageReason::Type,
                                format!("{at} capture {idx} has no resolved type"),
                            );
                        }
                        self.check_value(
                            vm,
                            *value,
                            *capture,
                            &format!("{at} capture {idx}"),
                            work,
                        )?;
                    }
                    Ok(())
                }
                _ => wrong(payload.shape().name),
            },
            BcType::List(elem) => match payload {
                Object::List { items } => {
                    for (idx, value) in items.iter().enumerate() {
                        self.check_value(vm, *value, elem, &format!("{at} item {idx}"), work)?;
                    }
                    Ok(())
                }
                _ => wrong(payload.shape().name),
            },
            BcType::Map(key, value) => match payload {
                Object::Map { entries, .. } => {
                    for (idx, (k, v)) in entries.iter().enumerate() {
                        self.check_value(vm, *k, key, &format!("{at} key {idx}"), work)?;
                        self.check_value(vm, *v, value, &format!("{at} value {idx}"), work)?;
                    }
                    Ok(())
                }
                _ => wrong(payload.shape().name),
            },
            BcType::Tuple(elems) => match payload {
                Object::Tuple { items } if items.len() == elems.len() => {
                    for (idx, (value, elem)) in items.iter().zip(elems.iter()).enumerate() {
                        self.check_value(vm, *value, *elem, &format!("{at} element {idx}"), work)?;
                    }
                    Ok(())
                }
                _ => wrong(payload.shape().name),
            },
            // An instance carries the fields its class layout declares,
            // under the type arguments of the position that named it.
            BcType::Class(_) | BcType::Inst(_, _) => match payload {
                Object::Instance { class, fields } => {
                    let types = self.types.instance_field_types(*class, ty).ok_or_else(|| {
                        ImageError::admission(
                            ImageReason::Type,
                            format!("{at} holds an instance whose field types do not follow"),
                        )
                    })?;
                    if types.len() != fields.len() {
                        return fail(
                            ImageReason::Layout,
                            format!("{at} holds another field count than its layout"),
                        );
                    }
                    for (idx, (value, field)) in fields.iter().zip(types.iter()).enumerate() {
                        // A field before its first assignment holds the
                        // uninitialized marker, and a read of it faults
                        // rather than trusting the slot.
                        if *value == Value::Uninit {
                            continue;
                        }
                        if !self.types.is_resolved(*field) {
                            return fail(
                                ImageReason::Type,
                                format!("{at} field {idx} has no resolved type"),
                            );
                        }
                        self.check_value(vm, *value, *field, &format!("{at} field {idx}"), work)?;
                    }
                    Ok(())
                }
                _ => wrong(payload.shape().name),
            },
            // A scalar type never holds a heap object.
            BcType::Unit | BcType::Bool | BcType::Int | BcType::Op(_, _) => {
                wrong(payload.shape().name)
            }
            // Every resolved type carries its substitution, so a
            // variable never reaches this walk.
            BcType::Var(_) => fail(
                ImageReason::Type,
                format!("{at} sits at an unresolved type variable"),
            ),
        }
    }

    /// The declared root result type of one nested container.
    ///
    /// The nested image stays opaque: this call decodes its container
    /// and reads the header alone. The nested body passes full
    /// admission at its own restore.
    fn nested_result_type(&self, bytes: &[u8], at: &str) -> Result<[u8; 32], ImageError> {
        let nested = codec::decode(bytes, LoadLimits::default()).map_err(|e| {
            ImageError::admission(
                e.reason,
                format!(
                    "{at} holds a nested container that does not decode: {}",
                    e.detail
                ),
            )
        })?;
        Ok(nested.result_type)
    }
}
