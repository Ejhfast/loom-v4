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
use lm_bytecode::closed::{ClosedType, TypeEnv, TypeEnvs};
use lm_bytecode::identity::{ModuleIdentity, COMPILER_ABI_VERSION};
use lm_bytecode::{BcRow, BcType, Instr};
use lm_heap::Object;
use lm_value::Value;
use lm_verify::{FramePoint, FrameSlots, FrameWitness, ResolvedTypes};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// The work one admission may perform.
///
/// The budget is one aggregate ledger for the whole image. It charges
/// one unit for each of these, and for nothing else:
///
/// - one resolved frame slot, operand, pending argument, or accepted
///   message the rules read;
/// - one triple the graph walk pushes. Every element of a list, a map,
///   a tuple, a closure, and an instance goes through that push, so the
///   work vector cannot grow past the ledger;
/// - one triple the graph walk visits;
/// - one object the coherence map records;
/// - each 64 bytes of one nested container it decodes.
///
/// The ledger therefore bounds the work vector, the visited set, and
/// the coherence map together. A compact container can never expand
/// into unbounded checking work.
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
    let tables = resolve_type_tables(image, module, &types, budget)?;
    let mut admit = Admit {
        image,
        module,
        identity,
        types,
        witness: tables,
        result_of: Vec::new(),
        mailbox_of: Vec::new(),
        frames_of: Vec::new(),
        allocators: RefCell::new(HashMap::new()),
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

/// Push one `(machine, object, resolved type)` triple onto the walk.
///
/// Every push charges the ledger, so the work vector can never grow
/// past the budget. A container with N elements pushes N triples, and
/// the ledger sees all N. Every element loop of `check_object` reaches
/// the walk through this one call.
fn push_work(
    budget: &mut AdmissionBudget,
    work: &mut Work,
    entry: (u32, u32, u32),
) -> Result<(), ImageError> {
    budget.charge(1)?;
    work.push(entry);
    Ok(())
}

/// The class of the instance one frame holds in its first local slot.
///
/// A method call moves its receiver into that slot, so the slot names
/// the class the runtime dispatched on. `None` marks a frame whose
/// first slot holds no instance, and a virtual call site then has no
/// receiver to resolve.
///
/// Every index the call reads is already proved: `check_references`
/// proved that every local names an object of its machine.
fn receiver_class(machine: &ImageMachine, base_local: u32) -> Option<u32> {
    let Value::Obj(r) = machine.locals.get(base_local as usize)? else {
        return None;
    };
    match machine.objects.get(r.slot as usize)?.object {
        Object::Instance { class, .. } => Some(class),
        _ => None,
    }
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

/// The witness tables of one image, resolved against the program.
///
/// The image carries one closed type table and one environment table.
/// The two maps below name the same entries in the two places
/// admission needs them: the resolved type universe of the verifier,
/// and one canonical table that answers content digests.
struct WitnessTables {
    /// The environment of every image environment ordinal, in the
    /// resolved type universe.
    envs: Vec<FrameWitness>,
    /// One canonical closed type table, for content digests.
    ///
    /// Its environment ordinals equal the image ordinals, because
    /// admission rejects a duplicate environment.
    canonical: RefCell<TypeEnvs>,
}

/// The state of one admission pass.
struct Admit<'m> {
    image: &'m Image,
    module: &'m lm_bytecode::Module,
    identity: &'m ModuleIdentity,
    types: ResolvedTypes<'m>,
    /// The witness tables the image carries.
    witness: WitnessTables,
    /// The resolved result type of every captured machine.
    result_of: Vec<Option<u32>>,
    /// The resolved mailbox message type of every captured proc.
    mailbox_of: Vec<Option<u32>>,
    /// The resolved slot types of every frame of every machine.
    frames_of: Vec<Vec<FrameSlots>>,
    /// The classes each function allocates, filled on demand.
    ///
    /// `New` and `NewG` are the one allocation path of an instance, so
    /// the set names the frames that may hold an object under
    /// construction.
    allocators: RefCell<HashMap<u32, HashSet<u32>>>,
}

/// Resolve the closed type table and the environment table of one
/// image against the program.
///
/// The call proves every entry before any later rule reads one: a
/// child index names an earlier entry, a class slot names a class of
/// the code manifest, an operation slot names the manifest, and an
/// effect name slot names the module string pool. It rejects a
/// duplicate entry in either table, so one image states one table.
///
/// Every entry charges the aggregate admission budget.
fn resolve_type_tables(
    image: &Image,
    module: &lm_bytecode::Module,
    types: &ResolvedTypes<'_>,
    budget: &mut AdmissionBudget,
) -> Result<WitnessTables, ImageError> {
    let mut canonical = TypeEnvs::new(u32::MAX, u32::MAX);
    let mut universe: Vec<u32> = Vec::with_capacity(image.types.len());
    let mut canonical_of: Vec<u32> = Vec::with_capacity(image.types.len());
    let mut seen: HashSet<ClosedType> = HashSet::new();
    let class_named = |slot: u32| {
        image
            .classes
            .binary_search_by_key(&slot, |(s, _)| *s)
            .is_ok()
    };
    for (at, node) in image.types.iter().enumerate() {
        budget.charge(1)?;
        for child in node.children() {
            if child as usize >= at {
                return fail(
                    ImageReason::Reference,
                    format!("closed type {at} names entry {child}, which is not an earlier one"),
                );
            }
        }
        if !seen.insert(node.clone()) {
            return fail(
                ImageReason::Layout,
                format!("closed type {at} repeats an earlier entry"),
            );
        }
        match node {
            ClosedType::Class(class) | ClosedType::Inst(class, _) => {
                if *class as usize >= module.classes.len() || !class_named(*class) {
                    return fail(
                        ImageReason::Code,
                        format!(
                            "closed type {at} names class slot {class}, which the manifest omits"
                        ),
                    );
                }
            }
            ClosedType::Op(op, _) => {
                if *op >= lm_abi::OP_COUNT {
                    return fail(
                        ImageReason::Code,
                        format!("closed type {at} names operation slot {op}"),
                    );
                }
            }
            _ => {}
        }
        if let ClosedType::Fn(params, muts, _, row) = node {
            if params.len() != muts.len() {
                return fail(
                    ImageReason::Layout,
                    format!("closed type {at} holds another marker count than parameters"),
                );
            }
            check_closed_row(module, row, at)?;
        }
        let mapped = node.remap(|child| canonical_of[child as usize]);
        let id = canonical.intern(mapped).map_err(|_| {
            ImageError::admission(ImageReason::Budget, "the closed type table passed its cap")
        })?;
        canonical_of.push(id);
        universe.push(types.intern(to_bc_type(node, &universe)));
    }
    // The environment table. Ordinal zero is the empty environment.
    let mut envs: Vec<FrameWitness> = Vec::with_capacity(image.envs.len());
    let mut seen_envs: HashSet<TypeEnv> = HashSet::new();
    for (at, env) in image.envs.iter().enumerate() {
        budget.charge(1)?;
        if at == 0 && !env.is_empty() {
            return fail(
                ImageReason::Layout,
                "environment zero is not the empty environment",
            );
        }
        if !seen_envs.insert(env.clone()) {
            return fail(
                ImageReason::Layout,
                format!("environment {at} repeats an earlier entry"),
            );
        }
        for ty in &env.types {
            if *ty as usize >= image.types.len() {
                return fail(
                    ImageReason::Reference,
                    format!("environment {at} names closed type {ty}, which the image has not"),
                );
            }
        }
        for row in &env.rows {
            check_closed_row(module, row, at)?;
        }
        let mapped = TypeEnv {
            types: env
                .types
                .iter()
                .map(|ty| canonical_of[*ty as usize])
                .collect(),
            rows: env.rows.clone(),
        };
        let id = canonical.intern_env(mapped).map_err(|_| {
            ImageError::admission(
                ImageReason::Budget,
                "the type environment table passed its cap",
            )
        })?;
        if id.0 as usize != at {
            return fail(
                ImageReason::Layout,
                format!("environment {at} does not take its own ordinal"),
            );
        }
        envs.push(FrameWitness {
            types: env.types.iter().map(|ty| universe[*ty as usize]).collect(),
            rows: env
                .rows
                .iter()
                .map(|row| row.iter().map(|slot| BcRow::Op(*slot)).collect())
                .collect(),
        });
    }
    Ok(WitnessTables {
        envs,
        canonical: RefCell::new(canonical),
    })
}

/// Prove that one closed effect row names this program and stays
/// canonical.
fn check_closed_row(
    module: &lm_bytecode::Module,
    row: &[u32],
    at: usize,
) -> Result<(), ImageError> {
    for slot in row {
        if *slot as usize >= module.strings.len() {
            return fail(
                ImageReason::Code,
                format!("entry {at} names effect name slot {slot}, which the program has not"),
            );
        }
    }
    if lm_bytecode::closed::canonical_row(module, row.to_vec()) != row {
        return fail(
            ImageReason::Layout,
            format!("entry {at} holds an effect row that is not canonical"),
        );
    }
    Ok(())
}

/// One verifier effect row as a closed row.
///
/// A closed row holds no effect variable, so a row that still names
/// one has no closed form.
fn closed_row(module: &lm_bytecode::Module, row: &[BcRow]) -> Option<Vec<u32>> {
    let mut out: Vec<u32> = Vec::with_capacity(row.len());
    for elem in row {
        match elem {
            BcRow::Op(slot) => out.push(*slot),
            BcRow::Var(_) => return None,
        }
    }
    Some(lm_bytecode::closed::canonical_row(module, out))
}

/// One image closed type node, in the resolved type universe.
///
/// Every child already has a universe index, because a child names an
/// earlier entry.
fn to_bc_type(node: &ClosedType, universe: &[u32]) -> BcType {
    let map = |id: &u32| universe[*id as usize];
    match node {
        ClosedType::Unit => BcType::Unit,
        ClosedType::Bool => BcType::Bool,
        ClosedType::Int => BcType::Int,
        ClosedType::Str => BcType::Str,
        ClosedType::StringBuilder => BcType::StringBuilder,
        ClosedType::ByteBuffer => BcType::ByteBuffer,
        ClosedType::Fault => BcType::Fault,
        ClosedType::Request => BcType::Request,
        ClosedType::PolicyTable => BcType::PolicyTable,
        ClosedType::EmptyVm => BcType::EmptyVm,
        ClosedType::Digest => BcType::Digest,
        ClosedType::SnapshotImage => BcType::SnapshotImage,
        ClosedType::Class(c) => BcType::Class(*c),
        ClosedType::Inst(c, args) => BcType::Inst(*c, args.iter().map(map).collect()),
        ClosedType::List(e) => BcType::List(map(e)),
        ClosedType::Map(k, v) => BcType::Map(map(k), map(v)),
        ClosedType::Tuple(elems) => BcType::Tuple(elems.iter().map(map).collect()),
        ClosedType::Fn(params, muts, ret, row) => BcType::Fn(
            params.iter().map(map).collect(),
            muts.clone(),
            map(ret),
            row.iter().map(|slot| BcRow::Op(*slot)).collect(),
        ),
        ClosedType::Vm(t) => BcType::Vm(map(t)),
        ClosedType::PendingCall(a, r) => BcType::PendingCall(map(a), map(r)),
        ClosedType::Handle(m, r) => BcType::Handle(map(m), map(r)),
        ClosedType::Op(op, f) => BcType::Op(*op, map(f)),
        ClosedType::Snapshot(t) => BcType::Snapshot(map(t)),
    }
}

impl Admit<'_> {
    fn run(&mut self, budget: &mut AdmissionBudget) -> Result<(), ImageError> {
        if self.image.machines.is_empty() {
            return fail(ImageReason::State, "a snapshot world holds no machine");
        }
        self.check_code_manifest()?;
        for vm in 0..self.image.machines.len() {
            self.check_references(vm as u32)?;
            self.check_state(vm as u32)?;
        }
        self.check_parent_forest()?;
        self.check_world()?;
        self.resolve_frames()?;
        self.resolve_relations();
        self.check_machine_witness()?;
        // The header repeats the root result type, and the witness of
        // the root machine derives it. The check runs after the
        // witness rules, so a damaged witness names its own rule.
        self.check_root_result_type()?;
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
    /// witness of that machine names it again. One image states one
    /// type.
    ///
    /// The header holds the canonical content digest of the closed
    /// type, so it names a class by definition hash and never by a
    /// numeric slot of one linked program.
    fn check_root_result_type(&self) -> Result<(), ImageError> {
        let root = self.machine_result_digest(0);
        if root != self.image.result_type {
            return fail(
                ImageReason::State,
                "the header and the root machine name two result types",
            );
        }
        Ok(())
    }

    /// The canonical digest of the closed result type of one machine.
    fn machine_result_digest(&self, vm: u32) -> [u8; 32] {
        let machine = self.machine(vm);
        let Some(func) = machine.body_func else {
            return [0u8; 32];
        };
        let Some(body) = self.module.funcs.get(func as usize) else {
            return [0u8; 32];
        };
        let mut table = self.witness.canonical.borrow_mut();
        let env = lm_value::TypeEnvId(machine.witness);
        let Ok(closed) = table.close(self.module, body.ret, env) else {
            return [0u8; 32];
        };
        table.digest(self.module, &self.identity.class_hashes, closed)
    }

    /// The canonical digest of one resolved universe type.
    ///
    /// The call rebuilds the type inside the canonical table, so the
    /// answer is the identity a nested container states. `None` marks
    /// a type that holds a variable, which no closed type can name.
    fn universe_digest(&self, ty: u32) -> Option<[u8; 32]> {
        let id = self.universe_closed(ty)?;
        let mut table = self.witness.canonical.borrow_mut();
        Some(table.digest(self.module, &self.identity.class_hashes, id))
    }

    /// One resolved universe type as a canonical closed type index.
    ///
    /// Every child index of a universe entry is smaller than the entry
    /// itself, so the walk terminates.
    fn universe_closed(&self, ty: u32) -> Option<u32> {
        let node = self.types.ty(ty)?;
        let built = match &node {
            BcType::Unit => ClosedType::Unit,
            BcType::Bool => ClosedType::Bool,
            BcType::Int => ClosedType::Int,
            BcType::Str => ClosedType::Str,
            BcType::StringBuilder => ClosedType::StringBuilder,
            BcType::ByteBuffer => ClosedType::ByteBuffer,
            BcType::Fault => ClosedType::Fault,
            BcType::Request => ClosedType::Request,
            BcType::PolicyTable => ClosedType::PolicyTable,
            BcType::EmptyVm => ClosedType::EmptyVm,
            BcType::Digest => ClosedType::Digest,
            BcType::SnapshotImage => ClosedType::SnapshotImage,
            BcType::Class(c) => ClosedType::Class(*c),
            BcType::Inst(c, args) => ClosedType::Inst(*c, self.universe_list(args)?),
            BcType::List(e) => ClosedType::List(self.universe_closed(*e)?),
            BcType::Map(k, v) => {
                ClosedType::Map(self.universe_closed(*k)?, self.universe_closed(*v)?)
            }
            BcType::Tuple(elems) => ClosedType::Tuple(self.universe_list(elems)?),
            BcType::Fn(params, muts, ret, row) => ClosedType::Fn(
                self.universe_list(params)?,
                muts.clone(),
                self.universe_closed(*ret)?,
                closed_row(self.module, row)?,
            ),
            BcType::Vm(t) => ClosedType::Vm(self.universe_closed(*t)?),
            BcType::PendingCall(a, r) => {
                ClosedType::PendingCall(self.universe_closed(*a)?, self.universe_closed(*r)?)
            }
            BcType::Handle(m, r) => {
                ClosedType::Handle(self.universe_closed(*m)?, self.universe_closed(*r)?)
            }
            BcType::Op(op, f) => ClosedType::Op(*op, self.universe_closed(*f)?),
            BcType::Snapshot(t) => ClosedType::Snapshot(self.universe_closed(*t)?),
            BcType::Var(_) => return None,
        };
        self.witness.canonical.borrow_mut().intern(built).ok()
    }

    fn universe_list(&self, ids: &[u32]) -> Option<Vec<u32>> {
        ids.iter()
            .map(|id| self.universe_closed(*id))
            .collect::<Option<Vec<u32>>>()
    }

    /// The witness one image environment ordinal names.
    fn env_of(&self, env: u32) -> Result<&FrameWitness, ImageError> {
        self.witness.envs.get(env as usize).ok_or_else(|| {
            ImageError::admission(
                ImageReason::Reference,
                format!("a witness names environment {env}, which the image has not"),
            )
        })
    }

    /// One declared module type under one image environment.
    fn subst_at(&self, ty: u32, env: u32) -> Result<u32, ImageError> {
        let held = self.env_of(env)?;
        Ok(self.types.substitute(ty, &held.types, &held.rows))
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
            // A policy table handle comes from a machine handle, and no
            // operation mints a handle to the performing machine, so no
            // machine holds a table handle to itself. A machine that
            // held one could pass any effect group to itself, past the
            // fresh default-deny table of specification 17.5. The table
            // edit path states the same rule at its use site.
            if matches!(entry.object, Object::NativeTable { vm: target } if target == vm) {
                return fail(
                    ImageReason::Reference,
                    at(&format!(
                        "object {ordinal} is a policy table handle to its own machine"
                    )),
                );
            }
            // A machine handle is holder local, so it never crosses a
            // boundary, and `Vm.New` is the one operation that mints
            // one. It gives the handle to the parent and names the
            // child, so no machine ever holds a handle to itself. The
            // restored world faults on such a handle, and the rule
            // states the property instead of relying on that.
            if matches!(entry.object, Object::NativeVm { vm: target } if target == vm) {
                return fail(
                    ImageReason::Reference,
                    at(&format!(
                        "object {ordinal} is a machine handle to its own machine"
                    )),
                );
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
            // Every operation slot an object names is a manifest slot.
            // The runtime reads the manifest by that slot, so the whole
            // pattern is checked here: a call token, a fault value, the
            // pending request, and a stored terminal fault.
            for op in [match entry.object {
                Object::NativeCall { op, .. } => Some(op),
                Object::NativeFault { op, .. } => op,
                _ => None,
            }]
            .into_iter()
            .flatten()
            {
                if op >= lm_abi::OP_COUNT {
                    return fail(
                        ImageReason::Code,
                        at(&format!(
                            "object {ordinal} names operation slot {op}, which the manifest has \
                             not"
                        )),
                    );
                }
            }
            // Every witness names an environment of the image, and the
            // arity of that environment matches the generic arity of
            // the class or the function the object names.
            if let Object::Instance { env, .. } | Object::Closure { env, .. } = &entry.object {
                self.env_of(env.env().0)?;
            }
            match &entry.object {
                Object::Instance { class, fields, env } => {
                    if *class as usize >= self.module.classes.len() || !self.class_named(*class) {
                        return fail(
                            ImageReason::Code,
                            at(&format!(
                                "object {ordinal} names class slot {class}, which the manifest \
                                 omits"
                            )),
                        );
                    }
                    // An abstract class is the closed parent of one
                    // enum family, and no verified program allocates
                    // one. An instance of it would reach the
                    // exhaustive-case backstop of every dispatch.
                    if self.module.classes[*class as usize].kind
                        == lm_bytecode::BcClassKind::Abstract
                    {
                        return fail(
                            ImageReason::State,
                            at(&format!(
                                "object {ordinal} is an instance of abstract class {class}"
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
                    // An instance states the arguments of its own
                    // class, or it states nothing. The VM kernel
                    // builds a core enum instance outside `New` and
                    // `NewG` and records the empty witness there, so
                    // the empty environment is legal at every arity.
                    let arity = self.module.classes[*class as usize].type_params as usize;
                    let held = self.env_of(env.env().0)?.types.len();
                    if held != 0 && held != arity {
                        return fail(
                            ImageReason::Type,
                            at(&format!(
                                "object {ordinal} carries a witness of another arity than class \
                                 {class}"
                            )),
                        );
                    }
                }
                Object::Closure {
                    func,
                    captures,
                    env,
                } => {
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
                    let body = &self.module.funcs[*func as usize];
                    let held = self.env_of(env.env().0)?;
                    if held.types.len() != body.type_params as usize
                        || held.rows.len() != body.effect_params as usize
                    {
                        return fail(
                            ImageReason::Type,
                            at(&format!(
                                "object {ordinal} carries a witness of another arity than \
                                 function {func}"
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
            // block ends with a terminator, so a live frame never
            // reaches the end. A faulted machine stopped inside the
            // instruction the counter passed, and the backstop of an
            // exhaustive case is the last instruction of its block, so
            // its counter may name the end.
            let limit = code.blocks[frame.block as usize].len();
            let past = match m.state {
                ImageState::Faulted => frame.ip as usize > limit,
                _ => frame.ip as usize >= limit,
            };
            if past {
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
        match &m.terminal {
            Some(ImageTerminal::Done(value)) => object_ref(value, "the terminal value")?,
            Some(ImageTerminal::Fault(rec)) => {
                if rec.op.is_some_and(|op| op >= lm_abi::OP_COUNT) {
                    return fail(
                        ImageReason::Code,
                        at("the terminal fault names no manifest operation"),
                    );
                }
            }
            None => {}
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
            // The operand arena starts where the bottom frame starts.
            // Specification 5.1 asks for an exact partition of the
            // arena, so no value sits below the bottom base and no
            // frame owns a value the program point does not prove.
            if idx == 0 && frame.base_operand != 0 {
                return fail(
                    ImageReason::Layout,
                    at("frame 0 does not start the operand arena"),
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
                // The frame runs that closure, so the two carry one
                // environment. A frame that named another environment
                // would read its captures under a substitution the
                // closure never held.
                Object::Closure { func, env, .. }
                    if func == frame.func && env.env().0 == frame.env => {}
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
                // A machine reaches `Done` only by returning its last
                // frame, so it holds none. The frameless-operand rule
                // above then forces its arenas empty as well.
                //
                // A fault leaves every frame in place, because the
                // frames are the record of where the machine stopped.
                // A faulted machine never executes again, so those
                // frames are diagnostic state: the rules above prove
                // their structure and admission derives no type from
                // them.
                if m.state == ImageState::Done && !m.frames.is_empty() {
                    return fail(ImageReason::State, at("a done machine holds a frame"));
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
                // The state rule above already refused a blocked
                // machine with no request. The read states that fact
                // again instead of asserting it.
                let Some(op) = m.pending.as_ref().map(|p| p.op) else {
                    return fail(
                        ImageReason::State,
                        at("a blocked machine holds no pending request"),
                    );
                };
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
        //
        // A request the operation table marks as a host attachment is
        // legal here. `Asked` records the request before any
        // attachment opens, and the holder answers it. The live
        // attachment belongs to `Waiting`, and the capture refuses
        // that state in `write.rs`, so no image ever carries one.
        if let Some(pending) = &m.pending {
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
                // `check_state` already refused an asked machine with
                // no request, so this read never answers `None`.
                if let (ImageState::Asked, Some(pending)) = (target.state, target.pending.as_ref())
                {
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
    /// the call site the frame below stopped inside, and the frame
    /// witness must agree with it. The bottom frame has no call site
    /// below it, so its witness answers alone.
    fn resolve_frames(&mut self) -> Result<(), ImageError> {
        let mut out: Vec<Vec<FrameSlots>> = Vec::with_capacity(self.image.machines.len());
        for (vm, machine) in self.image.machines.iter().enumerate() {
            // A faulted machine stopped inside an instruction that did
            // not complete, so its arenas are not the state the
            // verifier proves at any boundary. It never executes
            // again, so no rule reads a type from its frames.
            if machine.state == ImageState::Faulted {
                out.push(Vec::new());
                continue;
            }
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
                    // A method call moves its receiver into local slot
                    // 0 of the callee frame, so the class of that
                    // value is the class the runtime dispatched on.
                    receiver_class: receiver_class(machine, frame.base_local),
                })
                .collect();
            let mut held: Vec<FrameWitness> = Vec::with_capacity(machine.frames.len());
            for frame in &machine.frames {
                held.push(self.env_of(frame.env)?.clone());
            }
            let slots = self.types.resolve_chain(&points, &held).map_err(|e| {
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
        for vm in 0..self.image.machines.len() as u32 {
            result_of.push(self.machine_result_type(vm));
            mailbox_of.push(self.mailbox_type(vm));
        }
        self.result_of = result_of;
        self.mailbox_of = mailbox_of;
    }

    /// The declared terminal result type of one captured machine.
    ///
    /// The machine witness states the body function and the
    /// environment of its activation. A machine drops its body closure
    /// and every frame, so the witness is the one lasting evidence.
    /// `check_machine_witness` proves the witness against the body
    /// closure and against the bottom frame wherever one exists.
    fn machine_result_type(&self, vm: u32) -> Option<u32> {
        let machine = self.machine(vm);
        let ret = self.types.result_type(machine.body_func?)?;
        let closed = self.subst_at(ret, machine.witness).ok()?;
        self.types.is_resolved(closed).then_some(closed)
    }

    /// The mailbox message type of one captured machine.
    ///
    /// A machine that `Proc.Spawn` launched takes its type from its
    /// proc class. The class fixes the type, and the first parameter of
    /// the machine body names the proc instance. `None` means the type
    /// does not follow from the image, and a proc that carries a
    /// message then has no governing type.
    ///
    /// Every other machine accepts no message, so its type is `Never`.
    /// `sys.proc.run` hands the caller exactly that handle: it moves a
    /// loaded machine to the scheduler, and no proc class stands behind
    /// it (`crates/lm-hir/src/checkfn.rs`, the `sys.proc.run` surface).
    /// The lowering spells `Never` as `Unit`, and the verified type
    /// table starts with `Unit`, so the type resolves for every module.
    ///
    /// The rule proves no message of such a machine. `check_state`
    /// rejects a queued message on a machine that is not a proc, so no
    /// value reaches the mailbox rule through this answer.
    fn mailbox_type(&self, vm: u32) -> Option<u32> {
        let machine = self.machine(vm);
        if !machine.is_proc {
            return Some(self.types.intern(BcType::Unit));
        }
        let instance = *self.types.params(machine.body_func?)?.first()?;
        let closed = self.subst_at(instance, machine.witness).ok()?;
        self.types.proc_mailbox(closed)
    }

    /// Prove the machine witness of every captured machine.
    ///
    /// The witness is data, so admission checks it wherever a
    /// derivation exists. The stored proc body states the body
    /// function and its environment directly. A machine with no proc
    /// body runs its own body in its bottom frame, so that frame
    /// states them instead. A terminal machine holds neither, and the
    /// witness stands alone there.
    ///
    /// A machine that claims the proc birth grant must name a proc
    /// class through its witness. Without the rule an edited image
    /// takes the whole `Proc` group at restore.
    fn check_machine_witness(&self) -> Result<(), ImageError> {
        for (vm, machine) in self.image.machines.iter().enumerate() {
            let at = |what: &str| format!("machine {vm}: {what}");
            if let Some(func) = machine.body_func {
                if func as usize >= self.module.funcs.len() || !self.func_named(func) {
                    return fail(
                        ImageReason::Code,
                        at("the witness names a function the manifest omits"),
                    );
                }
            }
            let derived: Option<(u32, u32)> = match machine.start_body {
                Some(ordinal) => match machine.objects.get(ordinal as usize).map(|o| &o.object) {
                    Some(Object::Closure { func, env, .. }) => Some((*func, env.env().0)),
                    _ => None,
                },
                // A proc inside its constructor keeps its body
                // closure, so the branch above answers there. Every
                // other machine with a frame runs its own body in the
                // bottom frame.
                None => machine.frames.first().map(|f| (f.func, f.env)),
            };
            if let Some((func, env)) = derived {
                if machine.body_func != Some(func) || machine.witness != env {
                    return fail(
                        ImageReason::Type,
                        at("the machine witness does not match its body"),
                    );
                }
            }
            if machine.body_func.is_none() && machine.witness != 0 {
                return fail(
                    ImageReason::Type,
                    at("a machine with no body function names an environment"),
                );
            }
            if machine.is_proc && self.mailbox_of[vm].is_none() {
                return fail(
                    ImageReason::Mailbox,
                    at("a machine that claims the proc grant names no proc class"),
                );
            }
        }
        Ok(())
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
        // A faulted machine keeps its frames as diagnostic state, and
        // no rule below reads a type from them. `check_state` and
        // `check_references` already proved their structure.
        if machine.state == ImageState::Faulted {
            self.check_terminal(vm, budget, work)?;
            self.check_mailbox(vm, budget, work)?;
            self.check_proc_body(vm, budget, work)?;
            return Ok(());
        }
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
                        self.check_value(
                            vm,
                            *value,
                            *ty,
                            &at(&format!("local {slot}")),
                            budget,
                            work,
                        )?;
                    }
                    None => {
                        if *value != Value::Uninit {
                            self.check_value(
                                vm,
                                *value,
                                slots.declared[slot],
                                &at(&format!("local {slot}")),
                                budget,
                                work,
                            )?;
                        }
                    }
                }
            }
        }
        self.check_operands(vm, budget, work)?;
        self.check_pending_args(vm, budget, work)?;
        self.check_terminal(vm, budget, work)?;
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
            // The frame witness closes the declared function type, so
            // a closure of a generic body joins the walk at the type
            // its activation carries.
            let ty = self.subst_at(ty, frame.env)?;
            push_work(budget, work, (vm, closure, ty))?;
        }
        self.check_proc_body(vm, budget, work)?;
        Ok(())
    }

    /// The proc body is a closure the runtime calls with the proc
    /// instance. Its own function type proves its captures.
    fn check_proc_body(
        &self,
        vm: u32,
        budget: &mut AdmissionBudget,
        work: &mut Work,
    ) -> Result<(), ImageError> {
        let machine = self.machine(vm);
        let Some(body) = machine.start_body else {
            return Ok(());
        };
        if let Object::Closure { func, env, .. } = machine.objects[body as usize].object {
            let ty = self.types.fn_type(func).ok_or_else(|| {
                ImageError::admission(
                    ImageReason::Type,
                    format!("machine {vm}: the proc body has no function type"),
                )
            })?;
            let ty = self.subst_at(ty, env.env().0)?;
            push_work(budget, work, (vm, body, ty))?;
        }
        Ok(())
    }

    /// What the instruction one frame stopped inside took off the
    /// operand stack.
    ///
    /// A frame stops at one of two points. The top frame of a machine
    /// with no pending request stops before the instruction its program
    /// counter names, so that instruction consumed nothing. Every other
    /// frame, and the top frame of a machine with a pending request,
    /// stopped inside the instruction before the counter.
    ///
    /// The first answer counts the operands that instruction popped.
    /// The second counts the popped operands the pending record keeps.
    /// The counts come from the instruction alone, so the image states
    /// neither of them.
    fn stop_effect(&self, vm: u32, idx: usize) -> Result<(usize, usize), ImageError> {
        let machine = self.machine(vm);
        let top = idx + 1 == machine.frames.len();
        let pending = top.then_some(machine.pending.as_ref()).flatten();
        if top && pending.is_none() {
            return Ok((0, 0));
        }
        let at = |what: &str| format!("machine {vm}: frame {idx} {what}");
        let frame = &machine.frames[idx];
        // Every index below is proved: `check_references` proved the
        // function, the block, and the program counter of every frame.
        let block = &self.module.funcs[frame.func as usize].blocks[frame.block as usize];
        let Some(before) = frame.ip.checked_sub(1) else {
            return fail(
                ImageReason::Layout,
                at("stopped before the first instruction of its block"),
            );
        };
        let instr = block[before as usize];
        // A call pushes the frame above. A perform records the pending
        // request. No other instruction leaves a frame stopped, so an
        // image that names one states a stop the runtime never reaches.
        let counts = match (instr, pending) {
            (Instr::Call(callee) | Instr::CallG { func: callee, .. }, None) => {
                (self.module.funcs[callee as usize].params.len(), 0)
            }
            (Instr::CallVirtual { argc, .. } | Instr::CallVirtualG { argc, .. }, None) => {
                // A virtual call consumes its receiver as well.
                (argc as usize + 1, 0)
            }
            // A closure call consumes the closure value as well.
            (Instr::CallValue { argc }, None) => (argc as usize + 1, 0),
            (Instr::Perform { op, argc, .. }, Some(request)) => {
                if request.op != op {
                    return fail(
                        ImageReason::State,
                        at("stopped inside another operation than the pending request"),
                    );
                }
                (argc as usize, argc as usize)
            }
            // A perform through an operation value consumes that value
            // as well. The proved type of the value names the exact
            // operation, so the pending record states no operation the
            // program point does not prove.
            (Instr::PerformValue { argc, .. }, Some(request)) => {
                let types = &self.frames_of[vm as usize][idx].operands;
                let at_op = types.len().checked_sub(argc as usize + 1);
                let named = match at_op.and_then(|slot| types.get(slot)) {
                    Some(ty) => matches!(self.ty(*ty)?, BcType::Op(op, _) if op == request.op),
                    None => false,
                };
                if !named {
                    return fail(
                        ImageReason::State,
                        at("stopped inside another operation than the pending request"),
                    );
                }
                (argc as usize + 1, argc as usize)
            }
            _ => {
                return fail(
                    ImageReason::Layout,
                    at("did not stop inside a call or a perform"),
                )
            }
        };
        Ok(counts)
    }

    /// Prove the operand types of every stopped frame.
    ///
    /// The operands a frame retains are the stack the verifier proved
    /// before the instruction the frame stopped inside, less every
    /// operand that instruction consumed. The rule is an equality at
    /// every frame, not the top frame alone: a lower frame that keeps
    /// one extra value hides that value under the call, and the value
    /// survives the return with no proved type.
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
            let (consumed, _) = self.stop_effect(vm, idx)?;
            let Some(retained) = types.len().checked_sub(consumed) else {
                return fail(
                    ImageReason::Layout,
                    format!(
                        "machine {vm}: frame {idx} stopped inside an instruction its program \
                         point cannot supply"
                    ),
                );
            };
            if want != retained {
                return fail(
                    ImageReason::Layout,
                    format!(
                        "machine {vm}: frame {idx} holds {want} operands and the program point \
                         proves {retained}"
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
                    budget,
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
    /// The count comes from the perform instruction, never from the
    /// record: the image states the argument list, and a record that
    /// holds another number of arguments does not agree with the
    /// program point the frame names. The rule reads no manifest
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
        if machine.frames.is_empty() {
            return fail(
                ImageReason::State,
                format!("machine {vm}: a pending request holds no frame"),
            );
        }
        let idx = machine.frames.len() - 1;
        let types = &self.frames_of[vm as usize][idx].operands;
        let (_, argc) = self.stop_effect(vm, idx)?;
        if pending.args.len() != argc {
            return fail(
                ImageReason::State,
                format!(
                    "machine {vm}: the pending request holds {} arguments and the perform takes \
                     {argc}",
                    pending.args.len()
                ),
            );
        }
        // The arguments are the top of the proved stack. A perform
        // through an operation value keeps that value below them, and
        // `stop_effect` proved which operation it names.
        let Some(at) = types.len().checked_sub(argc) else {
            return fail(
                ImageReason::Layout,
                format!("machine {vm}: the perform takes more arguments than the stack proves"),
            );
        };
        budget.charge(argc as u64)?;
        for (offset, ty) in types.iter().skip(at).enumerate() {
            self.check_value(
                vm,
                pending.args[offset],
                *ty,
                &format!("machine {vm}: pending argument {offset}"),
                budget,
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
    fn check_terminal(
        &self,
        vm: u32,
        budget: &mut AdmissionBudget,
        work: &mut Work,
    ) -> Result<(), ImageError> {
        let machine = self.machine(vm);
        let Some(ImageTerminal::Done(value)) = &machine.terminal else {
            return Ok(());
        };
        // A terminal machine keeps no frame, so the witness is the one
        // record of the type its stored result carries.
        let Some(ty) = self.result_of[vm as usize] else {
            return fail(
                ImageReason::State,
                format!("machine {vm}: a terminal value carries no result type to prove"),
            );
        };
        self.check_value(
            vm,
            *value,
            ty,
            &format!("machine {vm}: the terminal value"),
            budget,
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
                budget,
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
        budget: &mut AdmissionBudget,
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
                push_work(budget, work, (vm, r.slot, ty))?;
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
        // Specification 5.4: one object carries one exact closed type.
        let mut exact: HashMap<(u32, u32), u32> = HashMap::new();
        while let Some((vm, object, ty)) = work.pop() {
            if !visited.insert((vm, object, ty)) {
                continue;
            }
            budget.charge(1)?;
            // The coherence rule runs first. It states the general
            // property of the object, and every later rule states a
            // property of one edge, so a shared object that two edges
            // type differently names that rule instead of the first
            // edge rule the walk happens to reach.
            self.check_coherence(vm, object, ty, &mut exact, budget)?;
            self.check_object(vm, object, ty, budget, work)?;
        }
        self.check_instance_witness(&exact)
    }

    /// Prove the witness of every instance the walk reached.
    ///
    /// An instance witness is evidence for no admission rule, because
    /// the edge that reaches the object already supplies
    /// `Inst(class, args)`. Admission checks it because the image
    /// carries it, and section 14 of the design states why the image
    /// carries it: a reflection query has no edge to read.
    ///
    /// The check runs after the coherence map settles, so it reads the
    /// one exact type of the object instead of the first edge the walk
    /// happened to pop.
    ///
    /// The VM kernel builds a core enum instance outside `New` and
    /// `NewG` and states no arguments there, so the empty witness
    /// passes at every class.
    fn check_instance_witness(&self, exact: &HashMap<(u32, u32), u32>) -> Result<(), ImageError> {
        let mut entries: Vec<(&(u32, u32), &u32)> = exact.iter().collect();
        entries.sort_unstable();
        for ((vm, object), ty) in entries {
            let Object::Instance { class, env, .. } =
                &self.image.machines[*vm as usize].objects[*object as usize].object
            else {
                continue;
            };
            let held = &self.env_of(env.env().0)?.types;
            if held.is_empty() {
                continue;
            }
            let args = self
                .types
                .as_instance(*ty)
                .map(|(_, args)| args)
                .unwrap_or_default();
            if *held != args {
                return fail(
                    ImageReason::Type,
                    format!(
                        "machine {vm} object {object} carries class arguments of class {class} \
                         that the edge does not name"
                    ),
                );
            }
        }
        Ok(())
    }

    /// The one exact closed type of one object.
    ///
    /// The type of an instance follows from the concrete class of the
    /// object, so two positions that name one instance at two class
    /// levels normalize to one type. The type of a closure follows from
    /// its function in the same way.
    ///
    /// Every other shape takes the type of the edge that reached it. An
    /// empty list names no element type, so the object alone answers
    /// nothing.
    fn exact_type(&self, vm: u32, object: u32, ty: u32) -> u32 {
        match self.image.machines[vm as usize].objects[object as usize].object {
            Object::Instance { class, .. } => self.types.instance_type(class, ty).unwrap_or(ty),
            // The witness closes the declared function type, so two
            // edges into one closure normalize to one type.
            Object::Closure { func, env, .. } => match self.types.fn_type(func) {
                Some(declared) => self.subst_at(declared, env.env().0).unwrap_or(ty),
                None => ty,
            },
            _ => ty,
        }
    }

    /// Prove that every edge into one object names one exact type.
    ///
    /// One typed edge proves that edge alone. Two locals typed
    /// `List[Int]` and `List[Str]` can name one empty mutable list, and
    /// each edge passes because the list holds no element. Verified
    /// code then appends an integer through the first local and reads a
    /// string through the second.
    ///
    /// Class arguments are invariant (`docs/specs/language-spec.md`
    /// section 6), so `List[Int]` and `List[Str]` are unrelated types
    /// and the aliased list rejects. A `Dog` instance reached from an
    /// `Animal` edge still admits: nominal subclassing is real
    /// subtyping, and the concrete class answers the same exact type at
    /// both edges.
    ///
    /// The map keeps the most specific type any edge names, so the
    /// order of the walk does not change the answer.
    fn check_coherence(
        &self,
        vm: u32,
        object: u32,
        ty: u32,
        exact: &mut HashMap<(u32, u32), u32>,
        budget: &mut AdmissionBudget,
    ) -> Result<(), ImageError> {
        let found = self.exact_type(vm, object, ty);
        let key = (vm, object);
        let Some(held) = exact.get(&key).copied() else {
            budget.charge(1)?;
            exact.insert(key, found);
            return Ok(());
        };
        if self.types.is_subtype(held, found) {
            return Ok(());
        }
        if self.types.is_subtype(found, held) {
            exact.insert(key, found);
            return Ok(());
        }
        fail(
            ImageReason::Layout,
            format!(
                "machine {vm} object {object} is reached at two types that no one value carries"
            ),
        )
    }

    /// Prove one object against one resolved type.
    #[allow(clippy::too_many_lines)]
    fn check_object(
        &self,
        vm: u32,
        object: u32,
        ty: u32,
        budget: &mut AdmissionBudget,
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
                    self.nested_result_type(bytes, budget, &at)?;
                    Ok(())
                }
                _ => wrong(payload.shape().name),
            },
            BcType::Snapshot(want) => match payload {
                Object::NativeSnapshot(bytes) => {
                    let found = self.nested_result_type(bytes, budget, &at)?;
                    // The closed type table gives the argument a
                    // canonical identity, and the nested header states
                    // the same identity for its root.
                    let Some(digest) = self.universe_digest(want) else {
                        return fail(
                            ImageReason::Type,
                            format!("{at} holds a snapshot type with no closed form"),
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
            // witness closes the declared function type and every
            // capture type, because a capture type can name a type
            // variable the closure signature does not hold.
            BcType::Fn(_, _, _, _) => match payload {
                Object::Closure {
                    func,
                    captures,
                    env,
                } => {
                    let declared_fn = self.types.fn_type(*func).ok_or_else(|| {
                        ImageError::admission(
                            ImageReason::Type,
                            format!("{at} names a function with no function type"),
                        )
                    })?;
                    let declared_fn = self.subst_at(declared_fn, env.env().0)?;
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
                        let capture = self.subst_at(*capture, env.env().0)?;
                        // A capture type that still holds a variable
                        // after the witness has no decidable value
                        // set, so admission rejects it instead of
                        // leaving the capture unchecked.
                        if !self.types.is_resolved(capture) {
                            return fail(
                                ImageReason::Type,
                                format!("{at} capture {idx} has no resolved type"),
                            );
                        }
                        self.check_value(
                            vm,
                            *value,
                            capture,
                            &format!("{at} capture {idx}"),
                            budget,
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
                        self.check_value(
                            vm,
                            *value,
                            elem,
                            &format!("{at} item {idx}"),
                            budget,
                            work,
                        )?;
                    }
                    Ok(())
                }
                _ => wrong(payload.shape().name),
            },
            BcType::Map(key, value) => match payload {
                Object::Map { entries, .. } => {
                    for (idx, (k, v)) in entries.iter().enumerate() {
                        self.check_value(vm, *k, key, &format!("{at} key {idx}"), budget, work)?;
                        self.check_value(
                            vm,
                            *v,
                            value,
                            &format!("{at} value {idx}"),
                            budget,
                            work,
                        )?;
                    }
                    Ok(())
                }
                _ => wrong(payload.shape().name),
            },
            BcType::Tuple(elems) => match payload {
                Object::Tuple { items } if items.len() == elems.len() => {
                    for (idx, (value, elem)) in items.iter().zip(elems.iter()).enumerate() {
                        self.check_value(
                            vm,
                            *value,
                            *elem,
                            &format!("{at} element {idx}"),
                            budget,
                            work,
                        )?;
                    }
                    Ok(())
                }
                _ => wrong(payload.shape().name),
            },
            // An instance carries the fields its class layout declares,
            // under the type arguments of the position that named it.
            BcType::Class(_) | BcType::Inst(_, _) => match payload {
                Object::Instance { class, fields, .. } => {
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
                    let under_construction = fields.contains(&Value::Uninit)
                        && !self.is_under_construction(vm, object, *class);
                    if under_construction {
                        return fail(
                            ImageReason::State,
                            format!(
                                "{at} holds an uninitialized field and is not the object under \
                                 construction of its machine"
                            ),
                        );
                    }
                    for (idx, (value, field)) in fields.iter().zip(types.iter()).enumerate() {
                        // A field before its first assignment holds the
                        // uninitialized marker. The rule above proves
                        // that the object is the one under
                        // construction on its own frame stack, so no
                        // other instance reaches this branch.
                        if *value == Value::Uninit {
                            continue;
                        }
                        if !self.types.is_resolved(*field) {
                            return fail(
                                ImageReason::Type,
                                format!("{at} field {idx} has no resolved type"),
                            );
                        }
                        self.check_value(
                            vm,
                            *value,
                            *field,
                            &format!("{at} field {idx}"),
                            budget,
                            work,
                        )?;
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

    /// True when one instance is the object under construction on the
    /// frame stack of its own machine.
    ///
    /// `New` and `NewG` are the one allocation path of an instance,
    /// and the compiler emits them inside the construction function of
    /// the class alone. That function stores the object in one local
    /// slot, evaluates the field defaults, and calls the initializer
    /// over it. `self` cannot escape before the initializer assigns
    /// every required field (`E1029`), and a read before the first
    /// assignment rejects at the checker (`E1028`). A partly built
    /// instance therefore sits in a local slot or on the operand stack
    /// of the frame that allocated it, and nowhere else.
    ///
    /// The rule is exactly that: some frame of the machine must name
    /// the object directly, and the function of that frame must
    /// allocate the class of the object.
    fn is_under_construction(&self, vm: u32, object: u32, class: u32) -> bool {
        let machine = self.machine(vm);
        let names = |value: &Value| matches!(value, Value::Obj(r) if r.slot == object);
        for (idx, frame) in machine.frames.iter().enumerate() {
            if !self.allocates(frame.func, class) {
                continue;
            }
            let local_end = match machine.frames.get(idx + 1) {
                Some(next) => next.base_local as usize,
                None => machine.locals.len(),
            };
            let operand_end = match machine.frames.get(idx + 1) {
                Some(next) => next.base_operand as usize,
                None => machine.operands.len(),
            };
            let locals = machine
                .locals
                .get(frame.base_local as usize..local_end.min(machine.locals.len()))
                .unwrap_or(&[]);
            let operands = machine
                .operands
                .get(frame.base_operand as usize..operand_end.min(machine.operands.len()))
                .unwrap_or(&[]);
            if locals.iter().any(names) || operands.iter().any(names) {
                return true;
            }
        }
        false
    }

    /// True when one function allocates instances of one class.
    ///
    /// The answer comes from the verified body: `New` and `NewG` name
    /// the class they allocate. The set is filled on demand, so a
    /// program pays for the functions its frames name alone.
    fn allocates(&self, func: u32, class: u32) -> bool {
        if let Some(known) = self.allocators.borrow().get(&func) {
            return known.contains(&class);
        }
        let mut found: HashSet<u32> = HashSet::new();
        if let Some(body) = self.module.funcs.get(func as usize) {
            for block in &body.blocks {
                for instr in block {
                    match instr {
                        Instr::New(c) | Instr::NewG { class: c, .. } => {
                            found.insert(*c);
                        }
                        _ => {}
                    }
                }
            }
        }
        let answer = found.contains(&class);
        self.allocators.borrow_mut().insert(func, found);
        answer
    }

    /// The declared root result type of one nested container.
    ///
    /// The nested image stays opaque: this call decodes its container
    /// and reads the header alone. The nested body passes full
    /// admission at its own restore.
    fn nested_result_type(
        &self,
        bytes: &[u8],
        budget: &mut AdmissionBudget,
        at: &str,
    ) -> Result<[u8; 32], ImageError> {
        // The nested container costs decoding work, so the aggregate
        // ledger carries it. One unit stands for one 64-byte block.
        budget.charge(bytes.len() as u64 / 64 + 1)?;
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
