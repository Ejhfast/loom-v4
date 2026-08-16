//! Module-level checking: predeclaration, signature resolution, the
//! pinned core image, and the prelude.
//!
//! The expression checker lives in `checkfn`. This file resolves the
//! module shape: classes, enums, generic signatures, effect rows, and
//! the order of definition indices. The core sources are ordinary
//! Loom code compiled by the same pipeline into every module, after
//! the user definitions, so user definition indices stay stable.

use crate::checkfn::{FnChecker, RetKind};
use crate::hir::*;
use lm_source::ast;
use lm_source::diag::Diagnostic;
use lm_source::span::Span;
use lm_types::{ClassId, ClassKind, Row, RowElem, Type, TypeId, TypeStore, NEVER, UNIT};
use std::collections::HashMap;
use std::sync::OnceLock;

/// The concatenated pinned core sources, in canonical file order.
pub const CORE_SOURCE: &str = concat!(
    include_str!("../../../core/option.lm"),
    "\n",
    include_str!("../../../core/result.lm"),
    "\n",
    include_str!("../../../core/ordering.lm"),
    "\n",
    include_str!("../../../core/pair.lm"),
    "\n",
    include_str!("../../../core/range.lm"),
    "\n",
    include_str!("../../../core/errors.lm"),
    "\n",
    include_str!("../../../core/vm.lm"),
    "\n",
    include_str!("../../../core/proc.lm"),
    "\n",
);

/// The type names the prelude places into unqualified scope.
pub const PRELUDE_TYPES: [&str; 10] = [
    "Option",
    "Result",
    "Ordering",
    "Pair",
    "Range",
    "Proc",
    "Recv",
    "SendResult",
    "ProcResult",
    "ProcError",
];

/// The constructor names the prelude places into unqualified scope.
pub const PRELUDE_CTORS: [&str; 4] = ["Some", "None", "Ok", "Err"];

/// Checker options. `prelude` controls only unqualified name
/// resolution; the core image itself never depends on it.
#[derive(Debug, Clone)]
pub struct CheckOptions {
    pub prelude: bool,
    /// The module path of the source under compilation, for example
    /// `mathlib.matrix`. It names this module's classes inside the
    /// emitted interface, and it forms the qualified key of every
    /// class this module declares. A structural hash that names one
    /// of those classes therefore follows the module path.
    pub module_path: String,
    /// The interfaces this module may import. The build tool
    /// constructs it from the manifest and the dependency interfaces.
    pub imports: crate::import::ImportEnv,
}

impl Default for CheckOptions {
    fn default() -> CheckOptions {
        CheckOptions {
            prelude: true,
            module_path: String::new(),
            imports: crate::import::ImportEnv::new(),
        }
    }
}

fn core_ast() -> &'static ast::Module {
    static CORE: OnceLock<ast::Module> = OnceLock::new();
    CORE.get_or_init(|| {
        let module = lm_source::parse::parse(CORE_SOURCE).expect("the core sources parse");
        assert!(module.entry.is_empty(), "the core has no entry expression");
        assert!(module.funcs.is_empty(), "the core has no free functions");
        module
    })
}

/// One callable signature. Methods include `self` as parameter zero.
/// For a method, the type parameters hold the class parameters first
/// and the method's own parameters after them.
#[derive(Clone)]
pub(crate) struct FnSig {
    pub(crate) type_params: Vec<String>,
    pub(crate) effect_params: Vec<String>,
    pub(crate) params: Vec<TypeId>,
    pub(crate) param_muts: Vec<bool>,
    /// Declared parameter names for labeled arguments. A method holds
    /// `self` at position zero.
    pub(crate) param_names: Vec<String>,
    pub(crate) ret: TypeId,
    pub(crate) row: Row,
}

/// One declared method, without `self` in the parameter lists.
#[derive(Clone)]
pub(crate) struct MethodSig {
    pub(crate) name: String,
    pub(crate) func: u32,
    pub(crate) mut_self: bool,
    pub(crate) params: Vec<TypeId>,
    pub(crate) param_muts: Vec<bool>,
    /// Declared parameter names, without `self`.
    pub(crate) param_names: Vec<String>,
    pub(crate) ret: TypeId,
    pub(crate) row: Row,
    pub(crate) own_type_params: Vec<String>,
    pub(crate) own_effect_params: Vec<String>,
}

/// Checker-side class information with the full field layout.
pub(crate) struct ClassInfo {
    /// True for an imported declaration: a shape with no body.
    pub(crate) imported: bool,
    pub(crate) name: String,
    pub(crate) parent: Option<u32>,
    pub(crate) type_params: Vec<String>,
    pub(crate) kind: ClassKind,
    /// The instance type seen by method bodies: `Class(c)` or
    /// `Inst(c, [Var 0..])`.
    pub(crate) self_ty: TypeId,
    pub(crate) field_names: Vec<String>,
    pub(crate) field_tys: Vec<TypeId>,
    pub(crate) has_default: Vec<bool>,
    /// The layout index where own fields start.
    pub(crate) own_start: usize,
    pub(crate) methods: Vec<MethodSig>,
    pub(crate) init: Option<MethodSig>,
    /// For an enum case: the family parent class index.
    pub(crate) family: Option<u32>,
    /// For an enum parent: the case class indices in arm order.
    pub(crate) arms: Vec<u32>,
    /// For an enum case: the short arm name, for example `Some`.
    pub(crate) arm_short: String,
}

impl ClassInfo {
    /// An unresolved table entry. Registration fixes every class index
    /// before any signature resolves, so the table is presized and
    /// every entry is replaced by its resolved form.
    pub(crate) fn placeholder(_idx: u32) -> ClassInfo {
        ClassInfo {
            imported: false,
            name: String::new(),
            parent: None,
            type_params: Vec::new(),
            kind: ClassKind::Normal,
            self_ty: UNIT,
            field_names: Vec::new(),
            field_tys: Vec::new(),
            has_default: Vec::new(),
            own_start: 0,
            methods: Vec::new(),
            init: None,
            family: None,
            arms: Vec::new(),
            arm_short: String::new(),
        }
    }
}

/// One resolved `use` binding.
#[derive(Clone)]
pub(crate) enum UseBinding {
    /// A `sys` group object, by manifest group name (`Io`).
    SysGroup(&'static str),
    /// A callable `sys` member: the manifest group name plus the
    /// surface member name (`print`, `read_line`, or `Vm`).
    SysMember { group: &'static str, member: String },
    /// A whole module bound to a short name. A member of it resolves
    /// through the qualified key `alias.member`.
    Module(String),
}

/// Map a surface `sys` member name to its manifest group name.
pub(crate) fn sys_group_name(name: &str) -> Option<&'static str> {
    match name {
        "io" => Some("Io"),
        "fs" => Some("Fs"),
        "clock" => Some("Clock"),
        "rand" => Some("Rand"),
        "net" => Some("Net"),
        "proc" => Some("Proc"),
        "vm" => Some("Vm"),
        "compiler" => Some("Compiler"),
        "reflect" => Some("Reflect"),
        _ => None,
    }
}

/// The manifest member name of one surface member:
/// `read_line` becomes `ReadLine`. The mapping is mechanical.
pub(crate) fn camel_member(surface: &str) -> String {
    surface
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// The surface name of one manifest member: `ReadLine` becomes
/// `read_line`.
pub(crate) fn snake_member(member: &str) -> String {
    let mut out = String::new();
    for (i, c) in member.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Resolve one `use` line that names a module or a definition of the
/// compile environment. Phase A of import materialization runs here:
/// the class indices are reserved before any signature resolves.
fn resolve_module_use(
    ctx: &mut Ctx,
    mat: &mut crate::import::Materializer<'_>,
    env: &crate::import::ImportEnv,
    decl: &ast::UseDecl,
) -> Result<(String, UseBinding), Diagnostic> {
    let text = decl.path.join(".");
    let root = &decl.path[0];
    if root == "std" {
        return Err(Diagnostic::new(
            "E1052",
            "the standard library does not ship with this toolchain yet; \
             use a path dependency or the `sys` operations",
            decl.span,
        ));
    }
    let Some(prefix) = env.roots.get(root) else {
        let mut known: Vec<&str> = env.roots.keys().map(|k| k.as_str()).collect();
        known.push("sys");
        return Err(Diagnostic::new(
            "E1052",
            if env.is_empty() {
                format!(
                    "`use {text}` names a module; a module import needs a package, \
                     so compile the file inside one"
                )
            } else {
                format!(
                    "`{root}` is not a root name here; the roots are {}",
                    known.join(", ")
                )
            },
            decl.span,
        ));
    };
    // The candidate path replaces the root with the prefix it names.
    let mut segments: Vec<String> = prefix.split('.').map(|s| s.to_string()).collect();
    segments.extend(decl.path[1..].iter().cloned());
    let full = segments.join(".");
    let bound = decl.path.last().expect("a use path has segments").clone();
    if env.module(&full).is_some() {
        // A module alias: every export binds under `alias.name`.
        let interface = env.module(&full).expect("checked").clone();
        for export in &interface.exports {
            let bound_name = format!("{bound}.{}", export.name);
            if export.kind.is_class() {
                let id = mat.reserve_class(ctx, &full, &export.name, decl.span)?;
                bind_type(ctx, &bound_name, id, decl.name_span)?;
            } else {
                mat.reserve_func(ctx, &bound_name, &full, &export.name, decl.span)?;
            }
        }
        return Ok((bound, UseBinding::Module(full)));
    }
    // A definition of a module: the last segment names the export.
    if segments.len() >= 2 {
        let module = segments[..segments.len() - 1].join(".");
        if let Some(interface) = env.module(&module) {
            let Some(export) = interface.find(&bound) else {
                let mut names: Vec<&str> =
                    interface.exports.iter().map(|e| e.name.as_str()).collect();
                names.sort_unstable();
                return Err(Diagnostic::new(
                    "E1052",
                    format!(
                        "the module `{module}` exports no `{bound}`; it exports {}",
                        names.join(", ")
                    ),
                    decl.name_span,
                ));
            };
            if export.kind.is_class() {
                let id = mat.reserve_class(ctx, &module, &bound, decl.span)?;
                bind_type(ctx, &bound, id, decl.name_span)?;
            } else {
                mat.reserve_func(ctx, &bound, &module, &bound, decl.span)?;
            }
            return Ok((bound, UseBinding::Module(module)));
        }
    }
    let known = env.paths_under(prefix);
    Err(Diagnostic::new(
        "E1052",
        if known.is_empty() {
            format!("`{text}` names no module of `{root}`")
        } else {
            format!(
                "`{text}` names no module; `{root}` provides {}",
                known.join(", ")
            )
        },
        decl.span,
    ))
}

/// Bind one imported type name in the module scope. A name the module
/// already defines is an error, never a silent shadow.
fn bind_type(ctx: &mut Ctx, name: &str, class: u32, span: Span) -> Result<(), Diagnostic> {
    if ctx.user_types.contains_key(name) {
        return Err(Diagnostic::new(
            "E1052",
            format!(
                "the name `{name}` already has a definition in this module; \
                 rename it or bind the module instead"
            ),
            span,
        ));
    }
    ctx.user_types.insert(name.to_string(), class);
    Ok(())
}

/// Resolve the `use` lines of one module into named bindings.
fn resolve_uses(
    ctx: &mut Ctx,
    mat: &mut crate::import::Materializer<'_>,
    env: &crate::import::ImportEnv,
    uses: &[ast::UseDecl],
) -> Result<HashMap<String, UseBinding>, Diagnostic> {
    let mut out: HashMap<String, UseBinding> = HashMap::new();
    for decl in uses {
        if decl.path[0] != "sys" {
            let (bound, binding) = resolve_module_use(ctx, mat, env, decl)?;
            if out.insert(bound.clone(), binding).is_some() {
                return Err(Diagnostic::new(
                    "E1052",
                    format!("the name `{bound}` already has a `use` binding"),
                    decl.name_span,
                ));
            }
            continue;
        }
        let binding = match decl.path.len() {
            1 => {
                return Err(Diagnostic::new(
                    "E1052",
                    "`use sys` binds nothing; name a group or an operation, \
                     for example `use sys.io` or `use sys.io.print`",
                    decl.span,
                ));
            }
            2 => {
                let Some(group) = sys_group_name(&decl.path[1]) else {
                    return Err(Diagnostic::new(
                        "E1052",
                        format!("`sys` has no group named `{}`", decl.path[1]),
                        decl.name_span,
                    ));
                };
                UseBinding::SysGroup(group)
            }
            3 => {
                let Some(group) = sys_group_name(&decl.path[1]) else {
                    return Err(Diagnostic::new(
                        "E1052",
                        format!("`sys` has no group named `{}`", decl.path[1]),
                        decl.span,
                    ));
                };
                let member = decl.path[2].clone();
                let starts_upper = member
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_uppercase())
                    .unwrap_or(false);
                let is_ctor = group == "Vm" && member == "Vm";
                if starts_upper && !is_ctor {
                    if lm_abi::fixed_member(group, &member).is_some() {
                        return Err(Diagnostic::new(
                            "E1052",
                            format!(
                                "callable `sys` members use snake_case; write \
                                 `use sys.{}.{}`",
                                decl.path[1],
                                snake_member(&member)
                            ),
                            decl.name_span,
                        ));
                    }
                    return Err(Diagnostic::new(
                        "E1052",
                        format!("the group `{group}` has no operation named `{member}`"),
                        decl.name_span,
                    ));
                }
                if !is_ctor && lm_abi::fixed_member(group, &camel_member(&member)).is_none() {
                    return Err(Diagnostic::new(
                        "E1052",
                        format!("the group `{group}` has no operation named `{member}`"),
                        decl.name_span,
                    ));
                }
                UseBinding::SysMember { group, member }
            }
            _ => {
                return Err(Diagnostic::new(
                    "E1052",
                    "a fixed `sys` binding has at most three path segments",
                    decl.span,
                ));
            }
        };
        let bound = decl.path.last().expect("a use path has segments").clone();
        if out.insert(bound.clone(), binding).is_some() {
            return Err(Diagnostic::new(
                "E1052",
                format!("the name `{bound}` already has a `use` binding"),
                decl.name_span,
            ));
        }
    }
    Ok(out)
}

/// The lexical resolution scope of the code being checked.
#[derive(Clone, Default)]
pub(crate) struct TyEnv {
    pub(crate) type_names: Vec<String>,
    pub(crate) effect_names: Vec<String>,
    /// True while checking core sources: names resolve against the
    /// core definitions only.
    pub(crate) core_scope: bool,
}

/// Shared module state for all function checkers.
pub(crate) struct Ctx {
    pub(crate) store: TypeStore,
    pub(crate) classes: Vec<ClassInfo>,
    pub(crate) user_types: HashMap<String, u32>,
    pub(crate) core_types: HashMap<String, u32>,
    pub(crate) prelude: bool,
    pub(crate) func_index: HashMap<String, u32>,
    pub(crate) sigs: Vec<FnSig>,
    pub(crate) funcs: Vec<Option<HirFunc>>,
    pub(crate) core: CoreIds,
    /// The import slots the module needs, in slot order.
    pub(crate) imports: Vec<HirImport>,
    /// The `use` bindings of the module, by bound name. They resolve
    /// below locals and module definitions. They never grant
    /// authority and never change a row.
    pub(crate) uses: HashMap<String, UseBinding>,
    /// The class index where the module classes start. Every earlier
    /// class belongs to the pinned core image.
    pub(crate) user_start: u32,
    /// The class index where the imported declarations start. The
    /// classes between `user_start` and here belong to this module.
    pub(crate) import_start: u32,
}

impl Ctx {
    /// Where one class comes from. The registration order fixes the
    /// three ranges: the core, then this module, then the imports.
    pub(crate) fn class_origin(&self, class: u32) -> crate::iface::ClassOrigin {
        if class < self.user_start {
            return crate::iface::ClassOrigin::Core;
        }
        if class < self.import_start {
            return crate::iface::ClassOrigin::Local;
        }
        for import in &self.imports {
            if import.kind == lm_bytecode::ImportKind::Class {
                if let HirImportDef::Class(c) = import.def {
                    if c == class {
                        return crate::iface::ClassOrigin::Imported(
                            import.module.clone(),
                            import.name.clone(),
                        );
                    }
                }
            }
        }
        crate::iface::ClassOrigin::Local
    }

    /// Look up a class or enum type name in the given scope.
    pub(crate) fn lookup_type(&self, name: &str, env: &TyEnv) -> Option<u32> {
        if env.core_scope {
            return self.core_types.get(name).copied();
        }
        if let Some(idx) = self.user_types.get(name) {
            return Some(*idx);
        }
        if self.prelude && PRELUDE_TYPES.contains(&name) {
            return self.core_types.get(name).copied();
        }
        None
    }

    /// Find a method by name and return the type arguments of the
    /// declaring class, seen from `class`. The list is empty when the
    /// declaring class has no type parameters.
    pub(crate) fn find_method_owner(
        &mut self,
        class: u32,
        name: &str,
    ) -> Option<(MethodSig, Vec<TypeId>)> {
        let arity = self.classes[class as usize].type_params.len();
        self.lookup_method(class, Vec::new(), arity, name)
    }

    /// The method `name` that the ancestor chain from `start` answers.
    /// `args` holds the type arguments of `start` seen from the class
    /// the caller asked about, and `arity` is that class's own generic
    /// arity.
    pub(crate) fn lookup_method(
        &mut self,
        start: u32,
        args: Vec<TypeId>,
        arity: usize,
        name: &str,
    ) -> Option<(MethodSig, Vec<TypeId>)> {
        let mut cur = start;
        let mut cur_args = args;
        loop {
            let found = self.classes[cur as usize]
                .methods
                .iter()
                .find(|m| m.name == name)
                .cloned();
            if let Some(sig) = found {
                if cur_args.is_empty() {
                    return Some((sig, cur_args));
                }
                let sig = self.substitute_method(&sig, &cur_args, arity);
                return Some((sig, cur_args));
            }
            let meta = self.store.class_meta(ClassId(cur));
            let parent = meta.parent?;
            cur_args = meta.parent_args.clone();
            cur = parent.0;
        }
    }

    /// Re-express one inherited method signature in the type
    /// parameters of the subclass. `args` replaces the class
    /// parameters of the declaring class, and the method's own
    /// parameters move down to follow the subclass parameters.
    fn substitute_method(&mut self, sig: &MethodSig, args: &[TypeId], arity: usize) -> MethodSig {
        let mut targs = args.to_vec();
        for i in 0..sig.own_type_params.len() {
            let var = self.store.intern(Type::Var((arity + i) as u32));
            targs.push(var);
        }
        let params = sig
            .params
            .iter()
            .map(|p| self.store.substitute(*p, &targs, &[]))
            .collect();
        let ret = self.store.substitute(sig.ret, &targs, &[]);
        MethodSig {
            params,
            ret,
            ..sig.clone()
        }
    }

    /// Find a field layout index by name.
    pub(crate) fn find_field(&self, class: u32, name: &str) -> Option<usize> {
        self.classes[class as usize]
            .field_names
            .iter()
            .position(|n| n == name)
    }

    /// The enum family index of a class, when it belongs to one.
    pub(crate) fn family_of(&self, class: u32) -> Option<u32> {
        let info = &self.classes[class as usize];
        match info.kind {
            ClassKind::EnumParent => Some(class),
            ClassKind::EnumCase => info.family,
            ClassKind::Normal => None,
        }
    }

    /// Find an arm of a family by its short name.
    pub(crate) fn find_arm(&self, family: u32, short: &str) -> Option<u32> {
        self.classes[family as usize]
            .arms
            .iter()
            .copied()
            .find(|arm| self.classes[*arm as usize].arm_short == short)
    }

    /// The families whose unqualified constructor names are in scope.
    pub(crate) fn ctor_families(&self, env: &TyEnv, ctor: &str) -> Vec<u32> {
        let mut families = Vec::new();
        if env.core_scope {
            for idx in self.core_types.values() {
                if self.classes[*idx as usize].kind == ClassKind::EnumParent {
                    families.push(*idx);
                }
            }
        } else {
            for idx in self.user_types.values() {
                if self.classes[*idx as usize].kind == ClassKind::EnumParent {
                    families.push(*idx);
                }
            }
            if self.prelude && PRELUDE_CTORS.contains(&ctor) {
                for name in ["Option", "Result"] {
                    if let Some(idx) = self.core_types.get(name) {
                        families.push(*idx);
                    }
                }
            }
        }
        families.sort_unstable();
        families
            .into_iter()
            .filter(|f| self.find_arm(*f, ctor).is_some())
            .collect()
    }

    /// Register one more checked function and return its index.
    pub(crate) fn push_func(&mut self, func: HirFunc, sig: FnSig) -> u32 {
        let idx = self.funcs.len() as u32;
        self.funcs.push(Some(func));
        self.sigs.push(sig);
        idx
    }

    /// The `Option[arg]` instance type for the pinned core `Option`.
    pub(crate) fn option_of(&mut self, arg: TypeId) -> TypeId {
        let class = ClassId(self.core.option_class);
        self.store.intern(Type::Inst(class, vec![arg]))
    }
}

pub(crate) fn resolve_type(
    ctx: &mut Ctx,
    env: &TyEnv,
    ty: &ast::TypeExpr,
) -> Result<TypeId, Diagnostic> {
    match &ty.kind {
        ast::TypeExprKind::Unit => Ok(UNIT),
        ast::TypeExprKind::Name(name) => {
            if let Some(pos) = env.type_names.iter().position(|n| n == name) {
                return Ok(ctx.store.intern(Type::Var(pos as u32)));
            }
            if let Some(id) = ctx.store.by_name(name) {
                return Ok(id);
            }
            if let Some(class) = ctx.lookup_type(name, env) {
                // The store metadata is complete after predeclaration,
                // before the `ClassInfo` entries exist, so forward and
                // recursive type references resolve here.
                let arity = ctx.store.class_meta(ClassId(class)).type_params;
                if arity != 0 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("the generic type `{name}` needs {arity} type argument(s)"),
                        ty.span,
                    ));
                }
                return Ok(ctx.store.intern(Type::Class(ClassId(class))));
            }
            if name == "List" || name == "Map" {
                return Err(Diagnostic::new(
                    "E1024",
                    format!("the generic type `{name}` needs type arguments"),
                    ty.span,
                ));
            }
            Err(Diagnostic::new(
                "E1013",
                format!("unknown type name `{name}`"),
                ty.span,
            ))
        }
        ast::TypeExprKind::Apply(name, args) => match name.as_str() {
            "Vm" => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("`Vm` takes 1 type argument, found {}", args.len()),
                        ty.span,
                    ));
                }
                let result = resolve_type(ctx, env, &args[0])?;
                Ok(ctx.store.intern(Type::Vm(result)))
            }
            "Handle" => {
                if args.len() != 2 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("`Handle` takes 2 type arguments, found {}", args.len()),
                        ty.span,
                    ));
                }
                let mailbox = resolve_type(ctx, env, &args[0])?;
                let result = resolve_type(ctx, env, &args[1])?;
                Ok(ctx.store.intern(Type::Handle(mailbox, result)))
            }
            "List" => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("`List` takes 1 type argument, found {}", args.len()),
                        ty.span,
                    ));
                }
                let elem = resolve_type(ctx, env, &args[0])?;
                Ok(ctx.store.intern(Type::List(elem)))
            }
            "Map" => {
                if args.len() != 2 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("`Map` takes 2 type arguments, found {}", args.len()),
                        ty.span,
                    ));
                }
                let key = resolve_type(ctx, env, &args[0])?;
                check_key_type(ctx, key, args[0].span)?;
                let value = resolve_type(ctx, env, &args[1])?;
                Ok(ctx.store.intern(Type::Map(key, value)))
            }
            other => {
                if let Some(class) = ctx.lookup_type(other, env) {
                    let arity = ctx.store.class_meta(ClassId(class)).type_params as usize;
                    if arity == 0 {
                        return Err(Diagnostic::new(
                            "E1024",
                            format!("the type `{other}` does not take type arguments"),
                            ty.span,
                        ));
                    }
                    if args.len() != arity {
                        return Err(Diagnostic::new(
                            "E1024",
                            format!(
                                "`{other}` takes {arity} type argument(s), found {}",
                                args.len()
                            ),
                            ty.span,
                        ));
                    }
                    let mut resolved = Vec::with_capacity(args.len());
                    for arg in args {
                        resolved.push(resolve_type(ctx, env, arg)?);
                    }
                    return Ok(ctx.store.intern(Type::Inst(ClassId(class), resolved)));
                }
                Err(Diagnostic::new(
                    "E1013",
                    format!("unknown type name `{other}`"),
                    ty.span,
                ))
            }
        },
        ast::TypeExprKind::ListShort(elem) => {
            let elem = resolve_type(ctx, env, elem)?;
            Ok(ctx.store.intern(Type::List(elem)))
        }
        ast::TypeExprKind::MapShort(key, value) => {
            let key_ty = resolve_type(ctx, env, key)?;
            check_key_type(ctx, key_ty, key.span)?;
            let value = resolve_type(ctx, env, value)?;
            Ok(ctx.store.intern(Type::Map(key_ty, value)))
        }
        ast::TypeExprKind::Tuple(elems) => {
            let mut resolved = Vec::with_capacity(elems.len());
            for elem in elems {
                resolved.push(resolve_type(ctx, env, elem)?);
            }
            Ok(ctx.store.intern(Type::Tuple(resolved)))
        }
        ast::TypeExprKind::Fn(params, muts, ret, row) => {
            let mut ptys = Vec::new();
            for p in params {
                ptys.push(resolve_type(ctx, env, p)?);
            }
            let ret = resolve_type(ctx, env, ret)?;
            let row = resolve_row(ctx, env, row)?;
            Ok(ctx.store.intern_fn(ptys, muts.clone(), ret, row))
        }
    }
}

/// Resolve declared row items to canonical row elements.
pub(crate) fn resolve_row(
    ctx: &mut Ctx,
    env: &TyEnv,
    items: &[ast::RowItem],
) -> Result<Row, Diagnostic> {
    let mut row = Vec::with_capacity(items.len());
    for item in items {
        if let Some(pos) = env.effect_names.iter().position(|n| n == &item.name) {
            row.push(RowElem::Var(pos as u32));
            continue;
        }
        if lm_abi::row_name_valid(&item.name) {
            let idx = ctx.store.intern_row_name(&item.name);
            row.push(RowElem::Op(idx));
            continue;
        }
        let starts_upper = item
            .name
            .chars()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false);
        if item.name.contains('.') || starts_upper {
            return Err(Diagnostic::new(
                "E1050",
                format!(
                    "`{}` is not an operation or group in the operation manifest",
                    item.name
                ),
                item.span,
            ));
        }
        return Err(Diagnostic::new(
            "E1046",
            format!(
                "unknown effect name `{}`; declare it with `effect {}` in the \
                 generic parameter list",
                item.name, item.name
            ),
            item.span,
        ));
    }
    Ok(ctx.store.canonical_row(row))
}

pub(crate) fn check_key_type(ctx: &Ctx, key: TypeId, span: Span) -> Result<(), Diagnostic> {
    if matches!(key, lm_types::BOOL | lm_types::INT | lm_types::STRING) {
        Ok(())
    } else {
        Err(Diagnostic::new(
            "E1033",
            format!(
                "a map key must be Bool, Int, or String, found {}",
                ctx.store.display(key)
            ),
            span,
        ))
    }
}

/// Split generic parameters into type names and effect names.
fn split_generics(generics: &[ast::GenericParam]) -> (Vec<String>, Vec<String>) {
    let mut type_names = Vec::new();
    let mut effect_names = Vec::new();
    for g in generics {
        if g.is_effect {
            effect_names.push(g.name.clone());
        } else {
            type_names.push(g.name.clone());
        }
    }
    (type_names, effect_names)
}

/// Check a parsed module and produce typed HIR with the default
/// options.
pub fn check_module(module: &ast::Module) -> Result<HirModule, Diagnostic> {
    check_module_with(module, CheckOptions::default())
}

/// Check a parsed module and produce typed HIR.
pub fn check_module_with(
    module: &ast::Module,
    options: CheckOptions,
) -> Result<HirModule, Diagnostic> {
    // The pinned core image owns the module path `core`. A source
    // module with that path would give a user class a core qualified
    // key, and the linker merges on that key.
    if options.module_path == lm_bytecode::CORE_MODULE {
        return Err(Diagnostic::new(
            "E0290",
            "the module path `core` belongs to the core image; rename the file or the package",
            Span::new(0, 0),
        ));
    }
    let core = core_ast();
    let mut ctx = Ctx {
        store: TypeStore::new(),
        classes: Vec::new(),
        user_types: HashMap::new(),
        core_types: HashMap::new(),
        prelude: options.prelude,
        func_index: HashMap::new(),
        sigs: Vec::new(),
        funcs: Vec::new(),
        core: CoreIds {
            option_class: 0,
            some_class: 0,
            none_class: 0,
        },
        uses: HashMap::new(),
        imports: Vec::new(),
        user_start: 0,
        import_start: 0,
    };
    // Pass 1: predeclare all type names. The core comes first, so a
    // core class that a module class inherits always keeps the lower
    // class index. Every later table reads that order: the verifier,
    // the dispatch builder, and the linker all require a parent to
    // precede its child.
    register_type_names(&mut ctx, core, true).expect("the core type names register");
    ctx.user_start = ctx.store.class_count() as u32;
    register_type_names(&mut ctx, module, false)?;
    ctx.import_start = ctx.store.class_count() as u32;
    // Import phase A: reserve the imported class indices before any
    // signature resolves, so a user signature may name an imported
    // type. Phase B fills the declarations after the core lands.
    let mut materializer = crate::import::Materializer::new(&options.imports);
    ctx.uses = resolve_uses(&mut ctx, &mut materializer, &options.imports, &module.uses)?;
    link_class_parents(&mut ctx, module, false)?;
    // The core has no user-style inheritance, only enum families,
    // which were linked during registration.
    let option_class = ctx.core_types["Option"];
    ctx.core = CoreIds {
        option_class,
        some_class: option_class + 1,
        none_class: option_class + 2,
    };
    // Pass 2a: predeclare top-level function signatures.
    for (idx, func) in module.funcs.iter().enumerate() {
        if ctx.func_index.contains_key(&func.name) || ctx.user_types.contains_key(&func.name) {
            return Err(Diagnostic::new(
                "E1010",
                format!("the name `{}` has more than one definition", func.name),
                func.name_span,
            ));
        }
        let (type_names, effect_names) = split_generics(&func.generics);
        let env = TyEnv {
            type_names: type_names.clone(),
            effect_names: effect_names.clone(),
            core_scope: false,
        };
        let sig = resolve_sig(
            &mut ctx,
            &env,
            type_names,
            effect_names,
            &func.params,
            &func.ret,
            &func.row,
            None,
        )?;
        ctx.func_index.insert(func.name.clone(), idx as u32);
        ctx.sigs.push(sig);
        ctx.funcs.push(None);
    }
    // The class table is index addressed from here on. Registration
    // fixed every index, so a later pass may fill the entries in any
    // order. The core resolves first, because a user class may name a
    // core class as its parent.
    ctx.classes = (0..ctx.import_start).map(ClassInfo::placeholder).collect();
    // Pass 2b: resolve core classes and enums.
    resolve_all_classes(&mut ctx, core, true).map_err(core_defect)?;
    // Pass 2c: resolve user classes and enums in class-index order.
    resolve_all_classes(&mut ctx, module, false)?;
    // Reserve the entry function index.
    let entry_idx = ctx.funcs.len();
    ctx.funcs.push(None);
    ctx.sigs.push(FnSig {
        type_params: vec![],
        effect_params: vec![],
        params: vec![],
        param_muts: vec![],
        param_names: vec![],
        ret: UNIT,
        row: vec![],
    });
    // Import phase B: fill the imported declarations. The class table
    // holds the user and the core classes now, so an imported
    // signature may name any of them. Phase B runs before any body
    // and before any field default, because both may name an
    // imported class.
    let import_span = module
        .uses
        .first()
        .map(|u| u.span)
        .unwrap_or(Span::new(0, 0));
    let import_fields = materializer.finish(&mut ctx, import_span)?;
    // Pass 3: check field defaults. The table is index addressed, like
    // the class table.
    let mut own_defaults: Vec<Vec<(Option<HExpr>, Vec<TypeId>)>> =
        vec![Vec::new(); ctx.import_start as usize];
    check_defaults(&mut ctx, module, false, &mut own_defaults)?;
    check_defaults(&mut ctx, core, true, &mut own_defaults).map_err(core_defect)?;
    // An imported declaration carries no default expression: the
    // provider construction function evaluates its own defaults. The
    // entries follow the user and the core classes, so the table
    // stays aligned with the class indices.
    for count in import_fields {
        own_defaults.push(vec![(None, Vec::new()); count]);
    }
    // Pass 4: check top-level function bodies.
    for (idx, func) in module.funcs.iter().enumerate() {
        let sig = ctx.sigs[idx].clone();
        let env = TyEnv {
            type_names: sig.type_params.clone(),
            effect_names: sig.effect_params.clone(),
            core_scope: false,
        };
        let mut checker = FnChecker::top_level(RetKind::Known(sig.ret), env, sig.row.clone());
        for (slot, param) in func.params.iter().enumerate() {
            checker
                .locals
                .push((sig.params[slot], sig.param_muts[slot]));
            if checker.scopes[0]
                .insert(param.name.clone(), slot as u32)
                .is_some()
            {
                return Err(Diagnostic::new(
                    "E1014",
                    format!("duplicate parameter name `{}`", param.name),
                    param.span,
                ));
            }
        }
        let checked = checker.check_callable(&mut ctx, &func.body, sig.ret, func.span)?;
        ctx.funcs[idx] = Some(HirFunc {
            imported: false,
            name: func.name.clone(),
            type_params: sig.type_params.len() as u32,
            effect_params: sig.effect_params.len() as u32,
            params: sig.params.clone(),
            param_muts: sig.param_muts.clone(),
            ret: sig.ret,
            row: sig.row.clone(),
            captures: vec![],
            locals: checked.locals,
            body: checked.body,
        });
    }
    // Pass 5: check user method bodies, then core method bodies.
    check_all_methods(&mut ctx, module, false)?;
    check_all_methods(&mut ctx, core, true).map_err(core_defect)?;
    // Pass 6: check the entry statements.
    let entry_span = module
        .entry
        .last()
        .map(|s| s.span)
        .unwrap_or(Span::new(0, 0));
    let checker = FnChecker::entry_collect(TyEnv::default());
    let (body, entry_ty, _mutable, locals, entry_row) =
        checker.check_entry(&mut ctx, &module.entry, entry_span)?;
    let entry_ty = if entry_ty == NEVER { UNIT } else { entry_ty };
    let exports = collect_exports(&ctx, module, &options.module_path)?;
    ctx.funcs[entry_idx] = Some(HirFunc {
        imported: false,
        name: "<entry>".to_string(),
        type_params: 0,
        effect_params: 0,
        params: vec![],
        param_muts: vec![],
        ret: entry_ty,
        row: entry_row,
        captures: vec![],
        locals,
        body,
    });
    assemble(
        ctx,
        own_defaults,
        entry_idx,
        exports,
        &options.module_path,
        module.funcs.len(),
    )
}

/// Collect the exported top-level definitions of the source module,
/// in declaration order. The embedded core and every imported
/// declaration stay out: a module exports only what it defines.
fn collect_exports(
    ctx: &Ctx,
    module: &ast::Module,
    module_path: &str,
) -> Result<Vec<HirExport>, Diagnostic> {
    let naming = crate::iface::Naming { ctx, module_path };
    let mut out: Vec<(lm_bytecode::ExportKind, String, u32)> = Vec::new();
    let class_index = |name: &str| -> u32 {
        *ctx.user_types
            .get(name)
            .expect("every user class name registers")
    };
    for class in &module.classes {
        out.push((
            lm_bytecode::ExportKind::Class,
            class.name.clone(),
            class_index(&class.name),
        ));
    }
    for enum_def in &module.enums {
        let parent = class_index(&enum_def.name);
        out.push((lm_bytecode::ExportKind::Enum, enum_def.name.clone(), parent));
        for arm in &enum_def.arms {
            let full = format!("{}.{}", enum_def.name, arm.name);
            let idx = ctx
                .find_arm(parent, &arm.name)
                .expect("every declared arm registers");
            out.push((lm_bytecode::ExportKind::EnumCase, full, idx));
        }
    }
    for func in &module.funcs {
        out.push((
            lm_bytecode::ExportKind::Function,
            func.name.clone(),
            ctx.func_index[&func.name],
        ));
    }
    Ok(out
        .into_iter()
        .map(|(kind, name, def)| HirExport {
            kind,
            name,
            def,
            item: naming.item(kind, def),
        })
        .collect())
}

/// A defect in the pinned core sources is an implementation defect,
/// not a user diagnostic.
fn core_defect(d: Diagnostic) -> Diagnostic {
    panic!(
        "the pinned core sources do not check: {} {}",
        d.code, d.message
    );
}

/// Build the checked module. `source_funcs` is the number of
/// top-level functions the source declares. Those take the first
/// function indices, so the binding pass reads exactly them.
fn assemble(
    ctx: Ctx,
    own_defaults: Vec<Vec<(Option<HExpr>, Vec<TypeId>)>>,
    entry_idx: usize,
    exports: Vec<HirExport>,
    module_path: &str,
    source_funcs: usize,
) -> Result<HirModule, Diagnostic> {
    let keys: Vec<String> = {
        let naming = crate::iface::Naming {
            ctx: &ctx,
            module_path,
        };
        (0..ctx.classes.len() as u32)
            .map(|c| naming.key(c))
            .collect()
    };
    let mut hir_classes: Vec<HirClass> = Vec::new();
    for (idx, info) in ctx.classes.iter().enumerate() {
        // A class inherits the field defaults of its ancestors. The
        // parent index may be greater than the child index, because a
        // module class may inherit a core class, so the walk collects
        // the chain instead of reading an earlier result.
        let mut chain: Vec<usize> = vec![idx];
        let mut cur = info.parent;
        while let Some(p) = cur {
            chain.push(p as usize);
            cur = ctx.classes[p as usize].parent;
        }
        let mut defaults: Vec<Option<HExpr>> = Vec::new();
        let mut default_locals: Vec<Vec<TypeId>> = Vec::new();
        for c in chain.iter().rev() {
            for (expr, locals) in &own_defaults[*c] {
                defaults.push(expr.clone());
                default_locals.push(locals.clone());
            }
        }
        debug_assert_eq!(defaults.len(), info.field_tys.len());
        let ctor_kind = if info.kind == ClassKind::EnumCase {
            CtorKind::CaseFields
        } else if info.init.is_some() {
            CtorKind::Init
        } else {
            CtorKind::Defaults
        };
        let (ctor_params, ctor_param_muts) = match (&info.init, info.kind) {
            (_, ClassKind::EnumCase) => {
                let count = info.field_tys.len();
                (info.field_tys.clone(), vec![false; count])
            }
            (Some(init), _) => (init.params.clone(), init.param_muts.clone()),
            (None, _) => (vec![], vec![]),
        };
        let ctor_row = info
            .init
            .as_ref()
            .map(|m| m.row.clone())
            .unwrap_or_default();
        hir_classes.push(HirClass {
            imported: info.imported,
            name: info.name.clone(),
            key: keys[idx].clone(),
            parent: info.parent,
            parent_args: ctx
                .store
                .class_meta(ClassId(idx as u32))
                .parent_args
                .clone(),
            type_params: info.type_params.len() as u32,
            kind: info.kind,
            ctor_kind,
            field_names: info.field_names.clone(),
            field_tys: info.field_tys.clone(),
            defaults,
            default_locals,
            methods: info
                .methods
                .iter()
                .map(|m| (m.name.clone(), m.func))
                .collect(),
            init: info.init.as_ref().map(|m| m.func),
            ctor_params,
            ctor_param_muts,
            ctor_row,
        });
    }
    // The stable core role slots. The compiler knows which class fills
    // each role, so the artifact carries the table and no later pass
    // resolves a core class by name or by hash.
    let mut core_roles = [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT];
    for (idx, class) in hir_classes.iter().enumerate() {
        let Some(label) = class.key.strip_prefix("core.") else {
            continue;
        };
        if let Some(role) = lm_bytecode::corepin::role_index(label) {
            if core_roles[role] == lm_bytecode::NO_ROLE {
                core_roles[role] = idx as u32;
            }
        }
    }
    let funcs: Vec<HirFunc> = ctx
        .funcs
        .into_iter()
        .map(|f| f.expect("every reserved function is checked"))
        .collect();
    // The named function bindings this module declares. A name points
    // at a function value; it is never a part of that value. A free
    // function takes the module path as its root, and a class member
    // takes the qualified key of its class, so an embedded core copy
    // binds the same names in every module.
    let mut bindings: Vec<lm_bytecode::FuncBinding> = Vec::new();
    for (idx, func) in funcs.iter().enumerate().take(source_funcs) {
        if func.imported {
            continue;
        }
        bindings.push(lm_bytecode::FuncBinding {
            key: lm_bytecode::qualified_key(module_path, &func.name),
            func: idx as u32,
            class: lm_bytecode::NO_CLASS,
        });
    }
    for class in &hir_classes {
        if class.imported {
            continue;
        }
        for (name, func) in &class.methods {
            bindings.push(lm_bytecode::FuncBinding {
                key: format!("{}.{name}", class.key),
                func: *func,
                class: lm_bytecode::NO_CLASS,
            });
        }
        if let Some(func) = class.init {
            bindings.push(lm_bytecode::FuncBinding {
                key: format!("{}.init", class.key),
                func,
                class: lm_bytecode::NO_CLASS,
            });
        }
    }
    Ok(HirModule {
        store: ctx.store,
        classes: hir_classes,
        funcs,
        entry: entry_idx,
        core: ctx.core,
        core_roles,
        exports,
        imports: ctx.imports,
        bindings,
    })
}

/// Register all class and enum names of one source module.
fn register_type_names(
    ctx: &mut Ctx,
    module: &ast::Module,
    is_core: bool,
) -> Result<(), Diagnostic> {
    let declare = |ctx: &mut Ctx,
                   name: &str,
                   span: Span,
                   type_params: u32,
                   kind: ClassKind|
     -> Result<u32, Diagnostic> {
        let map = if is_core {
            &mut ctx.core_types
        } else {
            &mut ctx.user_types
        };
        if kind != ClassKind::EnumCase && map.contains_key(name) {
            return Err(Diagnostic::new(
                "E1010",
                format!("the name `{name}` has more than one definition"),
                span,
            ));
        }
        let id = ctx.store.register_class(name, type_params, kind);
        if kind != ClassKind::EnumCase {
            let map = if is_core {
                &mut ctx.core_types
            } else {
                &mut ctx.user_types
            };
            map.insert(name.to_string(), id.0);
        }
        Ok(id.0)
    };
    for class in &module.classes {
        let (type_names, effect_names) = split_generics(&class.generics);
        if !effect_names.is_empty() {
            return Err(Diagnostic::new(
                "E1024",
                "a class cannot declare effect parameters",
                class.name_span,
            ));
        }
        declare(
            ctx,
            &class.name,
            class.name_span,
            type_names.len() as u32,
            ClassKind::Normal,
        )?;
    }
    for enum_def in &module.enums {
        let (type_names, effect_names) = split_generics(&enum_def.generics);
        if !effect_names.is_empty() {
            return Err(Diagnostic::new(
                "E1024",
                "an enum cannot declare effect parameters",
                enum_def.name_span,
            ));
        }
        let parent = declare(
            ctx,
            &enum_def.name,
            enum_def.name_span,
            type_names.len() as u32,
            ClassKind::EnumParent,
        )?;
        let mut seen: Vec<&str> = Vec::new();
        for arm in &enum_def.arms {
            if seen.contains(&arm.name.as_str()) {
                return Err(Diagnostic::new(
                    "E1040",
                    format!("the enum already has an arm named `{}`", arm.name),
                    arm.name_span,
                ));
            }
            seen.push(&arm.name);
            let full = format!("{}.{}", enum_def.name, arm.name);
            let arm_id = declare(
                ctx,
                &full,
                arm.name_span,
                type_names.len() as u32,
                ClassKind::EnumCase,
            )?;
            ctx.store.set_class_parent(ClassId(arm_id), ClassId(parent));
        }
    }
    Ok(())
}

/// Link declared parent classes and reject invalid inheritance.
fn link_class_parents(
    ctx: &mut Ctx,
    module: &ast::Module,
    is_core: bool,
) -> Result<(), Diagnostic> {
    for class in &module.classes {
        let map = if is_core {
            &ctx.core_types
        } else {
            &ctx.user_types
        };
        let idx = map[&class.name];
        if let Some(clause) = &class.parent {
            let pname = &clause.name;
            let pspan = &clause.span;
            if !class.generics.is_empty() {
                return Err(Diagnostic::new(
                    "E1024",
                    "a generic class cannot declare a parent in this slice",
                    *pspan,
                ));
            }
            // The parent name resolves like every other type name: a
            // module type first, then a core type the prelude names.
            let env = TyEnv {
                type_names: Vec::new(),
                effect_names: Vec::new(),
                core_scope: is_core,
            };
            let parent = ctx.lookup_type(pname, &env).ok_or_else(|| {
                Diagnostic::new("E1038", format!("unknown parent class `{pname}`"), *pspan)
            })?;
            let parent_meta = ctx.store.class_meta(ClassId(parent)).clone();
            match parent_meta.kind {
                ClassKind::EnumParent | ClassKind::EnumCase => {
                    return Err(Diagnostic::new(
                        "E1040",
                        format!("`{pname}` is part of a sealed enum and cannot be a parent"),
                        *pspan,
                    ));
                }
                ClassKind::Normal => {}
            }
            if parent >= ctx.import_start {
                // An imported class carries a signature and no body,
                // so a subclass cannot reach its `init`. Inheritance
                // across a module boundary needs a slot kind that
                // week 6 does not define.
                return Err(Diagnostic::new(
                    "E1038",
                    format!(
                        "`{pname}` is an imported class, and a class cannot \
                         inherit one; hold it in a field instead"
                    ),
                    *pspan,
                ));
            }
            // A parent must precede its subclass in the class table.
            // The core registers before every module class, so a core
            // parent always satisfies the rule.
            if parent >= idx {
                return Err(Diagnostic::new(
                    "E1038",
                    format!("the parent class `{pname}` must be declared before the subclass"),
                    *pspan,
                ));
            }
            // A bare `Proc` parent is sugar for `Proc[Never]`: the
            // proc takes no message (specification 18.1).
            let bare_proc =
                clause.args.is_empty() && ctx.core_types.get("Proc").copied() == Some(parent);
            if bare_proc {
                ctx.store
                    .set_class_parent_args(ClassId(idx), ClassId(parent), vec![NEVER]);
                continue;
            }
            // A generic parent carries one type argument per parameter.
            // The subclass declares no type parameters, so the
            // arguments are closed types.
            if clause.args.len() != parent_meta.type_params as usize {
                let want = parent_meta.type_params;
                let found = clause.args.len();
                return Err(Diagnostic::new(
                    "E1024",
                    format!(
                        "the parent class `{pname}` takes {want} type argument(s), \
                         found {found}"
                    ),
                    *pspan,
                ));
            }
            let mut args = Vec::with_capacity(clause.args.len());
            for arg in &clause.args {
                args.push(resolve_type(ctx, &env, arg)?);
            }
            ctx.store
                .set_class_parent_args(ClassId(idx), ClassId(parent), args);
        }
    }
    Ok(())
}

/// Resolve the classes and enums of one source module into
/// `ClassInfo` entries, in class-index order.
fn resolve_all_classes(
    ctx: &mut Ctx,
    module: &ast::Module,
    is_core: bool,
) -> Result<(), Diagnostic> {
    for class in &module.classes {
        let map = if is_core {
            &ctx.core_types
        } else {
            &ctx.user_types
        };
        let idx = map[&class.name];
        let info = resolve_class(ctx, class, idx, is_core)?;
        ctx.classes[idx as usize] = info;
    }
    for enum_def in &module.enums {
        resolve_enum(ctx, enum_def, is_core)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn resolve_sig(
    ctx: &mut Ctx,
    env: &TyEnv,
    type_params: Vec<String>,
    effect_params: Vec<String>,
    params: &[ast::Param],
    ret: &Option<ast::TypeExpr>,
    row: &[ast::RowItem],
    self_ty: Option<(TypeId, bool)>,
) -> Result<FnSig, Diagnostic> {
    let mut ptys = Vec::new();
    let mut muts = Vec::new();
    let mut names = Vec::new();
    if let Some((ty, mutable)) = self_ty {
        ptys.push(ty);
        muts.push(mutable);
        names.push("self".to_string());
    }
    let mut seen: Vec<&str> = Vec::new();
    for param in params {
        if seen.contains(&param.name.as_str()) {
            return Err(Diagnostic::new(
                "E1014",
                format!("duplicate parameter name `{}`", param.name),
                param.span,
            ));
        }
        seen.push(&param.name);
        ptys.push(resolve_type(ctx, env, &param.ty)?);
        muts.push(param.mutable);
        names.push(param.name.clone());
    }
    let ret = match ret {
        Some(ty) => resolve_type(ctx, env, ty)?,
        None => UNIT,
    };
    let row = resolve_row(ctx, env, row)?;
    Ok(FnSig {
        type_params,
        effect_params,
        params: ptys,
        param_muts: muts,
        param_names: names,
        ret,
        row,
    })
}

/// Resolve one method declaration into a signature and reserve its
/// function index.
fn resolve_method_sig(
    ctx: &mut Ctx,
    method: &ast::MethodDef,
    class_type_names: &[String],
    self_ty: TypeId,
    is_core: bool,
    is_init: bool,
) -> Result<MethodSig, Diagnostic> {
    let (own_type, own_effect) = split_generics(&method.generics);
    if is_init && (!own_type.is_empty() || !own_effect.is_empty()) {
        return Err(Diagnostic::new(
            "E1038",
            "`init` cannot declare generic parameters",
            method.name_span,
        ));
    }
    let mut type_names = class_type_names.to_vec();
    for own in &own_type {
        if type_names.contains(own) {
            return Err(Diagnostic::new(
                "E1014",
                format!("duplicate generic parameter name `{own}`"),
                method.name_span,
            ));
        }
        type_names.push(own.clone());
    }
    let env = TyEnv {
        type_names: type_names.clone(),
        effect_names: own_effect.clone(),
        core_scope: is_core,
    };
    let mut_self = is_init || method.mut_self;
    let sig = resolve_sig(
        ctx,
        &env,
        type_names,
        own_effect.clone(),
        &method.params,
        &method.ret,
        &method.row,
        Some((self_ty, mut_self)),
    )?;
    let func = ctx.funcs.len() as u32;
    ctx.sigs.push(sig.clone());
    ctx.funcs.push(None);
    Ok(MethodSig {
        name: method.name.clone(),
        func,
        mut_self: method.mut_self,
        params: sig.params[1..].to_vec(),
        param_muts: sig.param_muts[1..].to_vec(),
        param_names: sig.param_names[1..].to_vec(),
        ret: if is_init { UNIT } else { sig.ret },
        row: sig.row,
        own_type_params: own_type,
        own_effect_params: own_effect,
    })
}

/// Resolve one class declaration: layout, methods, and `init`.
fn resolve_class(
    ctx: &mut Ctx,
    class: &ast::ClassDef,
    idx: u32,
    is_core: bool,
) -> Result<ClassInfo, Diagnostic> {
    // `link_class_parents` already resolved and validated the parent,
    // and it recorded the link in the type store.
    let parent = ctx.store.class_meta(ClassId(idx)).parent.map(|p| p.0);
    let (type_names, _) = split_generics(&class.generics);
    let self_ty = if type_names.is_empty() {
        ctx.store.intern(Type::Class(ClassId(idx)))
    } else {
        let vars: Vec<TypeId> = (0..type_names.len())
            .map(|i| ctx.store.intern(Type::Var(i as u32)))
            .collect();
        ctx.store.intern(Type::Inst(ClassId(idx), vars))
    };
    let env = TyEnv {
        type_names: type_names.clone(),
        effect_names: vec![],
        core_scope: is_core,
    };
    // The subclass layout starts with the parent layout. A generic
    // parent contributes its fields with its type arguments applied.
    let parent_args = ctx.store.class_meta(ClassId(idx)).parent_args.clone();
    let (mut field_names, mut field_tys, mut has_default) = match parent {
        Some(p) => {
            let info = &ctx.classes[p as usize];
            let names = info.field_names.clone();
            let tys = info.field_tys.clone();
            let defaults = info.has_default.clone();
            if !parent_args.is_empty() {
                // A subclass copies the default expressions of its
                // parent. A default whose type names a class parameter
                // would arrive with the parameter still free, so this
                // slice rejects it instead of rewriting the expression.
                for (i, ty) in tys.iter().enumerate() {
                    if defaults[i] && ctx.store.contains_var(*ty) {
                        let span = class.parent.as_ref().map(|p| p.span).unwrap_or(class.span);
                        return Err(Diagnostic::new(
                            "E1024",
                            format!(
                                "the generic parent field `{}` has a default that names a \
                                 class type parameter; give the field an `init` instead",
                                names[i]
                            ),
                            span,
                        ));
                    }
                }
            }
            let tys = tys
                .into_iter()
                .map(|t| ctx.store.substitute(t, &parent_args, &[]))
                .collect();
            (names, tys, defaults)
        }
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    let own_start = field_names.len();
    for field in &class.fields {
        if field_names.contains(&field.name) {
            return Err(Diagnostic::new(
                "E1038",
                format!("the class already has a field named `{}`", field.name),
                field.span,
            ));
        }
        field_names.push(field.name.clone());
        field_tys.push(resolve_type(ctx, &env, &field.ty)?);
        has_default.push(field.default.is_some());
    }
    let mut methods: Vec<MethodSig> = Vec::new();
    let mut init: Option<MethodSig> = None;
    for method in &class.methods {
        if method.name == "freeze" {
            return Err(Diagnostic::new(
                "E1038",
                "`freeze` is a reserved method name",
                method.name_span,
            ));
        }
        if field_names.contains(&method.name)
            || methods.iter().any(|m| m.name == method.name)
            || (method.name == "init" && init.is_some())
        {
            return Err(Diagnostic::new(
                "E1038",
                format!("the class already has a member named `{}`", method.name),
                method.name_span,
            ));
        }
        if method.name == "init" {
            if !method.mut_self {
                return Err(Diagnostic::new(
                    "E1038",
                    "`init` must declare `mut self`",
                    method.name_span,
                ));
            }
            if method.ret.is_some() {
                return Err(Diagnostic::new(
                    "E1038",
                    "`init` cannot declare a result type",
                    method.name_span,
                ));
            }
            let msig = resolve_method_sig(ctx, method, &type_names, self_ty, is_core, true)?;
            init = Some(msig);
            continue;
        }
        let msig = resolve_method_sig(ctx, method, &type_names, self_ty, is_core, false)?;
        // Override compatibility with the nearest ancestor method. The
        // inherited signature is read in the subclass view, so a
        // generic parent compares with its arguments applied.
        if let Some(p) = parent {
            let inherited = ctx
                .lookup_method(p, parent_args.clone(), type_names.len(), &method.name)
                .map(|(sig, _)| sig);
            if let Some(base) = inherited {
                let same_params = base.params == msig.params
                    && base.param_muts == msig.param_muts
                    && base.mut_self == msig.mut_self
                    && base.own_type_params.len() == msig.own_type_params.len()
                    && base.own_effect_params.len() == msig.own_effect_params.len();
                if !same_params {
                    return Err(Diagnostic::new(
                        "E1031",
                        format!(
                            "the override of `{}` must keep the parameter types \
                             and the `mut` markers",
                            method.name
                        ),
                        method.name_span,
                    ));
                }
                if !ctx.store.compatible(base.ret, msig.ret) {
                    return Err(Diagnostic::new(
                        "E1031",
                        format!(
                            "the override of `{}` must keep or narrow the result type",
                            method.name
                        ),
                        method.name_span,
                    ));
                }
                if !ctx.store.row_included(&msig.row, &base.row) {
                    return Err(Diagnostic::new(
                        "E1046",
                        format!(
                            "the override of `{}` widens the effect row; the parent \
                             row is `{}`",
                            method.name,
                            display_row_or_empty(&ctx.store, &base.row)
                        ),
                        method.name_span,
                    ));
                }
            }
        }
        methods.push(msig);
    }
    if init.is_none() {
        if let Some(missing) = has_default.iter().position(|d| !d) {
            return Err(Diagnostic::new(
                "E1038",
                format!(
                    "the class needs an `init` because the field `{}` has no default",
                    field_names[missing]
                ),
                class.name_span,
            ));
        }
    }
    Ok(ClassInfo {
        imported: false,
        name: class.name.clone(),
        parent,
        type_params: type_names,
        kind: ClassKind::Normal,
        self_ty,
        field_names,
        field_tys,
        has_default,
        own_start,
        methods,
        init,
        family: None,
        arms: Vec::new(),
        arm_short: String::new(),
    })
}

fn display_row_or_empty(store: &TypeStore, row: &Row) -> String {
    if row.is_empty() {
        "empty".to_string()
    } else {
        store.display_row(row)
    }
}

/// Resolve one enum: the abstract parent, the case classes, and the
/// family methods on the parent.
fn resolve_enum(ctx: &mut Ctx, enum_def: &ast::EnumDef, is_core: bool) -> Result<(), Diagnostic> {
    let map = if is_core {
        &ctx.core_types
    } else {
        &ctx.user_types
    };
    let parent_idx = map[&enum_def.name];
    let (type_names, _) = split_generics(&enum_def.generics);
    let self_ty = if type_names.is_empty() {
        ctx.store.intern(Type::Class(ClassId(parent_idx)))
    } else {
        let vars: Vec<TypeId> = (0..type_names.len())
            .map(|i| ctx.store.intern(Type::Var(i as u32)))
            .collect();
        ctx.store.intern(Type::Inst(ClassId(parent_idx), vars))
    };
    let env = TyEnv {
        type_names: type_names.clone(),
        effect_names: vec![],
        core_scope: is_core,
    };
    // Resolve arm field types first; then methods on the parent.
    let mut arm_infos = Vec::new();
    let arm_base = parent_idx + 1;
    for (aidx, arm) in enum_def.arms.iter().enumerate() {
        let arm_class = arm_base + aidx as u32;
        let mut field_names = Vec::new();
        let mut field_tys = Vec::new();
        for (fname, fty) in &arm.fields {
            field_names.push(fname.clone());
            field_tys.push(resolve_type(ctx, &env, fty)?);
        }
        arm_infos.push((arm_class, arm.name.clone(), field_names, field_tys));
    }
    let mut methods = Vec::new();
    for method in &enum_def.methods {
        if method.name == "freeze" {
            return Err(Diagnostic::new(
                "E1038",
                "`freeze` is a reserved method name",
                method.name_span,
            ));
        }
        if method.name == "init" {
            return Err(Diagnostic::new(
                "E1040",
                "an enum cannot declare `init`",
                method.name_span,
            ));
        }
        if methods.iter().any(|m: &MethodSig| m.name == method.name) {
            return Err(Diagnostic::new(
                "E1038",
                format!("the enum already has a member named `{}`", method.name),
                method.name_span,
            ));
        }
        let msig = resolve_method_sig(ctx, method, &type_names, self_ty, is_core, false)?;
        methods.push(msig);
    }
    let arms: Vec<u32> = arm_infos.iter().map(|(idx, _, _, _)| *idx).collect();
    ctx.classes[parent_idx as usize] = ClassInfo {
        imported: false,
        name: enum_def.name.clone(),
        parent: None,
        type_params: type_names.clone(),
        kind: ClassKind::EnumParent,
        self_ty,
        field_names: Vec::new(),
        field_tys: Vec::new(),
        has_default: Vec::new(),
        own_start: 0,
        methods,
        init: None,
        family: None,
        arms,
        arm_short: String::new(),
    };
    for (arm_class, short, field_names, field_tys) in arm_infos {
        let arm_self_ty = if type_names.is_empty() {
            ctx.store.intern(Type::Class(ClassId(arm_class)))
        } else {
            let vars: Vec<TypeId> = (0..type_names.len())
                .map(|i| ctx.store.intern(Type::Var(i as u32)))
                .collect();
            ctx.store.intern(Type::Inst(ClassId(arm_class), vars))
        };
        let count = field_tys.len();
        ctx.classes[arm_class as usize] = ClassInfo {
            imported: false,
            name: format!("{}.{}", enum_def.name, short),
            parent: Some(parent_idx),
            type_params: type_names.clone(),
            kind: ClassKind::EnumCase,
            self_ty: arm_self_ty,
            field_names,
            field_tys,
            has_default: vec![false; count],
            own_start: 0,
            methods: Vec::new(),
            init: None,
            family: Some(parent_idx),
            arms: Vec::new(),
            arm_short: short,
        };
    }
    Ok(())
}

/// Check the field default expressions of one source module.
fn check_defaults(
    ctx: &mut Ctx,
    module: &ast::Module,
    is_core: bool,
    own_defaults: &mut [Vec<(Option<HExpr>, Vec<TypeId>)>],
) -> Result<(), Diagnostic> {
    for class in &module.classes {
        let map = if is_core {
            &ctx.core_types
        } else {
            &ctx.user_types
        };
        let idx = map[&class.name] as usize;
        let mut defaults = Vec::new();
        for field in &class.fields {
            let checked = match &field.default {
                Some(expr) => {
                    let own_start = ctx.classes[idx].own_start;
                    let fidx = own_start + defaults.len();
                    let field_ty = ctx.classes[idx].field_tys[fidx];
                    let env = TyEnv {
                        type_names: ctx.classes[idx].type_params.clone(),
                        effect_names: vec![],
                        core_scope: is_core,
                    };
                    let mut checker = FnChecker::top_level(RetKind::Entry, env, vec![]);
                    let expr = checker.check_expr(ctx, expr, field_ty)?;
                    // The temporary slot types of the default follow
                    // it into lowering, so the `<new>` scratch slots
                    // keep their declared types.
                    let locals: Vec<TypeId> = checker.locals.iter().map(|(t, _)| *t).collect();
                    (Some(expr), locals)
                }
                None => (None, Vec::new()),
            };
            defaults.push(checked);
        }
        own_defaults[idx] = defaults;
    }
    for enum_def in &module.enums {
        // The parent has no fields; each arm has required fields.
        let map = if is_core {
            &ctx.core_types
        } else {
            &ctx.user_types
        };
        let parent_idx = map[&enum_def.name] as usize;
        for (aidx, arm) in enum_def.arms.iter().enumerate() {
            own_defaults[parent_idx + 1 + aidx] = vec![(None, Vec::new()); arm.fields.len()];
        }
    }
    Ok(())
}

/// Check every method body of one source module.
fn check_all_methods(ctx: &mut Ctx, module: &ast::Module, is_core: bool) -> Result<(), Diagnostic> {
    for class in &module.classes {
        let map = if is_core {
            &ctx.core_types
        } else {
            &ctx.user_types
        };
        let cidx = map[&class.name];
        for method in &class.methods {
            check_method(ctx, cidx, method, is_core)?;
        }
    }
    for enum_def in &module.enums {
        let map = if is_core {
            &ctx.core_types
        } else {
            &ctx.user_types
        };
        let cidx = map[&enum_def.name];
        for method in &enum_def.methods {
            check_method(ctx, cidx, method, is_core)?;
        }
    }
    Ok(())
}

/// Check one method body, with constructor tracking for `init`.
fn check_method(
    ctx: &mut Ctx,
    cidx: u32,
    method: &ast::MethodDef,
    is_core: bool,
) -> Result<(), Diagnostic> {
    let is_init = method.name == "init";
    let (func_idx, sig) = {
        let info = &ctx.classes[cidx as usize];
        let msig = if is_init {
            info.init.as_ref().expect("init resolved")
        } else {
            info.methods
                .iter()
                .find(|m| m.name == method.name)
                .expect("method resolved")
        };
        (msig.func, ctx.sigs[msig.func as usize].clone())
    };
    let env = TyEnv {
        type_names: sig.type_params.clone(),
        effect_names: sig.effect_params.clone(),
        core_scope: is_core,
    };
    let mut checker = FnChecker::top_level(RetKind::Known(sig.ret), env, sig.row.clone());
    checker.self_class = Some(cidx);
    checker.locals.push((sig.params[0], sig.param_muts[0]));
    checker.scopes[0].insert("self".to_string(), 0);
    for (i, param) in method.params.iter().enumerate() {
        let slot = (i + 1) as u32;
        checker
            .locals
            .push((sig.params[i + 1], sig.param_muts[i + 1]));
        checker.scopes[0].insert(param.name.clone(), slot);
    }
    if is_init {
        let info = &ctx.classes[cidx as usize];
        let needs_super = info
            .parent
            .map(|p| ctx.classes[p as usize].init.is_some())
            .unwrap_or(false);
        checker.ctor = Some(crate::checkfn::CtorCtx {
            class: cidx,
            needs_super,
            state: crate::checkfn::CtorState {
                inited: info.has_default.clone(),
                super_done: false,
            },
        });
    }
    let checked = checker.check_callable(ctx, &method.body, sig.ret, method.span)?;
    // A constructor must complete on its normal exit.
    if is_init && !checked.diverges {
        let checker_state = checked.ctor.expect("ctor state present");
        crate::checkfn::require_complete(ctx, cidx, &checker_state, method.span)?;
    }
    ctx.funcs[func_idx as usize] = Some(HirFunc {
        imported: false,
        name: format!("{}.{}", ctx.classes[cidx as usize].name, method.name),
        type_params: sig.type_params.len() as u32,
        effect_params: sig.effect_params.len() as u32,
        params: sig.params.clone(),
        param_muts: sig.param_muts.clone(),
        ret: sig.ret,
        row: sig.row.clone(),
        captures: vec![],
        locals: checked.locals,
        body: checked.body,
    });
    Ok(())
}
