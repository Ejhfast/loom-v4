//! Snapshot image admission
//! (`docs/specs/sidecar/snapshot-image-admission.md` sections 5, 7, and 10).
//!
//! Admission is the one promotion from editable `Image` data to the
//! immutable `SnapshotImage` that restore accepts. It proves one rule:
//!
//! > An `Image` becomes `SnapshotImage` only when its structure
//! > resolves.
//!
//! Structural resolution means that every ordinal names an entry that
//! exists, every frame names a reachable instruction boundary, every
//! lifecycle record agrees with its state, and every stored table is
//! self-consistent. The restore path and the interpreter read all of
//! those without recovery.
//!
//! Admission proves no type of a stored value. The interpreter tests
//! the tag at each accessor, and the world checks a value against the
//! type of verified code at each VM boundary
//! (`crates/lm-vm/src/typecheck.rs`). A wrong type is therefore a
//! contained machine fault, never a host panic, and no rule here
//! derives an expected type from container data.
//!
//! The closed type table and the type environment table stay. They are
//! a runtime carrier: they give a restored generic frame its concrete
//! types, and they are the substrate a later `Type[T]` descriptor
//! reads. Admission checks them structurally alone: an ordinal lies in
//! range, an entry holds no free type variable, the table is acyclic,
//! and an arity matches. It proves nothing about whether a witness is
//! the one execution would have produced.

use super::{
    codec, image_roots, AdmissionIdentity, Image, ImageBlock, ImageError, ImageMachine,
    ImagePolicyCursor, ImageReason, ImageSlotTarget, ImageState, ImageTerminal, ImageWaitSource,
    LoadLimits, SnapshotImage, FORMAT_VERSION,
};
use crate::LoadedModule;
use lm_bytecode::closed::{ClosedType, TypeEnv, TypeEnvs};
use lm_bytecode::identity::{ModuleIdentity, COMPILER_ABI_VERSION};
use lm_bytecode::{BcType, ExtendedInstr, Instr, SlotContract};
use lm_heap::{CodeHandleKind, Object, PortableCodeKind};
use lm_value::Value;
use std::cell::RefCell;
use std::collections::HashSet;

/// The aggregate work ledger of one admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionBudget {
    limit: u64,
    used: u64,
    byte_limit: usize,
}

/// One bounded cache for repeated admissions with the same code.
///
/// The cache retains only the latest verified aggregate. It never
/// caches machine state, heaps, policies, or slot targets.
#[derive(Default)]
pub struct AdmissionCache {
    code: Option<CachedCode>,
}

#[derive(Clone, PartialEq, Eq)]
struct CodeCacheKey {
    base_verification: [u8; 32],
    artifacts: Vec<[u8; 32]>,
    providers: Vec<(Vec<u32>, Vec<u32>)>,
}

struct CachedCode {
    key: CodeCacheKey,
    aggregate: LoadedModule,
    installations: Vec<InstallationProof>,
}

enum CodeProof<'a> {
    Owned(Box<LoadedModule>, Vec<InstallationProof>),
    Cached(&'a CachedCode),
}

impl CodeProof<'_> {
    fn aggregate(&self) -> &LoadedModule {
        match self {
            CodeProof::Owned(aggregate, _) => aggregate.as_ref(),
            CodeProof::Cached(cached) => &cached.aggregate,
        }
    }

    fn installations(&self) -> &[InstallationProof] {
        match self {
            CodeProof::Owned(_, installations) => installations,
            CodeProof::Cached(cached) => &cached.installations,
        }
    }
}

/// The default aggregate admission work limit, in units.
///
/// One unit covers one table entry, stored record, or graph edge.
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
        let next = self.used.checked_add(units).ok_or_else(|| {
            ImageError::admission(ImageReason::Budget, "the admission work count overflowed")
        })?;
        if next > self.limit {
            return Err(ImageError::admission(
                ImageReason::Budget,
                format!(
                    "the admission work passed the budget of {} units",
                    self.limit
                ),
            ));
        }
        self.used = next;
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
    let proof = prove(&image, loaded, budget)?;
    codec::seal_admitted(image, proof.identity, proof.loaded, budget.byte_limit())
}

/// Verified code and identity produced by one admission proof.
pub(super) struct AdmissionProof {
    pub(super) identity: AdmissionIdentity,
    pub(super) loaded: LoadedModule,
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
) -> Result<AdmissionProof, ImageError> {
    prove_inner(image, loaded, budget, None)
}

pub(super) fn prove_cached(
    image: &Image,
    loaded: &LoadedModule,
    budget: &mut AdmissionBudget,
    cache: &mut AdmissionCache,
) -> Result<AdmissionProof, ImageError> {
    prove_inner(image, loaded, budget, Some(cache))
}

fn prove_inner(
    image: &Image,
    loaded: &LoadedModule,
    budget: &mut AdmissionBudget,
    cache: Option<&mut AdmissionCache>,
) -> Result<AdmissionProof, ImageError> {
    budget.charge(admission_cost(image)?)?;
    let base_identity = loaded.identity().map_err(|_| {
        ImageError::admission(ImageReason::Code, "the program has no verified identity")
    })?;
    let code = match cache {
        Some(cache) if !image.installations.is_empty() => {
            CodeProof::Cached(cache.prepare(image, loaded)?)
        }
        _ => {
            let (aggregate, installations) = rebuild_aggregate(image, loaded)?;
            CodeProof::Owned(Box::new(aggregate), installations)
        }
    };
    let aggregate = code.aggregate();
    let installations = code.installations();
    let identity = aggregate.identity().map_err(|_| {
        ImageError::admission(
            ImageReason::Code,
            "the installed code has no verified identity",
        )
    })?;
    let module = aggregate.module();
    check_identity(image, identity)?;
    let tables = resolve_type_tables(image, module, aggregate.bundle())?;
    let admit = Admit {
        image,
        module,
        bundle: aggregate.bundle(),
        identity,
        installations,
        witness: tables,
    };
    admit.run()?;
    Ok(AdmissionProof {
        identity: AdmissionIdentity {
            base_semantic: base_identity.semantic_hash,
            base_verification: loaded.verification_hash(),
            module_semantic: identity.semantic_hash,
            verification: aggregate.verification_hash(),
            format: image.format,
            abi_version: image.abi_version,
            compiler_abi: image.compiler_abi,
            verifier_version: image.verifier_version,
            bundle_digest: aggregate.bundle().digest(),
        },
        loaded: aggregate.clone(),
    })
}

impl AdmissionCache {
    fn prepare(&mut self, image: &Image, base: &LoadedModule) -> Result<&CachedCode, ImageError> {
        let key = code_cache_key(image, base)?;
        if let Some(cached) = &self.code {
            if cached.key == key {
                return Ok(self.code.as_ref().expect("the cached code exists"));
            }
        }
        let (aggregate, installations) = rebuild_aggregate(image, base)?;
        self.code = Some(CachedCode {
            key,
            aggregate,
            installations,
        });
        Ok(self.code.as_ref().expect("the cached code exists"))
    }
}

fn code_cache_key(image: &Image, base: &LoadedModule) -> Result<CodeCacheKey, ImageError> {
    let mut artifacts = Vec::new();
    artifacts
        .try_reserve_exact(image.installations.len())
        .map_err(|_| {
            ImageError::admission(ImageReason::Budget, "the code cache allocation failed")
        })?;
    artifacts.extend(
        image
            .installations
            .iter()
            .map(|artifact| lm_bytecode::hash::hash256(artifact)),
    );
    let mut providers = Vec::new();
    providers
        .try_reserve_exact(image.installations.len())
        .map_err(|_| {
            ImageError::admission(ImageReason::Budget, "the code cache allocation failed")
        })?;
    for installation in 0..image.installations.len() {
        let instance = image
            .vm_images
            .iter()
            .flat_map(|image| &image.instances)
            .find(|instance| instance.installation as usize == installation);
        let instance = instance.ok_or_else(|| {
            ImageError::admission(
                ImageReason::Code,
                format!("installed artifact {installation} has no module instance"),
            )
        })?;
        let mut functions = Vec::new();
        functions
            .try_reserve_exact(instance.funcs.len())
            .map_err(|_| {
                ImageError::admission(ImageReason::Budget, "the code cache allocation failed")
            })?;
        functions.extend_from_slice(&instance.funcs);
        let mut classes = Vec::new();
        classes
            .try_reserve_exact(instance.classes.len())
            .map_err(|_| {
                ImageError::admission(ImageReason::Budget, "the code cache allocation failed")
            })?;
        classes.extend_from_slice(&instance.classes);
        providers.push((functions, classes));
    }
    Ok(CodeCacheKey {
        base_verification: base.verification_hash(),
        artifacts,
        providers,
    })
}

/// Rebuild and verify the aggregate code stated by an image.
fn rebuild_aggregate(
    image: &Image,
    base: &LoadedModule,
) -> Result<(LoadedModule, Vec<InstallationProof>), ImageError> {
    if image.installations.is_empty() {
        return Ok((base.clone(), Vec::new()));
    }
    let mut module = base.module().clone();
    let mut proofs = Vec::new();
    proofs
        .try_reserve_exact(image.installations.len())
        .map_err(|_| {
            ImageError::admission(
                ImageReason::Budget,
                "the installation proof allocation failed",
            )
        })?;
    for (index, bytes) in image.installations.iter().enumerate() {
        let addition = lm_bytecode::decode_with_bundle(bytes, base.bundle()).map_err(|error| {
            ImageError::admission(
                ImageReason::Code,
                format!("installed artifact {index} did not decode: {error}"),
            )
        })?;
        lm_verify::verify_module_with_bundle(&addition, base.bundle()).map_err(|error| {
            ImageError::admission(
                ImageReason::Code,
                format!("installed artifact {index} did not verify: {error}"),
            )
        })?;
        let source_identity =
            lm_bytecode::identity::module_identity_with_bundle(&addition, base.bundle()).map_err(
                |_| {
                    ImageError::admission(
                        ImageReason::Code,
                        format!("installed artifact {index} has no semantic identity"),
                    )
                },
            )?;
        let instance = image
            .vm_images
            .iter()
            .flat_map(|vm| &vm.instances)
            .find(|instance| instance.installation as usize == index)
            .ok_or_else(|| {
                ImageError::admission(
                    ImageReason::Code,
                    format!("installed artifact {index} has no module instance"),
                )
            })?;
        let mut imports = Vec::new();
        imports
            .try_reserve_exact(addition.imports.len())
            .map_err(|_| {
                ImageError::admission(ImageReason::Budget, "the resolved import allocation failed")
            })?;
        for import in &addition.imports {
            let target = if import.kind == lm_bytecode::ImportKind::Class {
                instance
                    .classes
                    .get(import.def as usize)
                    .copied()
                    .map(lm_bytecode::append::ResolvedImport::Class)
            } else {
                instance
                    .funcs
                    .get(import.def as usize)
                    .copied()
                    .map(lm_bytecode::append::ResolvedImport::Function)
            }
            .ok_or_else(|| {
                ImageError::admission(
                    ImageReason::Code,
                    format!("installed artifact {index} has an invalid import target"),
                )
            })?;
            imports.push(target);
        }
        let appended = lm_bytecode::append::append_resolved(&module, &addition, &imports).map_err(
            |error| {
                ImageError::admission(
                    ImageReason::Code,
                    format!("installed artifact {index} did not link: {error}"),
                )
            },
        )?;
        proofs.push(InstallationProof {
            semantic_hash: source_identity.semantic_hash,
            entry: appended.reloc.funcs[addition.entry as usize],
            reloc: appended.reloc,
            source: addition,
            source_identity,
        });
        module = appended.module;
    }
    let loaded = crate::load_with_bundle(module, base.bundle()).map_err(|error| {
        ImageError::admission(
            ImageReason::Code,
            format!("the installed aggregate did not verify: {error}"),
        )
    })?;
    Ok((loaded, proofs))
}

struct InstallationProof {
    semantic_hash: [u8; 32],
    entry: u32,
    reloc: lm_bytecode::append::AppendReloc,
    source: lm_bytecode::Module,
    source_identity: ModuleIdentity,
}

fn fail<T>(reason: ImageReason, detail: impl Into<String>) -> Result<T, ImageError> {
    Err(ImageError::admission(reason, detail))
}

/// Calculate the complete structural work of one image.
fn admission_cost(image: &Image) -> Result<u64, ImageError> {
    let mut cost = 1u64;
    let mut add = |units: usize| -> Result<(), ImageError> {
        let units = u64::try_from(units).map_err(|_| {
            ImageError::admission(ImageReason::Budget, "the admission work count overflowed")
        })?;
        cost = cost.checked_add(units).ok_or_else(|| {
            ImageError::admission(ImageReason::Budget, "the admission work count overflowed")
        })?;
        Ok(())
    };
    add(image.funcs.len())?;
    add(image.classes.len())?;
    add(image.installations.len())?;
    for artifact in &image.installations {
        add(artifact.len())?;
    }
    add(image.vm_images.len())?;
    for image in &image.vm_images {
        add(image.slots.len())?;
        add(image.instances.len())?;
        for instance in &image.instances {
            add(instance.interface.as_ref().map_or(0, Vec::len))?;
            add(instance.funcs.len())?;
            add(instance.classes.len())?;
            add(instance.slots.len())?;
        }
        for entry in &image.objects {
            let edges = object_edges(&entry.object);
            add(2usize.saturating_add(edges.saturating_mul(2)))?;
        }
    }
    for node in &image.types {
        add(1 + closed_type_parts(node))?;
    }
    for env in &image.envs {
        add(1 + env.types.len() + env.rows.len())?;
        for row in &env.rows {
            add(row.len())?;
        }
    }
    for machine in &image.machines {
        add(1)?;
        add(machine.frames.len())?;
        add(machine.callbacks.len())?;
        for callback in &machine.callbacks {
            add(callback.captures.len())?;
        }
        add(machine.locals.len())?;
        add(machine.operands.len())?;
        add(machine.literals.len())?;
        add(machine.mailbox.queue.len())?;
        if let Some(pending) = &machine.pending {
            add(pending.args.len())?;
        }
        if machine.routed.is_some() {
            add(image.machines.len())?;
        }
        for entry in &machine.objects {
            let edges = object_edges(&entry.object);
            add(2usize.saturating_add(edges.saturating_mul(2)))?;
        }
    }
    Ok(cost)
}

fn closed_type_parts(node: &ClosedType) -> usize {
    match node {
        ClosedType::Inst(_, args) | ClosedType::Tuple(args) => args.len(),
        ClosedType::Fn(params, markers, _, row) | ClosedType::Callback(params, markers, _, row) => {
            params
                .len()
                .saturating_add(markers.len())
                .saturating_add(row.len())
                .saturating_add(1)
        }
        ClosedType::List(_)
        | ClosedType::Run(_)
        | ClosedType::Op(_, _)
        | ClosedType::RunSnapshot(_) => 1,
        ClosedType::Map(_, _) | ClosedType::PendingCall(_, _) | ClosedType::Handle(_, _) => 2,
        _ => 0,
    }
}

fn object_edges(object: &Object) -> usize {
    match object {
        Object::Instance { fields, .. } => fields.len(),
        Object::List { items, .. } | Object::Tuple { items } => items.len(),
        Object::Map { index, .. } => index.live_len().saturating_mul(2),
        Object::Closure { captures, .. } => captures.len(),
        Object::DynValue { .. } => 1,
        _ => 0,
    }
}

fn image_object_values(object: &Object, out: &mut Vec<Value>) {
    match object {
        Object::Instance { fields, .. }
        | Object::List { items: fields, .. }
        | Object::Tuple { items: fields } => out.extend(fields.iter().copied()),
        Object::Map { entries, .. } => {
            for entry in entries {
                if !entry.is_live() {
                    continue;
                }
                out.push(entry.key);
                out.push(entry.value);
            }
        }
        Object::Closure { captures, .. } => out.extend(captures.iter().copied()),
        Object::DynValue { value, .. } => out.push(*value),
        Object::NativeSlotChange { target, .. } => out.push(*target),
        _ => {}
    }
}

fn work_vec<T>(count: usize) -> Result<Vec<T>, ImageError> {
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|_| {
        ImageError::admission(ImageReason::Budget, "an admission work allocation failed")
    })?;
    Ok(values)
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

/// The witness tables of one image, resolved against the program.
///
/// The image carries one closed type table and one environment table.
/// Admission proves the structure of both, and it keeps one canonical
/// copy for the content digests the header rule reads.
struct WitnessTables {
    /// The type arity and the row arity of every image environment
    /// ordinal.
    arity: Vec<(usize, usize)>,
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
    bundle: &'m std::sync::Arc<lm_abi::AbiBundle>,
    identity: &'m ModuleIdentity,
    installations: &'m [InstallationProof],
    /// The witness tables the image carries.
    witness: WitnessTables,
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
    bundle: &lm_abi::AbiBundle,
) -> Result<WitnessTables, ImageError> {
    let mut canonical = TypeEnvs::new(u32::MAX, u32::MAX);
    canonical
        .reserve_capacity(image.types.len(), image.envs.len())
        .map_err(|_| {
            ImageError::admission(ImageReason::Budget, "the witness table allocation failed")
        })?;
    let mut canonical_of: Vec<u32> = work_vec(image.types.len())?;
    let mut seen: HashSet<ClosedType> = HashSet::new();
    seen.try_reserve(image.types.len()).map_err(|_| {
        ImageError::admission(
            ImageReason::Budget,
            "the closed type index allocation failed",
        )
    })?;
    let class_named = |slot: u32| {
        image
            .classes
            .binary_search_by_key(&slot, |(s, _)| *s)
            .is_ok()
    };
    for (at, node) in image.types.iter().enumerate() {
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
                // A class states its own arity, so an application of
                // another width names no type the program can build.
                let arity = module.classes[*class as usize].type_params as usize;
                let held = match node {
                    ClosedType::Inst(_, args) => args.len(),
                    _ => 0,
                };
                if held != arity {
                    return fail(
                        ImageReason::Layout,
                        format!("closed type {at} applies class {class} with {held} arguments"),
                    );
                }
            }
            ClosedType::Op(op, _) => {
                if *op >= bundle.op_count() {
                    return fail(
                        ImageReason::Code,
                        format!("closed type {at} names operation slot {op}"),
                    );
                }
            }
            _ => {}
        }
        if let ClosedType::Fn(params, muts, _, row) | ClosedType::Callback(params, muts, _, row) =
            node
        {
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
    }
    // The environment table. Ordinal zero is the empty environment.
    let mut arity: Vec<(usize, usize)> = work_vec(image.envs.len())?;
    let mut seen_envs: HashSet<TypeEnv> = HashSet::new();
    seen_envs.try_reserve(image.envs.len()).map_err(|_| {
        ImageError::admission(
            ImageReason::Budget,
            "the environment index allocation failed",
        )
    })?;
    for (at, env) in image.envs.iter().enumerate() {
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
        let mut types = work_vec(env.types.len())?;
        types.extend(env.types.iter().map(|ty| canonical_of[*ty as usize]));
        let mut rows = work_vec(env.rows.len())?;
        for source in &env.rows {
            let mut row = work_vec(source.len())?;
            row.extend_from_slice(source);
            rows.push(row);
        }
        let mapped = TypeEnv { types, rows };
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
        arity.push((env.types.len(), env.rows.len()));
    }
    Ok(WitnessTables {
        arity,
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
    for pair in row.windows(2) {
        let first = &module.strings[pair[0] as usize];
        let second = &module.strings[pair[1] as usize];
        if first >= second {
            return fail(
                ImageReason::Layout,
                format!("entry {at} holds an effect row that is not canonical"),
            );
        }
    }
    Ok(())
}

impl Admit<'_> {
    fn run(&self) -> Result<(), ImageError> {
        if self.image.distinguished.is_some() == self.image.full_vm.is_some() {
            return fail(
                ImageReason::State,
                "a snapshot selects either one run or one full VM",
            );
        }
        if self.image.distinguished.is_some() && self.image.machines.is_empty() {
            return fail(ImageReason::State, "a run snapshot holds no machine");
        }
        if self.image.distinguished.is_some_and(|machine| machine != 0) {
            return fail(
                ImageReason::Layout,
                "the distinguished run is not machine ordinal zero",
            );
        }
        if self
            .image
            .distinguished
            .is_some_and(|machine| machine as usize >= self.image.machines.len())
        {
            return fail(
                ImageReason::Reference,
                "the distinguished run names no captured machine",
            );
        }
        if self
            .image
            .full_vm
            .is_some_and(|image| image as usize >= self.image.vm_images.len())
        {
            return fail(
                ImageReason::Reference,
                "the full VM selector names no captured VM image",
            );
        }
        self.check_code_manifest()?;
        self.check_instances()?;
        self.check_slot_state()?;
        for image in 0..self.image.vm_images.len() {
            self.check_vm_image_heap(image as u32)?;
        }
        for vm in 0..self.image.machines.len() {
            self.check_references(vm as u32)?;
            self.check_state(vm as u32)?;
            self.check_stop_points(vm as u32)?;
        }
        self.check_image_order()?;
        self.check_parent_forest()?;
        self.check_world()?;
        self.check_machine_witness()?;
        // The header repeats the selected result type, and the witness
        // of that machine derives it. The check runs after the
        // witness rules, so a damaged witness names its own rule.
        self.check_distinguished_result_type()?;
        // The canonical order runs last. Every earlier rule states a
        // property of one position, so a diagnostic names that
        // position instead of the traversal an edit moved.
        for (vm, machine) in self.image.machines.iter().enumerate() {
            self.check_order(machine, vm as u32)?;
            self.check_callback_order(machine, vm as u32)?;
        }
        Ok(())
    }

    fn machine(&self, vm: u32) -> &ImageMachine {
        &self.image.machines[vm as usize]
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

    /// Prove each module instance against its installation record.
    fn check_instances(&self) -> Result<(), ImageError> {
        for (image, vm) in self.image.vm_images.iter().enumerate() {
            for (index, instance) in vm.instances.iter().enumerate() {
                let Some(proof) = self.installations.get(instance.installation as usize) else {
                    return fail(
                        ImageReason::Code,
                        format!("VM image {image} instance {index} names no installation"),
                    );
                };
                if instance.semantic_hash != proof.semantic_hash
                    || instance.entry != proof.entry
                    || instance.funcs != proof.reloc.funcs
                    || instance.classes != proof.reloc.classes
                    || instance.slots != proof.reloc.slots
                {
                    return fail(
                        ImageReason::Code,
                        format!("VM image {image} instance {index} has invalid relocation"),
                    );
                }
                if let Some(bytes) = &instance.interface {
                    self.check_interface(
                        &proof.source,
                        &proof.source_identity,
                        bytes,
                        "a module instance",
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Prove one portable code value before restore can expose it.
    fn check_portable_code(
        &self,
        kind: PortableCodeKind,
        bytes: &[u8],
        interface: Option<&[u8]>,
        index: u32,
        origin: Option<[u8; 32]>,
    ) -> Result<(), ImageError> {
        if kind == PortableCodeKind::Artifact && origin.is_none() {
            return Ok(());
        }
        let module = lm_bytecode::decode_with_bundle(bytes, self.bundle).map_err(|error| {
            ImageError::admission(
                ImageReason::Code,
                format!("a portable code value did not decode: {error}"),
            )
        })?;
        lm_verify::verify_module_with_bundle(&module, self.bundle).map_err(|error| {
            ImageError::admission(
                ImageReason::Code,
                format!("a portable code value did not verify: {error}"),
            )
        })?;
        let identity = lm_bytecode::identity::module_identity_with_bundle(&module, self.bundle)
            .map_err(|error| {
                ImageError::admission(
                    ImageReason::Code,
                    format!("a portable code value did not hash: {error}"),
                )
            })?;
        if let Some(interface) = interface {
            self.check_interface(&module, &identity, interface, "a portable code value")?;
        }
        if kind == PortableCodeKind::SlotSpec && index as usize >= module.slots.len() {
            return fail(
                ImageReason::Code,
                "a portable slot specification names no source slot",
            );
        }
        if kind == PortableCodeKind::Function && index as usize >= module.funcs.len() {
            return fail(
                ImageReason::Code,
                "a portable function names no source function",
            );
        }
        if kind == PortableCodeKind::Class && index as usize >= module.classes.len() {
            return fail(ImageReason::Code, "a portable class names no source class");
        }
        if kind == PortableCodeKind::VerifiedModule && index != u32::MAX {
            return fail(
                ImageReason::Code,
                "a verified module value carries a source index",
            );
        }
        if let Some(origin) = origin {
            let debug = lm_bytecode::debug::decode(&module.debug).map_err(|error| {
                ImageError::admission(
                    ImageReason::Code,
                    format!("a portable source origin did not decode: {error}"),
                )
            })?;
            let matches = debug.definitions.iter().any(|definition| {
                definition.origin == origin
                    && match kind {
                        PortableCodeKind::Function => {
                            definition.kind == lm_bytecode::debug::DefinitionKind::Function
                                && definition.target == index
                        }
                        PortableCodeKind::Class => {
                            definition.kind == lm_bytecode::debug::DefinitionKind::Class
                                && definition.target == index
                        }
                        _ => false,
                    }
            });
            if !matches {
                return fail(
                    ImageReason::Code,
                    "a portable source origin does not match its code",
                );
            }
        }
        Ok(())
    }

    fn check_interface(
        &self,
        source: &lm_bytecode::Module,
        identity: &ModuleIdentity,
        bytes: &[u8],
        owner: &str,
    ) -> Result<(), ImageError> {
        let interface = lm_bytecode::interface::decode_interface(bytes).map_err(|error| {
            ImageError::admission(
                ImageReason::Code,
                format!("{owner} has an interface that did not decode: {error}"),
            )
        })?;
        if lm_bytecode::interface::encode_interface(&interface) != bytes {
            return fail(
                ImageReason::Code,
                format!("{owner} has noncanonical interface bytes"),
            );
        }
        lm_bytecode::interface::validate_interface_with_bundle(
            source,
            identity,
            &interface,
            self.bundle,
        )
        .map_err(|error| {
            ImageError::admission(
                ImageReason::Code,
                format!("{owner} has an invalid interface: {error}"),
            )
        })
    }

    /// Prove each captured target against its immutable module contract.
    fn check_slot_state(&self) -> Result<(), ImageError> {
        for (image, vm) in self.image.vm_images.iter().enumerate() {
            if vm.slots.len() != self.module.slots.len() || vm.slot_versions.len() != vm.slots.len()
            {
                return fail(
                    ImageReason::Code,
                    format!("VM image {image} has a different slot table length"),
                );
            }
            for (slot, target) in vm.slots.iter().enumerate() {
                let spec = &self.module.slots[slot];
                let valid = match (&spec.contract, target) {
                    (_, ImageSlotTarget::Empty) => true,
                    (SlotContract::Function(contract), ImageSlotTarget::Function(func)) => {
                        self.func_named(*func) && self.callable_matches(*func, contract, false)
                    }
                    (SlotContract::Method(contract), ImageSlotTarget::Function(func)) => {
                        self.func_named(*func) && self.callable_matches(*func, contract, true)
                    }
                    (
                        SlotContract::Class {
                            type_params,
                            abi: _,
                            ty,
                            constructor,
                        },
                        ImageSlotTarget::Class {
                            class,
                            constructor: target_constructor,
                        },
                    ) => {
                        let target = self.module.classes.get(*class as usize);
                        let contract_class = match self.module.types.get(*ty as usize) {
                            Some(BcType::Class(class)) | Some(BcType::Inst(class, _)) => {
                                Some(*class)
                            }
                            _ => None,
                        };
                        self.class_named(*class)
                            && target.is_some_and(|target| target.type_params == *type_params)
                            && contract_class == Some(*class)
                            && self.func_named(*target_constructor)
                            && self.callable_matches(*target_constructor, constructor, false)
                    }
                    (SlotContract::Value { .. }, ImageSlotTarget::Value(_)) => true,
                    (
                        SlotContract::Process { message, result },
                        ImageSlotTarget::Process { proc, generation },
                    ) => self.process_slot_matches(*proc, *generation, *message, *result),
                    _ => false,
                };
                if !valid {
                    return fail(
                        ImageReason::Code,
                        format!("VM image {image} slot {slot} has an incompatible target"),
                    );
                }
            }
        }
        Ok(())
    }

    /// Test one portable process target against its slot contract.
    fn process_slot_matches(&self, proc: u32, generation: u32, message: u32, result: u32) -> bool {
        let Some(machine) = self.image.machines.get(proc as usize) else {
            return false;
        };
        if machine.generation != generation || !machine.is_proc {
            return false;
        }
        let Some(body_index) = machine.body_func else {
            return false;
        };
        let Some(body) = self.module.funcs.get(body_index as usize) else {
            return false;
        };
        let Some(receiver) = body.params.first() else {
            return false;
        };
        let mut types = self.witness.canonical.borrow_mut();
        let Ok(expected_message) = types.close(self.module, message, lm_value::TypeEnvId::EMPTY)
        else {
            return false;
        };
        let Ok(expected_result) = types.close(self.module, result, lm_value::TypeEnvId::EMPTY)
        else {
            return false;
        };
        let env = lm_value::TypeEnvId(machine.witness);
        let Ok(actual_result) = types.close(self.module, body.ret, env) else {
            return false;
        };
        if actual_result != expected_result {
            return false;
        }
        let Ok(receiver) = types.close(self.module, *receiver, env) else {
            return false;
        };
        let Some((class, args)) = types.as_instance(receiver) else {
            return false;
        };
        let Some(proc_class) = lm_bytecode::corepin::declared_layout(self.module).proc_class else {
            return false;
        };
        types
            .ancestor_args(self.module, class, &args, proc_class)
            .is_some_and(|args| args.as_slice() == [expected_message])
    }

    /// Prove one frozen value-slot heap and its canonical order.
    fn check_vm_image_heap(&self, image: u32) -> Result<(), ImageError> {
        let record = &self.image.vm_images[image as usize];
        let objects = record.objects.len() as u32;
        let machines = self.image.machines.len() as u32;
        let at = |what: &str| format!("VM image {image}: {what}");
        let check_value = |value: Value, what: &str| -> Result<(), ImageError> {
            match value {
                Value::Obj(reference) if reference.generation != 0 => fail(
                    ImageReason::Reference,
                    at(&format!("{what} holds a nonzero object generation")),
                ),
                Value::Obj(reference) if reference.slot >= objects => fail(
                    ImageReason::Reference,
                    at(&format!("{what} names no value-slot object")),
                ),
                Value::Callback(_) => fail(
                    ImageReason::State,
                    at(&format!("{what} holds a nonescaping callback")),
                ),
                Value::EmptyCase { ty, arm }
                    if arm != 1 || ty as usize >= self.image.types.len() =>
                {
                    fail(
                        ImageReason::Reference,
                        at(&format!("{what} holds an invalid empty case")),
                    )
                }
                _ => Ok(()),
            }
        };
        let mut roots = Vec::new();
        for (slot, target) in record.slots.iter().enumerate() {
            if let ImageSlotTarget::Value(value) = target {
                check_value(*value, &format!("slot {slot}"))?;
                if let Value::Obj(reference) = value {
                    roots.push(reference.slot);
                }
            }
        }
        let mut children = Vec::new();
        for (ordinal, entry) in record.objects.iter().enumerate() {
            if !entry.frozen {
                return fail(
                    ImageReason::State,
                    at(&format!("object {ordinal} is not frozen")),
                );
            }
            if entry.object.shape().boundary == lm_heap::BoundaryPolicy::HolderLocal {
                return fail(
                    ImageReason::State,
                    at(&format!("object {ordinal} is holder-local")),
                );
            }
            children.clear();
            entry.object.children(&mut children);
            for child in &children {
                check_value(Value::Obj(*child), &format!("object {ordinal}"))?;
            }
            let mut values = Vec::new();
            image_object_values(&entry.object, &mut values);
            for value in values {
                check_value(value, &format!("object {ordinal}"))?;
            }
            if matches!(entry.object, Object::DynValue { ty, .. } if ty as usize >= self.image.types.len())
            {
                return fail(
                    ImageReason::Reference,
                    at(&format!("object {ordinal} names no closed type")),
                );
            }
            match &entry.object {
                Object::NativeHandle { proc, generation } => {
                    let Some(target) = self.image.machines.get(*proc as usize) else {
                        return fail(
                            ImageReason::Reference,
                            at(&format!("object {ordinal} names no process")),
                        );
                    };
                    if *proc >= machines || target.generation != *generation {
                        return fail(
                            ImageReason::Reference,
                            at(&format!("object {ordinal} holds a stale process handle")),
                        );
                    }
                }
                Object::Instance { class, fields, env } => {
                    if !self.class_named(*class)
                        || self
                            .module
                            .classes
                            .get(*class as usize)
                            .is_none_or(|class| class.fields.len() != fields.len())
                    {
                        return fail(
                            ImageReason::Code,
                            at(&format!("object {ordinal} has an invalid class layout")),
                        );
                    }
                    self.env_of(env.env().0)?;
                }
                Object::Closure {
                    func,
                    captures,
                    env,
                } => {
                    if !self.func_named(*func)
                        || self
                            .module
                            .funcs
                            .get(*func as usize)
                            .is_none_or(|body| body.captures.len() != captures.len())
                    {
                        return fail(
                            ImageReason::Code,
                            at(&format!("object {ordinal} has an invalid closure layout")),
                        );
                    }
                    self.env_of(env.env().0)?;
                }
                Object::NativeCode(code) => {
                    self.check_portable_code(
                        code.kind,
                        code.bytes.as_slice(),
                        code.interface.as_ref().map(|bytes| bytes.as_slice()),
                        code.index,
                        code.origin,
                    )?;
                }
                _ => {}
            }
        }
        let mut seen = work_vec(record.objects.len())?;
        seen.resize(record.objects.len(), false);
        let mut stack = work_vec(roots.len())?;
        stack.extend(roots.iter().rev().copied());
        let mut next = 0usize;
        while let Some(ordinal) = stack.pop() {
            let ordinal = ordinal as usize;
            if seen[ordinal] {
                continue;
            }
            if ordinal != next {
                return fail(
                    ImageReason::Order,
                    at(&format!("object {ordinal} appears before object {next}")),
                );
            }
            seen[ordinal] = true;
            next += 1;
            children.clear();
            record.objects[ordinal].object.children(&mut children);
            stack.extend(children.iter().rev().map(|child| child.slot));
        }
        if next != record.objects.len() {
            return fail(
                ImageReason::Order,
                at("the value-slot heap holds an unreachable object"),
            );
        }
        Ok(())
    }

    fn callable_matches(
        &self,
        target: u32,
        contract: &lm_bytecode::BcCallableContract,
        method: bool,
    ) -> bool {
        let Some(func) = self.module.funcs.get(target as usize) else {
            return false;
        };
        let is_method = self
            .module
            .classes
            .iter()
            .any(|class| class.methods.iter().any(|(_, func)| *func == target));
        (!method || is_method)
            && func.captures.is_empty()
            && func.type_params == contract.type_params
            && func.effect_params == contract.effect_params
            && self.module.func_bounds.get(target as usize) == Some(&contract.type_bounds)
            && func.params == contract.params
            && func.param_muts == contract.param_muts
            && func.ret == contract.ret
            && func.row == contract.row
    }

    /// The header names the selected run result type.
    ///
    /// The header holds the canonical content digest of the closed
    /// type, so it names a class by definition hash and never by a
    /// numeric slot of one linked program.
    fn check_distinguished_result_type(&self) -> Result<(), ImageError> {
        match self.image.distinguished {
            Some(machine) => {
                let found = self.machine_result_digest(machine);
                if found != self.image.result_type {
                    return fail(
                        ImageReason::State,
                        "the header and the distinguished run name two result types",
                    );
                }
            }
            None if self.image.result_type != [0u8; 32] => {
                return fail(
                    ImageReason::State,
                    "a full VM snapshot carries a run result type",
                );
            }
            None => {}
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

    /// Prove the canonical first-reference order of VM images.
    fn check_image_order(&self) -> Result<(), ImageError> {
        let mut next = 0u32;
        let mut seen = vec![false; self.image.vm_images.len()];
        let mut visit = |image: u32| -> Result<(), ImageError> {
            let Some(slot) = seen.get_mut(image as usize) else {
                return fail(
                    ImageReason::Reference,
                    format!("VM image ordinal {image} names no captured image"),
                );
            };
            if *slot {
                return Ok(());
            }
            if image != next {
                return fail(
                    ImageReason::Layout,
                    format!("VM image ordinal {image} appears before ordinal {next}"),
                );
            }
            *slot = true;
            next += 1;
            Ok(())
        };
        if let Some(image) = self.image.full_vm {
            visit(image)?;
        }
        for machine in &self.image.machines {
            if let Some(image) = machine.image {
                visit(image)?;
            }
            for entry in &machine.objects {
                match entry.object {
                    Object::NativeVm { image, .. }
                    | Object::NativeCodeHandle { image, .. }
                    | Object::NativeSlotChange { image, .. } => visit(image)?,
                    _ => {}
                }
            }
        }
        if next as usize != self.image.vm_images.len() {
            return fail(
                ImageReason::Layout,
                "the VM image table holds an unreferenced entry",
            );
        }
        Ok(())
    }

    /// later rule follows a reference.
    fn check_references(&self, vm: u32) -> Result<(), ImageError> {
        let m = self.machine(vm);
        let objects = m.objects.len() as u32;
        let callbacks = m.callbacks.len() as u32;
        let machines = self.image.machines.len() as u32;
        let images = self.image.vm_images.len() as u32;
        let at = |what: &str| format!("machine {vm}: {what}");
        if m.image.is_some_and(|image| image >= images) {
            return fail(
                ImageReason::Reference,
                at("the owning VM image ordinal names no captured image"),
            );
        }
        let object_ref = |value: &Value, what: &str| -> Result<(), ImageError> {
            match value {
                Value::Obj(r) if r.generation != 0 => fail(
                    ImageReason::Reference,
                    at(&format!(
                        "{what} holds generation {}, and an image reference requires zero",
                        r.generation
                    )),
                ),
                Value::Obj(r) if r.slot >= objects => fail(
                    ImageReason::Reference,
                    at(&format!(
                        "{what} names object ordinal {} of {objects}",
                        r.slot
                    )),
                ),
                Value::Callback(reference) if reference.generation != 0 => fail(
                    ImageReason::Reference,
                    at(&format!(
                        "{what} holds callback generation {}, and an image reference requires zero",
                        reference.generation
                    )),
                ),
                Value::Callback(reference) if reference.slot >= callbacks => fail(
                    ImageReason::Reference,
                    at(&format!(
                        "{what} names callback ordinal {} of {callbacks}",
                        reference.slot
                    )),
                ),
                Value::EmptyCase { ty, arm } => {
                    let option = self.module.core_roles[lm_bytecode::corepin::ROLE_OPTION];
                    let valid = *arm == 1
                        && self.image.types.get(*ty as usize).is_some_and(|node| {
                            matches!(node, ClosedType::Inst(class, args)
                                if *class == option && args.len() == 1)
                        });
                    if valid {
                        Ok(())
                    } else {
                        fail(
                            ImageReason::Reference,
                            at(&format!("{what} holds an invalid empty case")),
                        )
                    }
                }
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
        if m.nested.is_some_and(|target| target >= machines) {
            return fail(
                ImageReason::Reference,
                at("the nested machine ordinal names no captured machine"),
            );
        }
        if let Some(route) = m.routed {
            if route.target >= machines {
                return fail(
                    ImageReason::Reference,
                    at("the routed request target names no captured machine"),
                );
            }
            if matches!(route.cursor, ImagePolicyCursor::Table(table) if table >= machines) {
                return fail(
                    ImageReason::Reference,
                    at("the policy cursor names no captured machine"),
                );
            }
        }
        let mut children = Vec::new();
        for (ordinal, entry) in m.objects.iter().enumerate() {
            children.clear();
            let want = object_edges(&entry.object);
            if want > children.capacity() {
                children.try_reserve_exact(want).map_err(|_| {
                    ImageError::admission(
                        ImageReason::Budget,
                        "the object child work allocation failed",
                    )
                })?;
            }
            entry.object.children(&mut children);
            for child in &children {
                if child.generation != 0 {
                    return fail(
                        ImageReason::Reference,
                        at(&format!(
                            "object {ordinal} holds generation {}, and an image reference requires zero",
                            child.generation
                        )),
                    );
                }
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
            if matches!(entry.object, Object::DynValue { ty, .. } if ty as usize >= self.image.types.len())
            {
                return fail(
                    ImageReason::Reference,
                    at(&format!("object {ordinal} names no closed type")),
                );
            }
            if let Object::NativeVm { image, generation } = entry.object {
                if generation != 0 {
                    return fail(
                        ImageReason::Reference,
                        at(&format!(
                            "object {ordinal} holds VM image generation {generation}, and a portable image requires zero"
                        )),
                    );
                }
                if image >= images {
                    return fail(
                        ImageReason::Reference,
                        at(&format!(
                            "object {ordinal} names VM image ordinal {image} of {images}"
                        )),
                    );
                }
            }
            if let Object::NativeCodeHandle {
                image,
                generation,
                instance,
                kind,
                index,
            } = entry.object
            {
                if generation != 0 || image >= images {
                    return fail(
                        ImageReason::Reference,
                        at(&format!(
                            "object {ordinal} holds an invalid code image handle"
                        )),
                    );
                }
                let Some(record) = self.image.vm_images[image as usize]
                    .instances
                    .get(instance as usize)
                else {
                    return fail(
                        ImageReason::Reference,
                        at(&format!("object {ordinal} names no module instance")),
                    );
                };
                let valid = match kind {
                    CodeHandleKind::Instance => index == instance,
                    CodeHandleKind::Function => {
                        record.funcs.contains(&index) && self.func_named(index)
                    }
                    CodeHandleKind::Class => {
                        record.classes.contains(&index) && self.class_named(index)
                    }
                    CodeHandleKind::Slot => {
                        record.slots.contains(&index) && (index as usize) < self.module.slots.len()
                    }
                    CodeHandleKind::FunctionBinding => {
                        let source_slot = usize::try_from(index).ok();
                        let mapped = source_slot
                            .and_then(|source_slot| record.slots.get(source_slot))
                            .and_then(|slot| self.module.slots.get(*slot as usize));
                        let source = self
                            .installations
                            .get(record.installation as usize)
                            .map(|proof| &proof.source);
                        matches!(
                            (source_slot, source, mapped),
                            (
                                Some(source_slot),
                                Some(source),
                                Some(lm_bytecode::SlotSpec {
                                    contract: SlotContract::Function(_) | SlotContract::Method(_),
                                    ..
                                })
                            ) if matches!(
                                source.slots.get(source_slot).and_then(|slot| slot.initial),
                                Some(lm_bytecode::SlotTarget::Function(function))
                                    if record.funcs.get(function as usize)
                                        .is_some_and(|target| self.func_named(*target))
                            )
                        )
                    }
                    CodeHandleKind::ClassBinding => {
                        let source_slot = usize::try_from(index).ok();
                        let mapped = source_slot
                            .and_then(|source_slot| record.slots.get(source_slot))
                            .and_then(|slot| self.module.slots.get(*slot as usize));
                        let source = self
                            .installations
                            .get(record.installation as usize)
                            .map(|proof| &proof.source);
                        matches!(
                            (source_slot, source, mapped),
                            (
                                Some(source_slot),
                                Some(source),
                                Some(lm_bytecode::SlotSpec {
                                    contract: SlotContract::Class { .. },
                                    ..
                                })
                            ) if matches!(
                                source.slots.get(source_slot).and_then(|slot| slot.initial),
                                Some(lm_bytecode::SlotTarget::Class { class, constructor })
                                    if record.classes.get(class as usize)
                                        .is_some_and(|target| self.class_named(*target))
                                        && record.funcs.get(constructor as usize)
                                            .is_some_and(|target| self.func_named(*target))
                            )
                        )
                    }
                };
                if !valid {
                    return fail(
                        ImageReason::Code,
                        at(&format!(
                            "object {ordinal} holds an invalid installed code handle"
                        )),
                    );
                }
            }
            if let Object::NativeSlotChange {
                image,
                generation,
                slot,
                ..
            } = entry.object
            {
                if generation != 0 || image >= images {
                    return fail(
                        ImageReason::Reference,
                        at(&format!(
                            "object {ordinal} holds an invalid slot change image"
                        )),
                    );
                }
                if self.image.vm_images[image as usize]
                    .slots
                    .get(slot as usize)
                    .is_none()
                {
                    return fail(
                        ImageReason::Reference,
                        at(&format!("object {ordinal} names no image slot")),
                    );
                }
            }
            if let Object::NativeCode(code) = &entry.object {
                self.check_portable_code(
                    code.kind,
                    code.bytes.as_slice(),
                    code.interface.as_ref().map(|bytes| bytes.as_slice()),
                    code.index,
                    code.origin,
                )?;
            }
            if let Object::NativeFault { trace, .. } = &entry.object {
                self.check_fault_trace(trace, &at(&format!("object {ordinal}")))?;
            }
            let target = match entry.object {
                Object::NativeRun { vm } | Object::NativeTable { vm } => Some(vm),
                Object::NativeRequest { vm, .. } | Object::NativeCall { vm, .. } => Some(vm),
                Object::NativeHandle { proc, .. } => Some(proc),
                Object::NativeResourceHandle { surface, .. } => Some(surface),
                Object::NativeWait { owner, .. } => Some(owner),
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
            if matches!(entry.object, Object::NativeRun { vm: target } if target == vm) {
                return fail(
                    ImageReason::Reference,
                    at(&format!(
                        "object {ordinal} is a run handle to its own machine"
                    )),
                );
            }
            if matches!(entry.object, Object::NativeWait { owner, .. } if owner != vm) {
                return fail(
                    ImageReason::Reference,
                    at(&format!("object {ordinal} holds another machine's wait")),
                );
            }
            if matches!(
                entry.object,
                Object::NativeWait { token, .. } if token == 0 || token >= m.next_wait
            ) {
                return fail(
                    ImageReason::State,
                    at(&format!("object {ordinal} holds an invalid wait token")),
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
            if matches!(
                entry.object,
                Object::NativeFileHandle { resource }
                    | Object::NativeResourceHandle { resource, .. }
                    | Object::NativeTcpStream { resource }
                    | Object::NativeTcpListener { resource }
                    | Object::NativeTlsStream { resource }
                    | Object::NativeHostResource { resource, .. }
                    if resource != 0
            ) {
                return fail(
                    ImageReason::State,
                    at(&format!(
                        "object {ordinal} carries a live resource identifier"
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
                if op >= self.bundle.op_count() {
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
                    let held = self.env_of(env.env().0)?.0;
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
                    if held.0 != body.type_params as usize || held.1 != body.effect_params as usize
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
            self.env_of(frame.env).map_err(|mut error| {
                error.detail = at(&format!(
                    "frame {idx} names environment {}, which the image has not",
                    frame.env
                ));
                error
            })?;
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
            if let Some(closure) = &frame.closure {
                object_ref(closure, &format!("frame {idx} capture context"))?;
            }
        }
        for (idx, callback) in m.callbacks.iter().enumerate() {
            if callback.func as usize >= self.module.funcs.len() || !self.func_named(callback.func)
            {
                return fail(
                    ImageReason::Code,
                    at(&format!("callback {idx} names no function")),
                );
            }
            let target = &self.module.funcs[callback.func as usize];
            if callback.captures.len() != target.captures.len() {
                return fail(
                    ImageReason::Layout,
                    at(&format!("callback {idx} has another capture count")),
                );
            }
            self.env_of(callback.env)?;
            if callback.owner_depth == 0 || callback.owner_depth as usize > m.frames.len() {
                return fail(
                    ImageReason::Layout,
                    at(&format!("callback {idx} has an invalid owner depth")),
                );
            }
            for (capture, value) in callback.captures.iter().enumerate() {
                if matches!(value, Value::Callback(_)) {
                    return fail(
                        ImageReason::State,
                        at(&format!("callback {idx} captures another callback")),
                    );
                }
                object_ref(value, &format!("callback {idx} capture {capture}"))?;
            }
        }
        for (idx, value) in m.locals.iter().enumerate() {
            object_ref(value, &format!("local {idx}"))?;
        }
        for (idx, value) in m.operands.iter().enumerate() {
            object_ref(value, &format!("operand {idx}"))?;
        }
        if let Some(pending) = &m.pending {
            if pending.op >= self.bundle.op_count() {
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
                if rec.op.is_some_and(|op| op >= self.bundle.op_count()) {
                    return fail(
                        ImageReason::Code,
                        at("the terminal fault names no manifest operation"),
                    );
                }
                self.check_fault_trace(&rec.trace, &at("the terminal fault"))?;
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
                Object::Str(text) if text.as_str() == self.module.strings[idx] => {}
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
        let block_target = match m.block {
            Some(ImageBlock::Send { target })
            | Some(ImageBlock::Done { target })
            | Some(ImageBlock::Snapshot { target, .. }) => Some(target),
            _ => None,
        };
        if let Some(target) = block_target {
            if target >= machines {
                return fail(
                    ImageReason::Reference,
                    at("a block names no captured machine"),
                );
            }
        }
        Ok(())
    }

    fn check_fault_trace(
        &self,
        trace: &[lm_heap::FaultSite],
        context: &str,
    ) -> Result<(), ImageError> {
        if trace.len() > 64 {
            return fail(
                ImageReason::LimitExceeded,
                format!("{context} has more than 64 fault locations"),
            );
        }
        for (index, site) in trace.iter().enumerate() {
            let Some(function) = self.module.funcs.get(site.function as usize) else {
                return fail(
                    ImageReason::Code,
                    format!("{context} location {index} names no function"),
                );
            };
            if !self.func_named(site.function) {
                return fail(
                    ImageReason::Code,
                    format!("{context} location {index} names an omitted function"),
                );
            }
            let Some(block) = function.blocks.get(site.block as usize) else {
                return fail(
                    ImageReason::Layout,
                    format!("{context} location {index} names no block"),
                );
            };
            if site.instruction as usize >= block.len() {
                return fail(
                    ImageReason::Layout,
                    format!("{context} location {index} names no instruction"),
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
            match closure {
                // The frame runs that closure, so the two carry one
                // environment. A frame that named another environment
                // would read its captures under a substitution the
                // closure never held.
                Value::Obj(reference) => match m.objects[reference.slot as usize].object {
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
                },
                Value::Callback(reference) => {
                    let callback = &m.callbacks[reference.slot as usize];
                    if callback.func != frame.func || callback.env != frame.env {
                        return fail(
                            ImageReason::Reference,
                            at(&format!(
                                "frame {idx} names a capture context that is not its own callback"
                            )),
                        );
                    }
                }
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
                if !m.frames.is_empty()
                    || m.pending.is_some()
                    || m.nested.is_some()
                    || m.routed.is_some()
                    || m.terminal.is_some()
                {
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
                if m.pending.is_some() != m.nested.is_some() {
                    return fail(
                        ImageReason::State,
                        at("a ready machine has an incomplete nested control edge"),
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
                if m.nested.is_some() || m.routed.is_some() {
                    return fail(
                        ImageReason::State,
                        at("an asked or blocked machine holds nested control state"),
                    );
                }
            }
            ImageState::Done | ImageState::Faulted => {
                if m.pending.is_some() || m.nested.is_some() || m.routed.is_some() {
                    return fail(
                        ImageReason::State,
                        at("a terminal machine holds live control state"),
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
        if let Some(target) = m.nested {
            if target == vm {
                return fail(
                    ImageReason::State,
                    at("a nested control edge targets its own machine"),
                );
            }
            let Some(pending) = m.pending.as_ref() else {
                return fail(
                    ImageReason::State,
                    at("a nested control edge has no pending operation"),
                );
            };
            if !matches!(
                pending.op,
                lm_abi::OP_VM_RUN | lm_abi::OP_VM_STEP | lm_abi::OP_VM_DRIVE
            ) {
                return fail(
                    ImageReason::State,
                    at("a nested control edge names another operation"),
                );
            }
            let receiver_matches = match pending.args.first() {
                Some(Value::Obj(reference)) => m.objects.get(reference.slot as usize).is_some_and(
                    |entry| matches!(entry.object, Object::NativeRun { vm: held } if held == target),
                ),
                _ => false,
            };
            if !receiver_matches {
                return fail(
                    ImageReason::State,
                    at("a nested control edge does not match its VM receiver"),
                );
            }
            let child = self.machine(target);
            if child.parent != Some(vm) {
                return fail(
                    ImageReason::State,
                    at("a nested control edge does not name a direct child"),
                );
            }
            if child.scheduler_owned {
                return fail(
                    ImageReason::State,
                    at("a nested control edge names a scheduler-owned machine"),
                );
            }
        }
        if let Some(route) = m.routed {
            if route.target == vm {
                return fail(
                    ImageReason::State,
                    at("a routed request targets its surface machine"),
                );
            }
            let target = self.machine(route.target);
            if target.state != ImageState::Asked || target.pending.is_none() {
                return fail(
                    ImageReason::State,
                    at("a routed request target holds no live request"),
                );
            }
            let cursor_matches = match route.cursor {
                ImagePolicyCursor::Table(table) => m.parent == Some(table),
                ImagePolicyCursor::Binding | ImagePolicyCursor::Root => m.parent.is_none(),
            };
            if !cursor_matches {
                return fail(
                    ImageReason::State,
                    at("a routed request has an invalid policy cursor"),
                );
            }
            let mut next = m.nested;
            let mut found = false;
            for _ in 0..self.image.machines.len() {
                let Some(current) = next else { break };
                if current == route.target {
                    found = true;
                    break;
                }
                let Some(machine) = self.image.machines.get(current as usize) else {
                    return fail(
                        ImageReason::Reference,
                        at("the routed request crosses an invalid nested edge"),
                    );
                };
                next = machine.nested;
            }
            if !found {
                return fail(
                    ImageReason::State,
                    at("a routed request target is not a nested descendant"),
                );
            }
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
                    ImageBlock::Wait { .. } => op == lm_abi::OP_WAIT_WAIT,
                    ImageBlock::Snapshot { .. } => op == lm_abi::OP_PROC_SNAPSHOT_WAIT,
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
        //
        // `Machine::new` starts the request counter at one, so the
        // runtime mints no ordinal zero. A container that states zero
        // gives a restored machine one request the runtime cannot
        // produce, and later code reads the counter as a live value.
        // Both fields therefore take a lower bound here.
        if m.next_ordinal == 0 {
            return fail(ImageReason::State, at("the next request ordinal is zero"));
        }
        if let Some(pending) = &m.pending {
            if pending.ordinal == 0 {
                return fail(
                    ImageReason::State,
                    at("the pending request ordinal is zero"),
                );
            }
            if pending.ordinal >= m.next_ordinal {
                return fail(
                    ImageReason::State,
                    at("the pending request ordinal is not below the next ordinal"),
                );
            }
        }
        if m.next_wait == 0 {
            return fail(ImageReason::State, at("the next wait token is zero"));
        }
        if m.waits.len() > crate::machine::MAX_LIVE_WAITS {
            return fail(ImageReason::State, at("the wait table passes its limit"));
        }
        let tokens = m.waits.iter().map(|wait| wait.token).collect::<Vec<_>>();
        let mut previous = 0;
        for wait in &m.waits {
            if wait.token == 0 || wait.token >= m.next_wait {
                return fail(
                    ImageReason::State,
                    at("a wait token is outside its counter"),
                );
            }
            if wait.token <= previous {
                return fail(
                    ImageReason::State,
                    at("the wait table is not strictly ordered"),
                );
            }
            previous = wait.token;
            match wait.source {
                ImageWaitSource::Receive if !m.is_proc => {
                    return fail(
                        ImageReason::State,
                        at("a non-proc machine holds a receive wait"),
                    )
                }
                ImageWaitSource::Drive { target } => {
                    if target == vm || target as usize >= self.image.machines.len() {
                        return fail(ImageReason::Reference, at("a drive wait has no child"));
                    }
                }
                _ => {}
            }
        }
        let mut parents = vec![0u8; m.waits.len()];
        for wait in &m.waits {
            let ImageWaitSource::Choice { first, second } = wait.source else {
                continue;
            };
            if first == second || first >= wait.token || second >= wait.token {
                return fail(ImageReason::State, at("a choice has invalid child tokens"));
            }
            for child in [first, second] {
                let Ok(index) = tokens.binary_search(&child) else {
                    return fail(ImageReason::State, at("a choice names no wait child"));
                };
                parents[index] = parents[index].saturating_add(1);
                if parents[index] > 1 {
                    return fail(ImageReason::State, at("two choices share one wait child"));
                }
            }
        }
        for (wait, parents) in m.waits.iter().zip(&parents) {
            if wait.linked != (*parents == 1) {
                return fail(
                    ImageReason::State,
                    at("a wait link flag has no matching choice"),
                );
            }
        }
        if let Some(ImageBlock::Wait { token }) = m.block {
            let Ok(root) = tokens.binary_search(&token) else {
                return fail(ImageReason::State, at("the active wait token is absent"));
            };
            if m.waits[root].linked {
                return fail(
                    ImageReason::State,
                    at("the active wait token is a choice child"),
                );
            }
            let receiver_matches = match m.pending.as_ref().and_then(|p| p.args.first()) {
                Some(Value::Obj(reference)) => {
                    m.objects.get(reference.slot as usize).is_some_and(|entry| {
                        matches!(
                            entry.object,
                            Object::NativeWait { owner, token: held }
                                if owner == vm && held == token
                        )
                    })
                }
                _ => false,
            };
            if !receiver_matches {
                return fail(
                    ImageReason::State,
                    at("the active wait does not match its receiver"),
                );
            }
            let mut stack = vec![token];
            let mut seen = HashSet::new();
            let mut drives = HashSet::new();
            while let Some(current) = stack.pop() {
                if !seen.insert(current) {
                    return fail(ImageReason::State, at("the active wait tree has a cycle"));
                }
                let index = tokens.binary_search(&current).map_err(|_| {
                    ImageError::new(ImageReason::State, at("a wait child is absent"))
                })?;
                match m.waits[index].source {
                    ImageWaitSource::Receive => {}
                    ImageWaitSource::Drive { target } => {
                        if !drives.insert(target) {
                            return fail(
                                ImageReason::State,
                                at("the active wait drives one child twice"),
                            );
                        }
                        let target = self.machine(target);
                        if target.parent != Some(vm)
                            || target.scheduler_owned
                            || target.state == ImageState::Empty
                        {
                            return fail(
                                ImageReason::State,
                                at("the active drive wait has no available child"),
                            );
                        }
                    }
                    ImageWaitSource::Choice { first, second } => {
                        stack.push(second);
                        stack.push(first);
                    }
                }
            }
        }
        if let Some(ImageBlock::Snapshot { target, .. }) = m.block {
            if target == vm {
                return fail(ImageReason::State, at("a snapshot wait targets its caller"));
            }
            let target_machine = self.machine(target);
            if !target_machine.scheduler_owned || target_machine.paused {
                return fail(
                    ImageReason::State,
                    at("a snapshot wait target is not a running proc"),
                );
            }
            let pending = m.pending.as_ref().expect("the block check found a request");
            let receiver_matches = match pending.args.first() {
                Some(Value::Obj(reference)) => m.objects.get(reference.slot as usize).is_some_and(
                    |entry| matches!(entry.object, Object::NativeHandle { proc, .. } if proc == target),
                ),
                _ => false,
            };
            if !receiver_matches || !matches!(pending.args.get(1), Some(Value::Int(_))) {
                return fail(
                    ImageReason::State,
                    at("a snapshot wait does not match its arguments"),
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
        if self.image.distinguished == Some(vm) && (m.scheduler_owned || m.paused) {
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
    /// walk of `resolve_policy` fail closed. The walk below is
    /// iterative, so it never grows the Rust stack.
    fn check_parent_forest(&self) -> Result<(), ImageError> {
        let n = self.image.machines.len();
        // 0 unvisited, 1 on the current path, 2 settled.
        let mut colour = work_vec(n)?;
        colour.resize(n, 0u8);
        let mut path = work_vec(n)?;
        for start in 0..n {
            if colour[start] != 0 {
                continue;
            }
            path.clear();
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
            for node in &path {
                colour[*node] = 2;
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
                // Allocation advances the counter before a token can
                // enter a heap. A future ordinal cannot be valid data.
                if request >= target.next_ordinal {
                    return fail(
                        ImageReason::State,
                        format!(
                            "machine {vm} object {ordinal} holds future request ordinal {request}"
                        ),
                    );
                }
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
        let mut seen = work_vec(count)?;
        seen.resize(count, false);
        let mut next = 0usize;
        let roots = image_roots(machine);
        let mut stack = work_vec(roots.len())?;
        stack.extend(roots.iter().rev().copied());
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
            let want = object_edges(&machine.objects[idx].object);
            if want > children.capacity() {
                children.try_reserve_exact(want).map_err(|_| {
                    ImageError::admission(
                        ImageReason::Budget,
                        "the order child work allocation failed",
                    )
                })?;
            }
            machine.objects[idx].object.children(&mut children);
            stack.try_reserve(children.len()).map_err(|_| {
                ImageError::admission(ImageReason::Budget, "the order stack allocation failed")
            })?;
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

    /// Prove canonical callback order and reachability.
    fn check_callback_order(&self, machine: &ImageMachine, vm: u32) -> Result<(), ImageError> {
        let count = machine.callbacks.len();
        let mut seen = work_vec(count)?;
        seen.resize(count, false);
        let mut values = Vec::new();
        for frame in &machine.frames {
            if let Some(value) = frame.closure {
                values.push(value);
            }
        }
        values.extend(machine.locals.iter().copied());
        values.extend(machine.operands.iter().copied());
        if let Some(pending) = &machine.pending {
            values.extend(pending.args.iter().copied());
        }
        if let Some(ImageTerminal::Done(value)) = machine.terminal {
            values.push(value);
        }
        values.extend(machine.mailbox.queue.iter().copied());
        let mut next = 0usize;
        let mut cursor = 0usize;
        while cursor < values.len() {
            let value = values[cursor];
            cursor += 1;
            let Value::Callback(reference) = value else {
                continue;
            };
            let index = reference.slot as usize;
            if seen[index] {
                continue;
            }
            if index != next {
                return fail(
                    ImageReason::Order,
                    format!(
                        "machine {vm}: callback traversal reaches {index} where canonical order needs {next}"
                    ),
                );
            }
            seen[index] = true;
            next += 1;
            values.extend(machine.callbacks[index].captures.iter().copied());
        }
        if next != count {
            return fail(
                ImageReason::Order,
                format!("machine {vm}: {} callbacks are unreachable", count - next),
            );
        }
        Ok(())
    }

    /// The type arity and the row arity of one image environment
    /// ordinal.
    fn env_of(&self, env: u32) -> Result<(usize, usize), ImageError> {
        self.witness
            .arity
            .get(env as usize)
            .copied()
            .ok_or_else(|| {
                ImageError::admission(
                    ImageReason::Reference,
                    format!("a witness names environment {env}, which the image has not"),
                )
            })
    }

    /// Prove the structural rules of the machine witness.
    ///
    /// The stored proc body states the body function and its
    /// environment directly. A machine with no proc body runs its own
    /// body in its bottom frame, so that frame states them instead. A
    /// terminal machine holds neither, and the witness stands alone
    /// there.
    ///
    /// A machine that claims the proc birth grant must name a proc
    /// class through the first parameter of its body. The rule reads
    /// the class table alone, so it derives no type from the image.
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
            self.env_of(machine.witness)?;
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
                        ImageReason::State,
                        at("the machine witness does not match its body"),
                    );
                }
            }
            if machine.body_func.is_none() && machine.witness != 0 {
                return fail(
                    ImageReason::State,
                    at("a machine with no body function names an environment"),
                );
            }
            if machine.is_proc && !self.body_takes_a_proc(machine) {
                return fail(
                    ImageReason::Mailbox,
                    at("a machine that claims the proc grant names no proc class"),
                );
            }
        }
        Ok(())
    }

    /// True when the body function of one machine takes a proc
    /// instance as its first parameter.
    ///
    /// `Proc.Spawn` calls the body over the constructed instance, so a
    /// machine with the proc birth grant has one. The rule reads the
    /// class table of the program, never the image.
    fn body_takes_a_proc(&self, machine: &ImageMachine) -> bool {
        let Some(proc_class) = self.proc_class() else {
            return false;
        };
        let Some(func) = machine.body_func else {
            return false;
        };
        let Some(body) = self.module.funcs.get(func as usize) else {
            return false;
        };
        let Some(first) = body.params.first() else {
            return false;
        };
        let class = match self.module.types.get(*first as usize) {
            Some(BcType::Class(c)) | Some(BcType::Inst(c, _)) => *c,
            _ => return false,
        };
        self.class_extends(class, proc_class)
    }

    /// The core `Proc` class slot the artifact declares.
    ///
    /// The verifier proved the shape of every filled role slot, so the
    /// answer names the core class and never a class of the image.
    fn proc_class(&self) -> Option<u32> {
        lm_bytecode::corepin::declared_layout(self.module).proc_class
    }

    /// True when `child` equals `ancestor` or inherits it.
    fn class_extends(&self, mut child: u32, ancestor: u32) -> bool {
        for _ in 0..=self.module.classes.len() {
            if child == ancestor {
                return true;
            }
            match self
                .module
                .classes
                .get(child as usize)
                .and_then(|c| c.parent())
            {
                Some(parent) => child = parent,
                None => return false,
            }
        }
        false
    }

    /// Prove that every stopped frame stopped where the runtime stops.
    ///
    /// A frame stops at one of two points. The top frame of a machine
    /// with no pending request stops before the instruction its
    /// program counter names. Every other frame, and the top frame of
    /// a machine with a pending request, stopped inside the
    /// instruction before the counter.
    ///
    /// A call pushes the frame above. A perform records the pending
    /// request. No other instruction leaves a frame stopped, so an
    /// image that names one states a stop the runtime never reaches.
    ///
    /// The rule reads the instruction alone. It derives no type, and
    /// the boundary check of the world reads the reply type of the
    /// same instruction at every restored perform.
    fn check_stop_points(&self, vm: u32) -> Result<(), ImageError> {
        let machine = self.machine(vm);
        // A faulted machine stopped inside the instruction that
        // faulted, so its frames record a position, never a boundary.
        if machine.state == ImageState::Faulted {
            return Ok(());
        }
        for (idx, frame) in machine.frames.iter().enumerate() {
            let at = |what: &str| format!("machine {vm}: frame {idx} {what}");
            let top = idx + 1 == machine.frames.len();
            let pending = top.then_some(machine.pending.as_ref()).flatten();
            if top && pending.is_none() {
                continue;
            }
            // Every index below is proved: `check_references` proved
            // the function, the block, and the program counter of
            // every frame.
            let block = &self.module.funcs[frame.func as usize].blocks[frame.block as usize];
            let Some(before) = frame.ip.checked_sub(1) else {
                return fail(
                    ImageReason::Layout,
                    at("stopped before the first instruction of its block"),
                );
            };
            let instr = block[before as usize];
            match (instr, pending) {
                // A direct call names its callee, so the frame above
                // must run exactly that function.
                (Instr::Call(callee) | Instr::CallG { func: callee, .. }, None) => {
                    if machine.frames[idx + 1].func != callee {
                        return fail(
                            ImageReason::Layout,
                            at("does not sit below the callee of its call site"),
                        );
                    }
                }
                // These calls select their callees from runtime values.
                // Their call sites name no function.
                (
                    Instr::CallVirtual { .. }
                    | Instr::CallVirtualG { .. }
                    | Instr::CallInterface { .. }
                    | Instr::CallValue { .. },
                    None,
                ) => {}
                (Instr::Extended(ExtendedInstr::CallSlot { slot, .. }), None) => {
                    let Some(spec) = self.module.slots.get(slot as usize) else {
                        return fail(ImageReason::Code, at("names no slot contract"));
                    };
                    let upper = machine.frames[idx + 1].func;
                    let compatible = match &spec.contract {
                        SlotContract::Function(contract) => {
                            self.callable_matches(upper, contract, false)
                        }
                        SlotContract::Method(contract) => {
                            self.callable_matches(upper, contract, true)
                        }
                        _ => false,
                    };
                    if !compatible {
                        return fail(
                            ImageReason::Layout,
                            at("does not sit below a compatible slot target"),
                        );
                    }
                }
                (Instr::Perform { op, argc, .. }, Some(request)) => {
                    if request.op != op {
                        return fail(
                            ImageReason::State,
                            at("stopped inside another operation than the pending request"),
                        );
                    }
                    if request.args.len() != argc as usize {
                        return fail(
                            ImageReason::State,
                            at("holds another argument count than its perform"),
                        );
                    }
                }
                // A perform through an operation value names its
                // operation at run time, so the instruction states the
                // argument count alone.
                (Instr::PerformValue { argc, .. }, Some(request)) => {
                    if request.args.len() != argc as usize {
                        return fail(
                            ImageReason::State,
                            at("holds another argument count than its perform"),
                        );
                    }
                }
                _ => {
                    return fail(
                        ImageReason::Layout,
                        at("did not stop inside a call or a perform"),
                    )
                }
            }
        }
        Ok(())
    }
}
