//! Import materialization: turn dependency interfaces into checker
//! declarations and named import slots.
//!
//! A `use` line of a module or a package names one module in the
//! compile environment. The checker materializes every export of that
//! module as a local declaration with a signature and no body, and
//! records one import slot per declaration. The slot pins the
//! interface hash, so the linker proves the provider still presents
//! the interface the module compiled against.
//!
//! Materialization runs in two phases, because the checker fills the
//! class table in index order:
//!
//! - phase A registers the class names and assigns the class indices,
//!   over the transitive closure of the referenced classes;
//! - phase B fills the declarations, creates the imported functions,
//!   and records the slots.
//!
//! Phase A runs before any signature resolves, so a user signature may
//! name an imported type. Phase B runs after the core classes land, so
//! an imported signature may name a core type.

use crate::check::{ClassInfo, Ctx, FnSig, MethodSig};
use crate::hir::{HirFunc, HirImport, HirImportDef};
use lm_bytecode::interface::{
    ExportEntry, IfaceClass, IfaceClassKind, IfaceFn, IfaceItem, IfaceRow, IfaceType, Interface,
    QualName,
};
use lm_bytecode::ImportKind;
use lm_source::diag::Diagnostic;
use lm_source::span::Span;
use lm_types::{ClassId, ClassKind, Row, RowElem, Type, TypeId};
use std::collections::{BTreeMap, HashMap};

/// The interfaces one module may import, and the root names its `use`
/// lines may start with.
///
/// The build tool constructs this environment from the manifest and
/// the dependency interfaces. Ordinary development never touches it.
#[derive(Debug, Clone, Default)]
pub struct ImportEnv {
    /// Every visible module, by full module path.
    pub modules: BTreeMap<String, Interface>,
    /// One `use` root name to the module path prefix it names. A
    /// dependency name maps to that package's prefix; an own module
    /// name maps to this package's prefix plus the module name.
    pub roots: BTreeMap<String, String>,
}

impl ImportEnv {
    pub fn new() -> ImportEnv {
        ImportEnv::default()
    }

    /// True when the environment carries no module at all.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// The interface of one module path.
    pub fn module(&self, path: &str) -> Option<&Interface> {
        self.modules.get(path)
    }

    /// The known module paths under one root, for a diagnostic.
    pub fn paths_under(&self, prefix: &str) -> Vec<&str> {
        self.modules
            .keys()
            .filter(|p| p.as_str() == prefix || p.starts_with(&format!("{prefix}.")))
            .map(|p| p.as_str())
            .collect()
    }
}

/// One class waiting for phase B.
struct PendingClass {
    /// The class index phase A assigned.
    id: u32,
    module: String,
    name: String,
    class: IfaceClass,
    iface_hash: [u8; 32],
}

/// One function waiting for phase B.
struct PendingFunc {
    /// The name the module binds it to.
    bound: String,
    module: String,
    name: String,
    sig: IfaceFn,
    iface_hash: [u8; 32],
}

/// The import materializer of one module.
pub(crate) struct Materializer<'a> {
    env: &'a ImportEnv,
    /// Resolved classes, by (module path, export name).
    classes: HashMap<(String, String), u32>,
    pending: Vec<PendingClass>,
    pending_funcs: Vec<PendingFunc>,
}

fn error(span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("E1052", message, span)
}

impl<'a> Materializer<'a> {
    pub(crate) fn new(env: &'a ImportEnv) -> Materializer<'a> {
        Materializer {
            env,
            classes: HashMap::new(),
            pending: Vec::new(),
            pending_funcs: Vec::new(),
        }
    }

    /// Find one export of one module.
    fn export<'b>(
        &self,
        module: &str,
        name: &str,
        span: Span,
    ) -> Result<&'b ExportEntry, Diagnostic>
    where
        'a: 'b,
    {
        let interface = self
            .env
            .module(module)
            .ok_or_else(|| error(span, format!("the module `{module}` is not visible here")))?;
        interface.find(name).ok_or_else(|| {
            error(
                span,
                format!("the module `{module}` exports no definition named `{name}`"),
            )
        })
    }

    // ------------------------------------------------------------
    // Phase A: reserve class indices.
    // ------------------------------------------------------------

    /// Reserve one imported class and every class its signature
    /// names. The recursion stops at a class already reserved, so a
    /// reference cycle terminates.
    pub(crate) fn reserve_class(
        &mut self,
        ctx: &mut Ctx,
        module: &str,
        name: &str,
        span: Span,
    ) -> Result<u32, Diagnostic> {
        let key = (module.to_string(), name.to_string());
        if let Some(id) = self.classes.get(&key) {
            return Ok(*id);
        }
        let entry = self.export(module, name, span)?;
        let IfaceItem::Class(class) = &entry.item else {
            return Err(error(
                span,
                format!("`{module}.{name}` is a function, not a type"),
            ));
        };
        let kind = match class.kind {
            IfaceClassKind::Normal => ClassKind::Normal,
            IfaceClassKind::EnumParent => ClassKind::EnumParent,
            IfaceClassKind::EnumCase => ClassKind::EnumCase,
        };
        let id = ctx
            .store
            .register_class(name.to_string(), class.type_params, kind)
            .0;
        self.classes.insert(key, id);
        self.pending.push(PendingClass {
            id,
            module: module.to_string(),
            name: name.to_string(),
            class: class.clone(),
            iface_hash: entry.iface_hash,
        });
        // Reserve every class the declaration names.
        let class = class.clone();
        if let Some(parent) = &class.parent {
            let parent = self.reserve_qual(ctx, parent, span)?;
            // The store answers the subtype questions, so an imported
            // child must carry its parent there too. The link happens
            // in phase A, before any signature resolves.
            ctx.store.set_class_parent(ClassId(id), ClassId(parent));
        }
        if let Some(family) = &class.family {
            self.reserve_qual(ctx, family, span)?;
        }
        for arm in &class.arms {
            let full = format!("{name}.{arm}");
            self.reserve_class(ctx, module, &full, span)?;
        }
        for field in &class.fields {
            self.reserve_type(ctx, &field.ty, span)?;
        }
        for method in &class.methods {
            self.reserve_fn(ctx, &method.sig, span)?;
        }
        if let Some(init) = &class.init {
            self.reserve_fn(ctx, init, span)?;
        }
        Ok(id)
    }

    fn reserve_qual(
        &mut self,
        ctx: &mut Ctx,
        qual: &QualName,
        span: Span,
    ) -> Result<u32, Diagnostic> {
        if qual.is_core() {
            return core_class(ctx, &qual.name, span);
        }
        self.reserve_class(ctx, &qual.module, &qual.name, span)
    }

    fn reserve_type(
        &mut self,
        ctx: &mut Ctx,
        ty: &IfaceType,
        span: Span,
    ) -> Result<(), Diagnostic> {
        match ty {
            IfaceType::Named { class, args } => {
                self.reserve_qual(ctx, class, span)?;
                for a in args {
                    self.reserve_type(ctx, a, span)?;
                }
            }
            IfaceType::List(e)
            | IfaceType::Vm(e)
            | IfaceType::Snapshot(e)
            | IfaceType::Op(_, e) => self.reserve_type(ctx, e, span)?,
            IfaceType::Map(k, v) | IfaceType::PendingCall(k, v) | IfaceType::Handle(k, v) => {
                self.reserve_type(ctx, k, span)?;
                self.reserve_type(ctx, v, span)?;
            }
            IfaceType::Tuple(elems) => {
                for e in elems {
                    self.reserve_type(ctx, e, span)?;
                }
            }
            IfaceType::Fn { params, ret, .. } => {
                for p in params {
                    self.reserve_type(ctx, p, span)?;
                }
                self.reserve_type(ctx, ret, span)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn reserve_fn(&mut self, ctx: &mut Ctx, sig: &IfaceFn, span: Span) -> Result<(), Diagnostic> {
        for p in &sig.params {
            self.reserve_type(ctx, p, span)?;
        }
        self.reserve_type(ctx, &sig.ret, span)
    }

    /// Reserve one imported function under the given bound name.
    pub(crate) fn reserve_func(
        &mut self,
        ctx: &mut Ctx,
        bound: &str,
        module: &str,
        name: &str,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let entry = self.export(module, name, span)?;
        let IfaceItem::Func(sig) = &entry.item else {
            return Err(error(
                span,
                format!("`{module}.{name}` is a type, not a function"),
            ));
        };
        let sig = sig.clone();
        let iface_hash = entry.iface_hash;
        self.reserve_fn(ctx, &sig, span)?;
        self.pending_funcs.push(PendingFunc {
            bound: bound.to_string(),
            module: module.to_string(),
            name: name.to_string(),
            sig,
            iface_hash,
        });
        Ok(())
    }

    // ------------------------------------------------------------
    // Phase B: fill the declarations.
    // ------------------------------------------------------------

    /// Fill every reserved declaration, create the imported
    /// functions, and record the import slots. The result is the own
    /// field count of each imported class, in class-index order, so
    /// the caller can keep the default table aligned.
    pub(crate) fn finish(&mut self, ctx: &mut Ctx, span: Span) -> Result<Vec<usize>, Diagnostic> {
        let pending = std::mem::take(&mut self.pending);
        let mut own_fields = Vec::with_capacity(pending.len());
        for item in &pending {
            debug_assert_eq!(item.id as usize, ctx.classes.len());
            let info = self.class_info(ctx, item, span)?;
            own_fields.push(info.field_names.len() - info.own_start);
            ctx.classes.push(info);
            ctx.imports.push(HirImport {
                module: item.module.clone(),
                name: item.name.clone(),
                kind: ImportKind::Class,
                def: HirImportDef::Class(item.id),
                hash: item.iface_hash,
            });
            ctx.imports.push(HirImport {
                module: item.module.clone(),
                name: item.name.clone(),
                kind: ImportKind::Ctor,
                def: HirImportDef::Ctor(item.id),
                hash: item.iface_hash,
            });
        }
        // The method functions follow the class table, so every class
        // index exists before a method signature resolves.
        for item in &pending {
            for method in &item.class.methods.clone() {
                let self_ty = ctx.classes[item.id as usize].self_ty;
                let sig = self.fn_sig(ctx, &method.sig, Some(self_ty), span)?;
                let func = ctx.push_func(
                    HirFunc {
                        name: format!("{}.{}", item.name, method.name),
                        type_params: sig.type_params.len() as u32,
                        effect_params: sig.effect_params.len() as u32,
                        params: sig.params.clone(),
                        param_muts: sig.param_muts.clone(),
                        ret: sig.ret,
                        row: sig.row.clone(),
                        captures: vec![],
                        locals: sig.params.clone(),
                        body: vec![],
                        imported: true,
                    },
                    sig,
                );
                ctx.imports.push(HirImport {
                    module: item.module.clone(),
                    name: format!("{}.{}", item.name, method.name),
                    kind: ImportKind::Method,
                    def: HirImportDef::Func(func),
                    hash: item.iface_hash,
                });
                let info = &mut ctx.classes[item.id as usize];
                let entry = info
                    .methods
                    .iter_mut()
                    .find(|m| m.name == method.name)
                    .expect("every declared method has a signature");
                entry.func = func;
            }
        }
        let funcs = std::mem::take(&mut self.pending_funcs);
        for item in &funcs {
            let sig = self.fn_sig(ctx, &item.sig, None, span)?;
            let func = ctx.push_func(
                HirFunc {
                    name: item.name.clone(),
                    type_params: sig.type_params.len() as u32,
                    effect_params: sig.effect_params.len() as u32,
                    params: sig.params.clone(),
                    param_muts: sig.param_muts.clone(),
                    ret: sig.ret,
                    row: sig.row.clone(),
                    captures: vec![],
                    locals: sig.params.clone(),
                    body: vec![],
                    imported: true,
                },
                sig,
            );
            if ctx.func_index.insert(item.bound.clone(), func).is_some() {
                return Err(error(
                    span,
                    format!(
                        "the name `{}` already has a definition in this module; \
                         rename it or bind the module instead",
                        item.bound
                    ),
                ));
            }
            ctx.imports.push(HirImport {
                module: item.module.clone(),
                name: item.name.clone(),
                kind: ImportKind::Func,
                def: HirImportDef::Func(func),
                hash: item.iface_hash,
            });
        }
        Ok(own_fields)
    }

    /// The checker declaration of one imported class.
    fn class_info(
        &self,
        ctx: &mut Ctx,
        item: &PendingClass,
        span: Span,
    ) -> Result<ClassInfo, Diagnostic> {
        let class = &item.class;
        let kind = match class.kind {
            IfaceClassKind::Normal => ClassKind::Normal,
            IfaceClassKind::EnumParent => ClassKind::EnumParent,
            IfaceClassKind::EnumCase => ClassKind::EnumCase,
        };
        let type_params: Vec<String> = (0..class.type_params).map(|i| format!("${i}")).collect();
        let self_ty = if class.type_params == 0 {
            ctx.store.intern(Type::Class(ClassId(item.id)))
        } else {
            let vars: Vec<TypeId> = (0..class.type_params)
                .map(|i| ctx.store.intern(Type::Var(i)))
                .collect();
            ctx.store.intern(Type::Inst(ClassId(item.id), vars))
        };
        let parent = match &class.parent {
            None => None,
            Some(p) => Some(self.resolve_qual(ctx, p, span)?),
        };
        let family = match &class.family {
            None => None,
            Some(f) => Some(self.resolve_qual(ctx, f, span)?),
        };
        let mut field_names = Vec::new();
        let mut field_tys = Vec::new();
        let mut has_default = Vec::new();
        for field in &class.fields {
            field_names.push(field.name.clone());
            field_tys.push(self.resolve_type(ctx, &field.ty, span)?);
            has_default.push(field.has_default);
        }
        let mut methods = Vec::new();
        for method in &class.methods {
            methods.push(self.method_sig(ctx, item, method, self_ty, span)?);
        }
        let init = match &class.init {
            None => None,
            Some(sig) => {
                let fsig = self.fn_sig(ctx, sig, None, span)?;
                Some(MethodSig {
                    name: "init".to_string(),
                    // Phase B fills no function for `init`: the
                    // construction function of the class carries it.
                    func: u32::MAX,
                    mut_self: true,
                    params: fsig.params.clone(),
                    param_muts: fsig.param_muts.clone(),
                    param_names: fsig.param_names.clone(),
                    ret: lm_types::UNIT,
                    row: fsig.row.clone(),
                    own_type_params: Vec::new(),
                    own_effect_params: fsig.effect_params.clone(),
                })
            }
        };
        let mut arms = Vec::new();
        for arm in &class.arms {
            let full = format!("{}.{}", item.name, arm);
            arms.push(
                *self
                    .classes
                    .get(&(item.module.clone(), full.clone()))
                    .ok_or_else(|| error(span, format!("the arm `{full}` is not exported")))?,
            );
        }
        Ok(ClassInfo {
            imported: true,
            name: item.name.clone(),
            parent,
            type_params,
            kind,
            self_ty,
            field_names,
            field_tys,
            has_default,
            own_start: class.own_start as usize,
            methods,
            init,
            family,
            arms,
            arm_short: class.arm_short.clone(),
        })
    }

    fn method_sig(
        &self,
        ctx: &mut Ctx,
        item: &PendingClass,
        method: &lm_bytecode::interface::IfaceMethod,
        self_ty: TypeId,
        span: Span,
    ) -> Result<MethodSig, Diagnostic> {
        let class_params = item.class.type_params as usize;
        let sig = self.fn_sig(ctx, &method.sig, Some(self_ty), span)?;
        // The interface counts the class type parameters first, so the
        // method's own parameters follow them.
        let own_type_params: Vec<String> =
            sig.type_params[class_params.min(sig.type_params.len())..].to_vec();
        Ok(MethodSig {
            name: method.name.clone(),
            func: u32::MAX,
            mut_self: method.mut_self,
            params: sig.params[1..].to_vec(),
            param_muts: sig.param_muts[1..].to_vec(),
            param_names: sig.param_names[1..].to_vec(),
            ret: sig.ret,
            row: sig.row.clone(),
            own_type_params,
            own_effect_params: sig.effect_params.clone(),
        })
    }

    /// One callable signature. `self_ty` prepends the receiver.
    fn fn_sig(
        &self,
        ctx: &mut Ctx,
        sig: &IfaceFn,
        self_ty: Option<TypeId>,
        span: Span,
    ) -> Result<FnSig, Diagnostic> {
        let mut params = Vec::new();
        let mut param_muts = Vec::new();
        let mut param_names = Vec::new();
        if let Some(ty) = self_ty {
            params.push(ty);
            param_muts.push(false);
            param_names.push("self".to_string());
        }
        for ((p, m), n) in sig
            .params
            .iter()
            .zip(sig.param_muts.iter())
            .zip(sig.param_names.iter())
        {
            params.push(self.resolve_type(ctx, p, span)?);
            param_muts.push(*m);
            param_names.push(n.clone());
        }
        let ret = self.resolve_type(ctx, &sig.ret, span)?;
        let row = self.resolve_row(ctx, &sig.row, span)?;
        Ok(FnSig {
            type_params: (0..sig.type_params).map(|i| format!("${i}")).collect(),
            effect_params: (0..sig.effect_params).map(|i| format!("e{i}")).collect(),
            params,
            param_muts,
            param_names,
            ret,
            row,
        })
    }

    fn resolve_row(&self, ctx: &mut Ctx, row: &[IfaceRow], span: Span) -> Result<Row, Diagnostic> {
        let mut out = Vec::new();
        for elem in row {
            match elem {
                IfaceRow::Op(name) => {
                    if !lm_abi::row_name_valid(name) {
                        return Err(error(
                            span,
                            format!("the interface names `{name}`, which is not an operation"),
                        ));
                    }
                    out.push(RowElem::Op(ctx.store.intern_row_name(name)));
                }
                IfaceRow::Var(v) => out.push(RowElem::Var(*v)),
            }
        }
        Ok(ctx.store.canonical_row(out))
    }

    fn resolve_qual(&self, ctx: &mut Ctx, qual: &QualName, span: Span) -> Result<u32, Diagnostic> {
        if qual.is_core() {
            return core_class(ctx, &qual.name, span);
        }
        self.classes
            .get(&(qual.module.clone(), qual.name.clone()))
            .copied()
            .ok_or_else(|| {
                error(
                    span,
                    format!("the type `{}` is not visible here", qual.text()),
                )
            })
    }

    fn resolve_type(
        &self,
        ctx: &mut Ctx,
        ty: &IfaceType,
        span: Span,
    ) -> Result<TypeId, Diagnostic> {
        let id = match ty {
            IfaceType::Unit => lm_types::UNIT,
            IfaceType::Bool => lm_types::BOOL,
            IfaceType::Int => lm_types::INT,
            IfaceType::Str => lm_types::STRING,
            IfaceType::StringBuilder => lm_types::STRING_BUILDER,
            IfaceType::ByteBuffer => lm_types::BYTE_BUFFER,
            IfaceType::Digest => lm_types::DIGEST,
            IfaceType::Fault => lm_types::FAULT,
            IfaceType::Request => lm_types::REQUEST,
            IfaceType::PolicyTable => lm_types::POLICY_TABLE,
            IfaceType::EmptyVm => lm_types::EMPTY_VM,
            IfaceType::SnapshotImage => lm_types::SNAPSHOT_IMAGE,
            IfaceType::Var(i) => ctx.store.intern(Type::Var(*i)),
            IfaceType::Named { class, args } => {
                let idx = self.resolve_qual(ctx, class, span)?;
                if args.is_empty() {
                    ctx.store.intern(Type::Class(ClassId(idx)))
                } else {
                    let mut resolved = Vec::with_capacity(args.len());
                    for a in args {
                        resolved.push(self.resolve_type(ctx, a, span)?);
                    }
                    ctx.store.intern(Type::Inst(ClassId(idx), resolved))
                }
            }
            IfaceType::List(e) => {
                let e = self.resolve_type(ctx, e, span)?;
                ctx.store.intern(Type::List(e))
            }
            IfaceType::Map(k, v) => {
                let k = self.resolve_type(ctx, k, span)?;
                let v = self.resolve_type(ctx, v, span)?;
                ctx.store.intern(Type::Map(k, v))
            }
            IfaceType::Tuple(elems) => {
                let mut out = Vec::with_capacity(elems.len());
                for e in elems {
                    out.push(self.resolve_type(ctx, e, span)?);
                }
                ctx.store.intern(Type::Tuple(out))
            }
            IfaceType::Fn {
                params,
                param_muts,
                ret,
                row,
            } => {
                let mut out = Vec::with_capacity(params.len());
                for p in params {
                    out.push(self.resolve_type(ctx, p, span)?);
                }
                let ret = self.resolve_type(ctx, ret, span)?;
                let row = self.resolve_row(ctx, row, span)?;
                ctx.store.intern_fn(out, param_muts.clone(), ret, row)
            }
            IfaceType::Vm(t) => {
                let t = self.resolve_type(ctx, t, span)?;
                ctx.store.intern(Type::Vm(t))
            }
            IfaceType::Snapshot(t) => {
                let t = self.resolve_type(ctx, t, span)?;
                ctx.store.intern(Type::Snapshot(t))
            }
            IfaceType::PendingCall(a, r) => {
                let a = self.resolve_type(ctx, a, span)?;
                let r = self.resolve_type(ctx, r, span)?;
                ctx.store.intern(Type::PendingCall(a, r))
            }
            IfaceType::Handle(m, r) => {
                let m = self.resolve_type(ctx, m, span)?;
                let r = self.resolve_type(ctx, r, span)?;
                ctx.store.intern(Type::Handle(m, r))
            }
            IfaceType::Op(op, f) => {
                let f = self.resolve_type(ctx, f, span)?;
                ctx.store.intern(Type::Op(*op, f))
            }
        };
        Ok(id)
    }
}

/// One core class by its canonical name. An arm carries the family
/// name and the short name, for example `Option.Some`.
fn core_class(ctx: &mut Ctx, name: &str, span: Span) -> Result<u32, Diagnostic> {
    if let Some(idx) = ctx.core_types.get(name) {
        return Ok(*idx);
    }
    if let Some((family, short)) = name.split_once('.') {
        if let Some(parent) = ctx.core_types.get(family).copied() {
            if let Some(arm) = ctx.find_arm(parent, short) {
                return Ok(arm);
            }
        }
    }
    Err(error(
        span,
        format!(
            "the interface names the core definition `{name}`, which this \
                 core image does not carry"
        ),
    ))
}
