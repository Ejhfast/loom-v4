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

use crate::check::{
    class_self_type, core_native_repr, index_methods, register_core_native_class, AssociatedInfo,
    ClassInfo, ConformanceInfo, ConformancePremise, Ctx, FnSig, InterfaceInfo, InterfaceMethodSig,
    InterfaceUse, MethodSig,
};
use crate::hir::{Flow, HExpr, HExprKind, HStmt, HirFunc, HirImport, HirImportDef};
use lm_bytecode::interface::{
    ExportEntry, IfaceClass, IfaceClassKind, IfaceFn, IfaceInterface, IfaceInterfaceUse, IfaceItem,
    IfaceRow, IfaceType, Interface, QualName,
};
use lm_bytecode::ImportKind;
use lm_source::diag::Diagnostic;
use lm_source::span::Span;
use lm_types::{ClassId, ClassKind, Row, RowElem, Type, TypeId};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

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

    /// True when the environment carries no package dependency root.
    pub fn has_no_package_roots(&self) -> bool {
        self.roots.keys().all(|root| root == "std")
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

/// Add core names used by the selected dependency surfaces.
pub(crate) fn add_used_core_names(
    env: &ImportEnv,
    uses: &[lm_source::ast::UseDecl],
    names: &mut HashSet<String>,
) {
    let mut work = Vec::new();
    for declaration in uses {
        let Some(root) = declaration.path.first() else {
            continue;
        };
        if root == "sys" {
            continue;
        }
        let Some(prefix) = env.roots.get(root) else {
            continue;
        };
        let mut segments: Vec<String> = prefix.split('.').map(str::to_string).collect();
        segments.extend(declaration.path.iter().skip(1).cloned());
        let full = segments.join(".");
        if let Some(interface) = env.module(&full) {
            work.extend(
                interface
                    .exports
                    .iter()
                    .map(|export| (full.clone(), export.name.clone())),
            );
            continue;
        }
        if segments.len() < 2 {
            continue;
        }
        let module = segments[..segments.len() - 1].join(".");
        let name = segments.last().expect("the path has a definition");
        if env
            .module(&module)
            .and_then(|item| item.find(name))
            .is_some()
        {
            work.push((module, name.clone()));
        }
    }

    let mut seen = HashSet::new();
    while let Some((module, name)) = work.pop() {
        if !seen.insert((module.clone(), name.clone())) {
            continue;
        }
        let Some(entry) = env.module(&module).and_then(|item| item.find(&name)) else {
            continue;
        };
        add_item_core_names(&module, &name, &entry.item, names, &mut work);
    }
}

fn add_qual_core_name(
    qual: &QualName,
    names: &mut HashSet<String>,
    work: &mut Vec<(String, String)>,
) {
    if qual.is_core() {
        names.insert(qual.name.clone());
    } else {
        work.push((qual.module.clone(), qual.name.clone()));
    }
}

fn add_interface_core_names(
    interface: &IfaceInterfaceUse,
    names: &mut HashSet<String>,
    work: &mut Vec<(String, String)>,
) {
    add_qual_core_name(&interface.interface, names, work);
    for ty in &interface.types {
        add_type_core_names(ty, names, work);
    }
}

fn add_type_core_names(
    ty: &IfaceType,
    names: &mut HashSet<String>,
    work: &mut Vec<(String, String)>,
) {
    match ty {
        IfaceType::Named { class, args } => {
            add_qual_core_name(class, names, work);
            for argument in args {
                add_type_core_names(argument, names, work);
            }
        }
        IfaceType::Projection {
            base, interface, ..
        } => {
            add_type_core_names(base, names, work);
            add_qual_core_name(interface, names, work);
        }
        IfaceType::List(element)
        | IfaceType::Run(element)
        | IfaceType::RunSnapshot(element)
        | IfaceType::Op(_, element) => add_type_core_names(element, names, work),
        IfaceType::Map(key, value)
        | IfaceType::PendingCall(key, value)
        | IfaceType::Handle(key, value) => {
            add_type_core_names(key, names, work);
            add_type_core_names(value, names, work);
        }
        IfaceType::Tuple(elements) => {
            for element in elements {
                add_type_core_names(element, names, work);
            }
        }
        IfaceType::Fn { params, ret, .. } => {
            for param in params {
                add_type_core_names(param, names, work);
            }
            add_type_core_names(ret, names, work);
        }
        _ => {}
    }
}

fn add_fn_core_names(
    function: &IfaceFn,
    names: &mut HashSet<String>,
    work: &mut Vec<(String, String)>,
) {
    for bounds in &function.type_bounds {
        for bound in bounds {
            add_interface_core_names(bound, names, work);
        }
    }
    for param in &function.params {
        add_type_core_names(param, names, work);
    }
    add_type_core_names(&function.ret, names, work);
}

fn add_item_core_names(
    module: &str,
    name: &str,
    item: &IfaceItem,
    names: &mut HashSet<String>,
    work: &mut Vec<(String, String)>,
) {
    match item {
        IfaceItem::Func(function) => add_fn_core_names(function, names, work),
        IfaceItem::Interface(interface) => {
            for parent in &interface.parents {
                add_interface_core_names(parent, names, work);
            }
            for bounds in &interface.type_bounds {
                for bound in bounds {
                    add_interface_core_names(bound, names, work);
                }
            }
            for associated in &interface.associated {
                for bound in &associated.bounds {
                    add_interface_core_names(bound, names, work);
                }
            }
            for method in &interface.methods {
                for bounds in &method.type_bounds {
                    for bound in bounds {
                        add_interface_core_names(bound, names, work);
                    }
                }
                for premise in &method.premises {
                    add_type_core_names(&premise.subject, names, work);
                    for bound in &premise.bounds {
                        add_interface_core_names(bound, names, work);
                    }
                }
                for param in &method.params {
                    add_type_core_names(param, names, work);
                }
                add_type_core_names(&method.ret, names, work);
            }
        }
        IfaceItem::Class(class) => {
            if let Some(parent) = &class.parent {
                add_qual_core_name(parent, names, work);
            }
            if let Some(family) = &class.family {
                add_qual_core_name(family, names, work);
            }
            for arm in &class.arms {
                work.push((module.to_string(), format!("{name}.{arm}")));
            }
            for field in &class.fields {
                add_type_core_names(&field.ty, names, work);
            }
            for bounds in &class.type_bounds {
                for bound in bounds {
                    add_interface_core_names(bound, names, work);
                }
            }
            for conformance in &class.conformances {
                add_interface_core_names(&conformance.application, names, work);
                for premise in &conformance.premises {
                    for bound in &premise.bounds {
                        add_interface_core_names(bound, names, work);
                    }
                }
                for ty in &conformance.associated {
                    add_type_core_names(ty, names, work);
                }
            }
            for method in &class.methods {
                add_fn_core_names(&method.sig, names, work);
            }
            if let Some(init) = &class.init {
                add_fn_core_names(init, names, work);
            }
        }
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

/// One imported interface waiting for its contract types.
struct PendingInterface {
    id: u32,
    module: String,
    name: String,
    interface: IfaceInterface,
}

/// One hidden default function waiting for phase B.
struct PendingDefault {
    interface: u32,
    method: u32,
    module: String,
    binding: String,
    iface_hash: [u8; 32],
}

/// The import materializer of one module.
pub(crate) struct Materializer<'a> {
    env: &'a ImportEnv,
    /// True when this materializer provides the implicit core module.
    core: bool,
    /// Resolved classes, by (module path, export name).
    classes: HashMap<(String, String), u32>,
    interfaces: HashMap<(String, String), u32>,
    reserving_classes: HashSet<(String, String)>,
    reserving_interfaces: HashSet<(String, String)>,
    expanded_classes: HashSet<(String, String)>,
    expanded_interfaces: HashSet<(String, String)>,
    pending: Vec<PendingClass>,
    pending_interfaces: Vec<PendingInterface>,
    pending_defaults: Vec<PendingDefault>,
    pending_funcs: Vec<PendingFunc>,
    inline_intrinsics: HashMap<(String, String), lm_abi::IntrinsicSlot>,
}

fn error(span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("E1052", message, span)
}

impl<'a> Materializer<'a> {
    pub(crate) fn new(env: &'a ImportEnv) -> Materializer<'a> {
        Materializer {
            env,
            core: false,
            classes: HashMap::new(),
            interfaces: HashMap::new(),
            reserving_classes: HashSet::new(),
            reserving_interfaces: HashSet::new(),
            expanded_classes: HashSet::new(),
            expanded_interfaces: HashSet::new(),
            pending: Vec::new(),
            pending_interfaces: Vec::new(),
            pending_defaults: Vec::new(),
            pending_funcs: Vec::new(),
            inline_intrinsics: HashMap::new(),
        }
    }

    /// Create the materializer for the implicit core dependency.
    pub(crate) fn new_core(env: &'a ImportEnv) -> Materializer<'a> {
        Materializer {
            core: true,
            ..Materializer::new(env)
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

    /// Reserve one imported interface and every contract dependency.
    pub(crate) fn reserve_interface(
        &mut self,
        ctx: &mut Ctx,
        module: &str,
        name: &str,
        span: Span,
    ) -> Result<u32, Diagnostic> {
        let key = (module.to_string(), name.to_string());
        let id = self.reserve_interface_header(ctx, module, name, span)?;
        if !self.expanded_interfaces.insert(key) {
            return Ok(id);
        }
        let entry = self.export(module, name, span)?;
        let IfaceItem::Interface(interface) = &entry.item else {
            unreachable!("the interface header checked the export kind")
        };
        let interface = interface.clone();
        for parent in &interface.parents {
            self.reserve_interface_use(ctx, parent, span)?;
        }
        for bounds in &interface.type_bounds {
            for bound in bounds {
                self.reserve_interface_use(ctx, bound, span)?;
            }
        }
        for associated in &interface.associated {
            for bound in &associated.bounds {
                self.reserve_interface_use(ctx, bound, span)?;
            }
        }
        for method in &interface.methods {
            for bounds in &method.type_bounds {
                for bound in bounds {
                    self.reserve_interface_use(ctx, bound, span)?;
                }
            }
            for premise in &method.premises {
                self.reserve_type(ctx, &premise.subject, span)?;
                for bound in &premise.bounds {
                    self.reserve_interface_use(ctx, bound, span)?;
                }
            }
            for ty in method.params.iter().chain([&method.ret]) {
                self.reserve_type(ctx, ty, span)?;
            }
        }
        Ok(id)
    }

    /// Reserve one interface after its explicit parents.
    fn reserve_interface_header(
        &mut self,
        ctx: &mut Ctx,
        module: &str,
        name: &str,
        span: Span,
    ) -> Result<u32, Diagnostic> {
        let key = (module.to_string(), name.to_string());
        if let Some(id) = self.interfaces.get(&key) {
            return Ok(*id);
        }
        if !self.reserving_interfaces.insert(key.clone()) {
            return Err(error(
                span,
                format!("the interface `{module}.{name}` has an inheritance cycle"),
            ));
        }
        let entry = self.export(module, name, span)?;
        let IfaceItem::Interface(interface) = &entry.item else {
            return Err(error(
                span,
                format!("`{module}.{name}` is not an interface"),
            ));
        };
        if ctx.interfaces.len() > lm_bytecode::MAX_INTERFACE_CALL_INDEX as usize {
            return Err(error(
                span,
                "the module has too many interfaces for compact calls",
            ));
        }
        if interface.methods.len() > lm_bytecode::MAX_INTERFACE_CALL_INDEX as usize + 1 {
            return Err(error(
                span,
                "the imported interface has too many methods for compact calls",
            ));
        }
        let interface = interface.clone();
        for parent in &interface.parents {
            self.reserve_interface_header_qual(ctx, &parent.interface, span)?;
        }
        let id = ctx.interfaces.len() as u32;
        self.interfaces.insert(key.clone(), id);
        if self.core && ctx.core_interfaces.insert(name.to_string(), id).is_some() {
            return Err(error(
                span,
                format!("the core interface `{name}` has more than one definition"),
            ));
        }
        ctx.interfaces.push(InterfaceInfo {
            origin: Some((module.to_string(), name.to_string())),
            name: name.to_string(),
            type_params: (0..interface.type_params)
                .map(|index| format!("${index}"))
                .collect(),
            effect_params: (0..interface.effect_params)
                .map(|index| format!("e{index}"))
                .collect(),
            generic_is_effect: interface.generic_is_effect.clone(),
            parents: Vec::new(),
            type_bounds: vec![Vec::new(); interface.type_params as usize],
            associated: interface
                .associated
                .iter()
                .map(|item| AssociatedInfo {
                    name: item.name.clone(),
                    bounds: Vec::new(),
                })
                .collect(),
            methods: Vec::new(),
            method_index: Vec::new(),
        });
        self.pending_interfaces.push(PendingInterface {
            id,
            module: module.to_string(),
            name: name.to_string(),
            interface: interface.clone(),
        });
        self.reserving_interfaces.remove(&key);
        Ok(id)
    }

    fn reserve_interface_header_qual(
        &mut self,
        ctx: &mut Ctx,
        qual: &QualName,
        span: Span,
    ) -> Result<u32, Diagnostic> {
        if qual.is_core() {
            if self.core {
                return self.reserve_interface_header(
                    ctx,
                    lm_bytecode::CORE_MODULE,
                    &qual.name,
                    span,
                );
            }
            return ctx.core_interfaces.get(&qual.name).copied().ok_or_else(|| {
                error(
                    span,
                    format!("the core interface `{}` does not exist", qual.name),
                )
            });
        }
        self.reserve_interface_header(ctx, &qual.module, &qual.name, span)
    }

    fn reserve_interface_qual(
        &mut self,
        ctx: &mut Ctx,
        qual: &QualName,
        span: Span,
    ) -> Result<u32, Diagnostic> {
        if qual.is_core() {
            if self.core {
                return self.reserve_interface(ctx, lm_bytecode::CORE_MODULE, &qual.name, span);
            }
            return ctx.core_interfaces.get(&qual.name).copied().ok_or_else(|| {
                error(
                    span,
                    format!("the core interface `{}` does not exist", qual.name),
                )
            });
        }
        self.reserve_interface(ctx, &qual.module, &qual.name, span)
    }

    fn reserve_interface_use(
        &mut self,
        ctx: &mut Ctx,
        application: &IfaceInterfaceUse,
        span: Span,
    ) -> Result<(), Diagnostic> {
        self.reserve_interface_qual(ctx, &application.interface, span)?;
        for ty in &application.types {
            self.reserve_type(ctx, ty, span)?;
        }
        Ok(())
    }

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
        let id = self.reserve_class_header(ctx, module, name, span)?;
        if !self.expanded_classes.insert(key) {
            return Ok(id);
        }
        let entry = self.export(module, name, span)?;
        let IfaceItem::Class(class) = &entry.item else {
            unreachable!("the class header checked the export kind")
        };
        let class = class.clone();
        let parent = class
            .parent
            .as_ref()
            .map(|parent| self.reserve_qual(ctx, parent, span))
            .transpose()?;
        if let Some(family) = &class.family {
            self.reserve_qual(ctx, family, span)?;
        }
        // Reserve every class the declaration names.
        if let Some(parent) = parent {
            // The store answers the subtype questions, so an imported
            // child must carry its parent there too. The link happens
            // in phase A, before any signature resolves.
            ctx.store.set_class_parent(ClassId(id), ClassId(parent));
        }
        for arm in &class.arms {
            let full = format!("{name}.{arm}");
            self.reserve_class(ctx, module, &full, span)?;
        }
        for field in &class.fields {
            self.reserve_type(ctx, &field.ty, span)?;
        }
        for bounds in &class.type_bounds {
            for bound in bounds {
                self.reserve_interface_use(ctx, bound, span)?;
            }
        }
        for conformance in &class.conformances {
            self.reserve_interface_use(ctx, &conformance.application, span)?;
            for premise in &conformance.premises {
                for bound in &premise.bounds {
                    self.reserve_interface_use(ctx, bound, span)?;
                }
            }
            for ty in &conformance.associated {
                self.reserve_type(ctx, ty, span)?;
            }
        }
        for method in &class.methods {
            self.reserve_fn(ctx, &method.sig, span)?;
        }
        if let Some(init) = &class.init {
            self.reserve_fn(ctx, init, span)?;
        }
        Ok(id)
    }

    /// Reserve one class after its explicit parent and enum family.
    fn reserve_class_header(
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
        if !self.reserving_classes.insert(key.clone()) {
            return Err(error(
                span,
                format!("the class `{module}.{name}` has an inheritance cycle"),
            ));
        }
        let entry = self.export(module, name, span)?;
        let IfaceItem::Class(class) = &entry.item else {
            return Err(error(
                span,
                format!("`{module}.{name}` is a function, not a type"),
            ));
        };
        let class = class.clone();
        let iface_hash = entry.iface_hash;
        if let Some(parent) = &class.parent {
            self.reserve_class_header_qual(ctx, parent, span)?;
        }
        if let Some(family) = &class.family {
            self.reserve_class_header_qual(ctx, family, span)?;
        }
        let kind = match class.kind {
            IfaceClassKind::Normal => ClassKind::Normal,
            IfaceClassKind::EnumParent => ClassKind::EnumParent,
            IfaceClassKind::EnumCase => ClassKind::EnumCase,
        };
        let id = ctx
            .store
            .register_class(name.to_string(), class.type_params, kind)
            .0;
        if class.is_final {
            ctx.store.set_class_final(ClassId(id));
        }
        self.classes.insert(key.clone(), id);
        if self.core && kind != ClassKind::EnumCase {
            if ctx.core_types.insert(name.to_string(), id).is_some() {
                return Err(error(
                    span,
                    format!("the core type `{name}` has more than one definition"),
                ));
            }
            register_core_native_class(&mut ctx.store, name, ClassId(id));
        }
        self.pending.push(PendingClass {
            id,
            module: module.to_string(),
            name: name.to_string(),
            class,
            iface_hash,
        });
        self.reserving_classes.remove(&key);
        Ok(id)
    }

    fn reserve_class_header_qual(
        &mut self,
        ctx: &mut Ctx,
        qual: &QualName,
        span: Span,
    ) -> Result<u32, Diagnostic> {
        if qual.is_core() {
            if self.core {
                return self.reserve_class_header(ctx, lm_bytecode::CORE_MODULE, &qual.name, span);
            }
            return core_class(ctx, &qual.name, span);
        }
        self.reserve_class_header(ctx, &qual.module, &qual.name, span)
    }

    fn reserve_qual(
        &mut self,
        ctx: &mut Ctx,
        qual: &QualName,
        span: Span,
    ) -> Result<u32, Diagnostic> {
        if qual.is_core() {
            if self.core {
                return self.reserve_class(ctx, lm_bytecode::CORE_MODULE, &qual.name, span);
            }
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
        if self.core {
            let native = match ty {
                IfaceType::Unit => Some("Unit".to_string()),
                IfaceType::Bool => Some("Bool".to_string()),
                IfaceType::Int => Some("Int".to_string()),
                IfaceType::Float => Some("Float".to_string()),
                IfaceType::Str => Some("String".to_string()),
                IfaceType::Bytes => Some("Bytes".to_string()),
                IfaceType::FileHandle => Some("FileHandle".to_string()),
                IfaceType::List(_) => Some("List".to_string()),
                IfaceType::Map(_, _) => Some("Map".to_string()),
                IfaceType::Tuple(elements) if (2..=16).contains(&elements.len()) => {
                    Some(format!("Tuple{}", elements.len()))
                }
                _ => None,
            };
            if let Some(name) = native {
                self.reserve_class(ctx, lm_bytecode::CORE_MODULE, &name, span)?;
            }
        }
        match ty {
            IfaceType::Named { class, args } => {
                self.reserve_qual(ctx, class, span)?;
                for a in args {
                    self.reserve_type(ctx, a, span)?;
                }
            }
            IfaceType::Projection {
                base, interface, ..
            } => {
                self.reserve_type(ctx, base, span)?;
                self.reserve_interface_qual(ctx, interface, span)?;
            }
            IfaceType::List(e)
            | IfaceType::Run(e)
            | IfaceType::RunSnapshot(e)
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
        for bounds in &sig.type_bounds {
            for bound in bounds {
                self.reserve_interface_use(ctx, bound, span)?;
            }
        }
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

    /// Reserve the selected surface of one implicit dependency.
    pub(crate) fn reserve_unit(
        &mut self,
        ctx: &mut Ctx,
        unit: &lm_bytecode::artifact::LinkUnit,
        intrinsics: &[Option<lm_abi::IntrinsicSlot>],
        names: &HashSet<String>,
        methods: &HashSet<String>,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let module = unit.module_path();
        self.index_inline_intrinsics(unit, intrinsics);
        let mut selected = names.clone();
        loop {
            let mut changed = false;
            for export in &unit.interface().exports {
                let IfaceItem::Class(class) = &export.item else {
                    continue;
                };
                let class_selected = selected.contains(&export.name)
                    || selected.contains(&class.arm_short)
                    || class
                        .methods
                        .iter()
                        .any(|method| methods.contains(&method.name));
                if class_selected {
                    changed |= selected.insert(export.name.clone());
                    if let Some(parent) = &class.parent {
                        if parent.is_core() {
                            changed |= selected.insert(parent.name.clone());
                        }
                    }
                    if let Some(family) = &class.family {
                        if family.is_core() {
                            changed |= selected.insert(family.name.clone());
                        }
                    }
                }
                let parent_selected = class
                    .parent
                    .as_ref()
                    .is_some_and(|parent| parent.is_core() && selected.contains(&parent.name));
                let family_selected = class
                    .family
                    .as_ref()
                    .is_some_and(|family| family.is_core() && selected.contains(&family.name));
                if parent_selected || family_selected {
                    changed |= selected.insert(export.name.clone());
                }
            }
            if !changed {
                break;
            }
        }
        for export in &unit.interface().exports {
            match &export.item {
                IfaceItem::Class(class)
                    if selected.contains(&export.name)
                        || selected.contains(&class.arm_short)
                        || class
                            .methods
                            .iter()
                            .any(|method| methods.contains(&method.name)) =>
                {
                    self.reserve_class(ctx, module, &export.name, span)?;
                }
                IfaceItem::Interface(interface)
                    if selected.contains(&export.name)
                        || interface
                            .methods
                            .iter()
                            .any(|method| methods.contains(&method.name)) =>
                {
                    self.reserve_interface(ctx, module, &export.name, span)?;
                }
                _ => {}
            }
        }
        // A reserved signature can add an abstract parent after the
        // selection pass. Keep its complete local class family.
        loop {
            let mut candidates: Vec<String> = unit
                .interface()
                .exports
                .iter()
                .filter_map(|export| {
                    let IfaceItem::Class(class) = &export.item else {
                        return None;
                    };
                    let parent = class.parent.as_ref().filter(|item| item.is_core());
                    let family = class.family.as_ref().filter(|item| item.is_core());
                    let needed = parent.into_iter().chain(family).any(|item| {
                        self.classes
                            .contains_key(&(module.to_string(), item.name.clone()))
                    });
                    needed.then(|| export.name.clone())
                })
                .collect();
            for roles in lm_bytecode::corepin::ROLE_FAMILIES {
                let members: Vec<String> = roles
                    .iter()
                    .filter_map(|role| {
                        let class = *unit.module().core_roles.get(*role)?;
                        unit.module()
                            .classes
                            .get(class as usize)
                            .map(|item| item.name.clone())
                    })
                    .collect();
                let active = members.iter().any(|name| {
                    self.classes
                        .contains_key(&(module.to_string(), name.clone()))
                });
                if active {
                    candidates.extend(members);
                }
            }
            candidates.sort_unstable();
            candidates.dedup();
            let before = self.expanded_classes.len();
            for name in candidates {
                self.reserve_class(ctx, module, &name, span)?;
            }
            if self.expanded_classes.len() == before {
                break;
            }
        }
        for export in &unit.interface().exports {
            if !matches!(export.item, IfaceItem::Func(_)) {
                continue;
            }
            let provider = unit
                .module()
                .exports
                .iter()
                .find(|item| item.name == export.name)
                .ok_or_else(|| {
                    error(
                        span,
                        format!(
                            "the module `{module}` has no function export `{}`",
                            export.name
                        ),
                    )
                })?;
            let function = unit
                .module()
                .funcs
                .get(provider.def as usize)
                .ok_or_else(|| {
                    error(
                        span,
                        format!("the function export `{}` has no definition", export.name),
                    )
                })?;
            if !selected.contains(&export.name) && !selected.contains(&function.name) {
                continue;
            }
            self.reserve_func(ctx, &function.name, module, &export.name, span)?;
        }
        Ok(())
    }

    fn index_inline_intrinsics(
        &mut self,
        unit: &lm_bytecode::artifact::LinkUnit,
        intrinsics: &[Option<lm_abi::IntrinsicSlot>],
    ) {
        let path = unit.module_path();
        for export in &unit.module().exports {
            if export.kind == lm_bytecode::ExportKind::Function {
                if let Some(Some(intrinsic)) = intrinsics.get(export.def as usize) {
                    self.inline_intrinsics
                        .insert((path.to_string(), export.name.clone()), *intrinsic);
                }
                continue;
            }
            if !export.kind.is_class() {
                continue;
            }
            let Some(class) = unit.module().classes.get(export.def as usize) else {
                continue;
            };
            for (selector, function) in &class.methods {
                let Some(name) = unit.module().selectors.get(*selector as usize) else {
                    continue;
                };
                let Some(Some(intrinsic)) = intrinsics.get(*function as usize) else {
                    continue;
                };
                self.inline_intrinsics.insert(
                    (path.to_string(), format!("{}.{}", export.name, name)),
                    *intrinsic,
                );
            }
            let init_key = format!("{}.init", class.key);
            let init = unit
                .module()
                .bindings
                .iter()
                .find(|binding| binding.key == init_key)
                .map(|binding| binding.func);
            if let Some(Some(intrinsic)) = init.and_then(|func| intrinsics.get(func as usize)) {
                self.inline_intrinsics.insert(
                    (path.to_string(), format!("{}.init", export.name)),
                    *intrinsic,
                );
            }
        }
    }

    fn imported_body(&self, module: &str, name: &str, sig: &FnSig) -> Vec<HStmt> {
        let Some(intrinsic) = self
            .inline_intrinsics
            .get(&(module.to_string(), name.to_string()))
            .copied()
        else {
            return Vec::new();
        };
        let args = sig
            .params
            .iter()
            .enumerate()
            .map(|(slot, ty)| HExpr {
                flow: Flow::Normal,
                ty: *ty,
                mutable: sig.param_muts[slot],
                kind: HExprKind::Local(slot as u32),
            })
            .collect();
        vec![HStmt::Expr(
            HExpr {
                flow: Flow::Normal,
                ty: sig.ret,
                mutable: true,
                kind: HExprKind::Intrinsic { intrinsic, args },
            }
            .finish_flow(),
        )]
    }

    /// Fill every reserved interface contract.
    pub(crate) fn finish_interfaces(
        &mut self,
        ctx: &mut Ctx,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let pending = std::mem::take(&mut self.pending_interfaces);
        for item in pending {
            let parents = item
                .interface
                .parents
                .iter()
                .map(|parent| self.resolve_interface_use(ctx, parent, span))
                .collect::<Result<Vec<_>, _>>()?;
            let type_bounds = self.resolve_bounds(ctx, &item.interface.type_bounds, span)?;
            let associated: Vec<AssociatedInfo> = item
                .interface
                .associated
                .iter()
                .map(|associated| {
                    Ok(AssociatedInfo {
                        name: associated.name.clone(),
                        bounds: associated
                            .bounds
                            .iter()
                            .map(|bound| self.resolve_interface_use(ctx, bound, span))
                            .collect::<Result<_, _>>()?,
                    })
                })
                .collect::<Result<_, Diagnostic>>()?;
            let methods: Vec<Rc<InterfaceMethodSig>> = item
                .interface
                .methods
                .iter()
                .map(|method| {
                    let params = method
                        .params
                        .iter()
                        .map(|ty| self.resolve_type(ctx, ty, span))
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(Rc::new(InterfaceMethodSig {
                        name: method.name.clone(),
                        mut_self: method.mut_self,
                        own_type_params: (0..method.type_params)
                            .map(|index| format!("${index}"))
                            .collect(),
                        own_type_bounds: self.resolve_bounds(ctx, &method.type_bounds, span)?,
                        own_effect_params: (0..method.effect_params)
                            .map(|index| format!("e{index}"))
                            .collect(),
                        premises: method
                            .premises
                            .iter()
                            .map(|premise| {
                                Ok(crate::check::TypePremise {
                                    subject: self.resolve_type(ctx, &premise.subject, span)?,
                                    bounds: premise
                                        .bounds
                                        .iter()
                                        .map(|bound| self.resolve_interface_use(ctx, bound, span))
                                        .collect::<Result<Vec<_>, _>>()?,
                                })
                            })
                            .collect::<Result<Vec<_>, Diagnostic>>()?,
                        params,
                        param_muts: method.param_muts.clone(),
                        param_names: method.param_names.clone(),
                        ret: self.resolve_type(ctx, &method.ret, span)?,
                        row: self.resolve_row(ctx, &method.row, span)?,
                        default_func: None,
                        default_binding: method.default.clone(),
                    }))
                })
                .collect::<Result<_, Diagnostic>>()?;
            {
                let info = &mut ctx.interfaces[item.id as usize];
                info.parents = parents;
                info.type_bounds = type_bounds;
                info.associated = associated;
                info.method_index = crate::check::index_interface_methods(&methods);
                info.methods = methods;
            }
            for (method, declaration) in item.interface.methods.iter().enumerate() {
                let Some(binding) = &declaration.default else {
                    continue;
                };
                let export = self.export(&item.module, binding, span)?;
                if !matches!(export.item, IfaceItem::Func(_)) {
                    return Err(error(
                        span,
                        format!("the default binding `{binding}` is not a function"),
                    ));
                }
                self.pending_defaults.push(PendingDefault {
                    interface: item.id,
                    method: method as u32,
                    module: item.module.clone(),
                    binding: binding.clone(),
                    iface_hash: export.iface_hash,
                });
            }
            debug_assert_eq!(
                ctx.interfaces[item.id as usize].origin.as_ref(),
                Some(&(item.module.clone(), item.name.clone()))
            );
        }
        Ok(())
    }

    // ------------------------------------------------------------
    // Phase B: fill the declarations.
    // ------------------------------------------------------------

    /// Fill every reserved declaration, create the imported
    /// functions, and record the import slots. The result is the own
    /// field count of each imported class and its class index.
    pub(crate) fn finish(
        &mut self,
        ctx: &mut Ctx,
        span: Span,
    ) -> Result<Vec<(u32, usize)>, Diagnostic> {
        let pending = std::mem::take(&mut self.pending);
        let mut own_fields = Vec::with_capacity(pending.len());
        for item in &pending {
            let info = self.class_info(ctx, item, span)?;
            own_fields.push((item.id, info.field_names.len() - info.own_start));
            let target = ctx
                .classes
                .get_mut(item.id as usize)
                .ok_or_else(|| error(span, "an imported class has no reserved table entry"))?;
            *target = info;
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
            if let Some(init) = &item.class.init {
                let self_ty = ctx.classes[item.id as usize].self_ty;
                let sig = self.fn_sig(ctx, init, Some((self_ty, true)), span)?;
                let name = format!("{}.init", item.name);
                let body = self.imported_body(&item.module, &name, &sig);
                let func = ctx.push_func(
                    HirFunc {
                        core: self.core,
                        source_span: None,
                        name: name.clone(),
                        type_params: sig.type_params.len() as u32,
                        type_bounds: crate::check::hir_bounds(&sig.type_bounds),
                        effect_params: sig.effect_params.len() as u32,
                        params: sig.params.clone(),
                        param_muts: sig.param_muts.clone(),
                        param_names: sig.param_names.clone(),
                        ret: sig.ret,
                        row: sig.row.clone(),
                        captures: vec![],
                        locals: sig.params.clone(),
                        body,
                        imported: true,
                    },
                    sig,
                );
                ctx.imports.push(HirImport {
                    module: item.module.clone(),
                    name,
                    kind: ImportKind::Method,
                    def: HirImportDef::Func(func),
                    hash: item.iface_hash,
                });
                Rc::get_mut(
                    ctx.classes[item.id as usize]
                        .init
                        .as_mut()
                        .expect("the imported class has an initializer"),
                )
                .expect("an imported initializer is not shared yet")
                .func = func;
            }
            for method in &item.class.methods.clone() {
                let self_ty = ctx.classes[item.id as usize].self_ty;
                let sig = self.fn_sig(ctx, &method.sig, Some((self_ty, method.mut_self)), span)?;
                let name = format!("{}.{}", item.name, method.name);
                let body = self.imported_body(&item.module, &name, &sig);
                let func = ctx.push_func(
                    HirFunc {
                        core: self.core,
                        source_span: None,
                        name: name.clone(),
                        type_params: sig.type_params.len() as u32,
                        type_bounds: crate::check::hir_bounds(&sig.type_bounds),
                        effect_params: sig.effect_params.len() as u32,
                        params: sig.params.clone(),
                        param_muts: sig.param_muts.clone(),
                        param_names: sig.param_names.clone(),
                        ret: sig.ret,
                        row: sig.row.clone(),
                        captures: vec![],
                        locals: sig.params.clone(),
                        body,
                        imported: true,
                    },
                    sig,
                );
                ctx.imports.push(HirImport {
                    module: item.module.clone(),
                    name,
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
                Rc::get_mut(entry)
                    .expect("an imported method signature is not shared yet")
                    .func = func;
            }
        }
        let defaults = std::mem::take(&mut self.pending_defaults);
        for item in defaults {
            let contract = ctx.interfaces[item.interface as usize].clone();
            let requirement = Rc::clone(&contract.methods[item.method as usize]);
            let self_ty = ctx.store.intern(Type::Var(0));
            let mut type_params = Vec::with_capacity(
                1 + contract.type_params.len() + requirement.own_type_params.len(),
            );
            type_params.push("Self".to_string());
            type_params.extend(contract.type_params.iter().cloned());
            type_params.extend(requirement.own_type_params.iter().cloned());
            let application = InterfaceUse {
                interface: item.interface,
                type_args: (0..contract.type_params.len())
                    .map(|at| ctx.store.intern(Type::Var(at as u32 + 1)))
                    .collect(),
                row_args: (0..contract.effect_params.len())
                    .map(|at| vec![RowElem::Var(at as u32)])
                    .collect(),
            };
            let mut type_bounds = vec![vec![application]];
            type_bounds.extend(contract.type_bounds.iter().cloned());
            type_bounds.extend(requirement.own_type_bounds.iter().cloned());
            let mut effect_params = contract.effect_params.clone();
            effect_params.extend(requirement.own_effect_params.iter().cloned());
            let mut params = vec![self_ty];
            params.extend(requirement.params.iter().copied());
            let mut param_muts = vec![requirement.mut_self];
            param_muts.extend(requirement.param_muts.iter().copied());
            let mut param_names = vec!["self".to_string()];
            param_names.extend(requirement.param_names.iter().cloned());
            let sig = FnSig {
                type_params,
                type_bounds,
                effect_params,
                params,
                param_muts,
                param_names,
                ret: requirement.ret,
                row: requirement.row.clone(),
            };
            let body = self.imported_body(&item.module, &item.binding, &sig);
            let func = ctx.push_func(
                HirFunc {
                    core: self.core,
                    source_span: None,
                    name: item.binding.clone(),
                    type_params: sig.type_params.len() as u32,
                    type_bounds: crate::check::hir_bounds(&sig.type_bounds),
                    effect_params: sig.effect_params.len() as u32,
                    params: sig.params.clone(),
                    param_muts: sig.param_muts.clone(),
                    param_names: sig.param_names.clone(),
                    ret: sig.ret,
                    row: sig.row.clone(),
                    captures: vec![],
                    locals: sig.params.clone(),
                    body,
                    imported: true,
                },
                sig,
            );
            ctx.imports.push(HirImport {
                module: item.module,
                name: item.binding,
                kind: ImportKind::Func,
                def: HirImportDef::Func(func),
                hash: item.iface_hash,
            });
            Rc::make_mut(
                &mut ctx.interfaces[item.interface as usize].methods[item.method as usize],
            )
            .default_func = Some(func);
        }
        let funcs = std::mem::take(&mut self.pending_funcs);
        for item in &funcs {
            let sig = self.fn_sig(ctx, &item.sig, None, span)?;
            let body = self.imported_body(&item.module, &item.name, &sig);
            let func = ctx.push_func(
                HirFunc {
                    core: self.core,
                    source_span: None,
                    name: item.bound.clone(),
                    type_params: sig.type_params.len() as u32,
                    type_bounds: crate::check::hir_bounds(&sig.type_bounds),
                    effect_params: sig.effect_params.len() as u32,
                    params: sig.params.clone(),
                    param_muts: sig.param_muts.clone(),
                    param_names: sig.param_names.clone(),
                    ret: sig.ret,
                    row: sig.row.clone(),
                    captures: vec![],
                    locals: sig.params.clone(),
                    body,
                    imported: true,
                },
                sig,
            );
            let functions = if self.core {
                &mut ctx.core_func_index
            } else {
                &mut ctx.func_index
            };
            if functions.insert(item.bound.clone(), func).is_some() {
                return Err(error(
                    span,
                    format!(
                        "the function name `{}` has more than one definition",
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
        let native_repr = self.core.then(|| core_native_repr(&item.name)).flatten();
        let self_ty = class_self_type(
            &mut ctx.store,
            ClassId(item.id),
            class.type_params,
            native_repr,
        );
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
            methods.push(Rc::new(self.method_sig(ctx, item, method, self_ty, span)?));
        }
        let type_bounds = self.resolve_bounds(ctx, &class.type_bounds, span)?;
        let conformances: Vec<Rc<ConformanceInfo>> = class
            .conformances
            .iter()
            .map(|conformance| {
                let application =
                    self.resolve_interface_use(ctx, &conformance.application, span)?;
                let premises = conformance
                    .premises
                    .iter()
                    .map(|premise| {
                        Ok(ConformancePremise {
                            param: premise.param,
                            bounds: premise
                                .bounds
                                .iter()
                                .map(|bound| self.resolve_interface_use(ctx, bound, span))
                                .collect::<Result<_, Diagnostic>>()?,
                        })
                    })
                    .collect::<Result<Vec<_>, Diagnostic>>()?;
                let associated = conformance
                    .associated
                    .iter()
                    .map(|ty| self.resolve_type(ctx, ty, span))
                    .collect::<Result<Vec<_>, _>>()?;
                ctx.store.set_conformance(
                    ClassId(item.id),
                    lm_types::InterfaceId(application.interface),
                    associated.clone(),
                );
                Ok(Rc::new(ConformanceInfo {
                    application,
                    premises,
                    associated,
                    method_overrides: conformance.method_overrides.clone(),
                }))
            })
            .collect::<Result<_, Diagnostic>>()?;
        let init = match &class.init {
            None => None,
            Some(sig) => {
                let fsig = self.fn_sig(ctx, sig, None, span)?;
                Some(Rc::new(MethodSig {
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
                    class_type_bounds: fsig.type_bounds.clone(),
                    own_type_params: Vec::new(),
                    own_type_bounds: Vec::new(),
                    own_effect_params: fsig.effect_params.clone(),
                }))
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
        let method_index = index_methods(&methods);
        Ok(ClassInfo {
            imported: true,
            source_span: None,
            is_final: class.is_final,
            is_frozen: class.is_frozen,
            native_repr,
            name: item.name.clone(),
            parent,
            type_params,
            type_bounds,
            conformances,
            kind,
            self_ty,
            field_names,
            field_tys,
            has_default,
            own_start: class.own_start as usize,
            methods,
            method_index,
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
        let sig = self.fn_sig(ctx, &method.sig, Some((self_ty, method.mut_self)), span)?;
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
            class_type_bounds: sig.type_bounds[..class_params.min(sig.type_bounds.len())].to_vec(),
            own_type_params,
            own_type_bounds: sig.type_bounds[class_params.min(sig.type_bounds.len())..].to_vec(),
            own_effect_params: sig.effect_params.clone(),
        })
    }

    /// One callable signature. `self_param` prepends the receiver.
    fn fn_sig(
        &self,
        ctx: &mut Ctx,
        sig: &IfaceFn,
        self_param: Option<(TypeId, bool)>,
        span: Span,
    ) -> Result<FnSig, Diagnostic> {
        let mut params = Vec::new();
        let mut param_muts = Vec::new();
        let mut param_names = Vec::new();
        if let Some((ty, mutable)) = self_param {
            params.push(ty);
            param_muts.push(mutable);
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
        let type_bounds = self.resolve_bounds(ctx, &sig.type_bounds, span)?;
        Ok(FnSig {
            type_params: (0..sig.type_params).map(|i| format!("${i}")).collect(),
            type_bounds,
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
                    if !ctx.bundle.row_name_valid(name) {
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

    fn resolve_interface_qual(
        &self,
        ctx: &Ctx,
        qual: &QualName,
        span: Span,
    ) -> Result<u32, Diagnostic> {
        if qual.is_core() {
            return ctx.core_interfaces.get(&qual.name).copied().ok_or_else(|| {
                error(
                    span,
                    format!("the core interface `{}` does not exist", qual.name),
                )
            });
        }
        self.interfaces
            .get(&(qual.module.clone(), qual.name.clone()))
            .copied()
            .ok_or_else(|| {
                error(
                    span,
                    format!("the interface `{}` is not visible here", qual.text()),
                )
            })
    }

    fn resolve_interface_use(
        &self,
        ctx: &mut Ctx,
        application: &IfaceInterfaceUse,
        span: Span,
    ) -> Result<InterfaceUse, Diagnostic> {
        Ok(InterfaceUse {
            interface: self.resolve_interface_qual(ctx, &application.interface, span)?,
            type_args: application
                .types
                .iter()
                .map(|ty| self.resolve_type(ctx, ty, span))
                .collect::<Result<_, _>>()?,
            row_args: application
                .rows
                .iter()
                .map(|row| self.resolve_row(ctx, row, span))
                .collect::<Result<_, _>>()?,
        })
    }

    fn resolve_bounds(
        &self,
        ctx: &mut Ctx,
        bounds: &[Vec<IfaceInterfaceUse>],
        span: Span,
    ) -> Result<Vec<Vec<InterfaceUse>>, Diagnostic> {
        bounds
            .iter()
            .map(|items| {
                items
                    .iter()
                    .map(|item| self.resolve_interface_use(ctx, item, span))
                    .collect()
            })
            .collect()
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
            IfaceType::Float => lm_types::FLOAT,
            IfaceType::Str => lm_types::STRING,
            IfaceType::Never => lm_types::NEVER,
            IfaceType::Bytes => lm_types::BYTES,
            IfaceType::FileHandle => lm_types::FILE_HANDLE,
            IfaceType::ResourceHandle => lm_types::RESOURCE_HANDLE,
            IfaceType::HostResource => lm_types::HOST_RESOURCE,
            IfaceType::Digest => lm_types::DIGEST,
            IfaceType::Fault => lm_types::FAULT,
            IfaceType::Request => lm_types::REQUEST,
            IfaceType::PolicyTable => lm_types::POLICY_TABLE,
            IfaceType::Vm => lm_types::VM,
            IfaceType::VmSnapshot => lm_types::VM_SNAPSHOT,
            IfaceType::Var(i) => ctx.store.intern(Type::Var(*i)),
            IfaceType::Projection {
                base,
                interface,
                assoc,
            } => {
                let base = self.resolve_type(ctx, base, span)?;
                let interface = self.resolve_interface_qual(ctx, interface, span)?;
                let associated = ctx.interfaces[interface as usize]
                    .associated
                    .iter()
                    .position(|item| item.name == *assoc)
                    .ok_or_else(|| {
                        error(
                            span,
                            format!(
                                "the interface `{}` has no associated type `{assoc}`",
                                ctx.interfaces[interface as usize].name
                            ),
                        )
                    })? as u32;
                ctx.store.intern(Type::Projection {
                    base,
                    interface: lm_types::InterfaceId(interface),
                    assoc: associated,
                })
            }
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
            IfaceType::Callback {
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
                ctx.store.intern_callback(out, param_muts.clone(), ret, row)
            }
            IfaceType::Run(t) => {
                let t = self.resolve_type(ctx, t, span)?;
                ctx.store.intern(Type::Run(t))
            }
            IfaceType::Wait(t) => {
                let t = self.resolve_type(ctx, t, span)?;
                ctx.store.intern(Type::Wait(t))
            }
            IfaceType::RunSnapshot(t) => {
                let t = self.resolve_type(ctx, t, span)?;
                ctx.store.intern(Type::RunSnapshot(t))
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
