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
use lm_types::{
    ClassId, ClassKind, InterfaceId, Row, RowElem, Type, TypeId, TypeStore, NEVER, UNIT,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::OnceLock;

/// The concatenated pinned core sources, in canonical file order.
pub const CORE_SOURCE: &str = concat!(
    include_str!("../../../core/option.lm"),
    "\n",
    include_str!("../../../core/result.lm"),
    include_str!("../../../core/control.lm"),
    "\n",
    include_str!("../../../core/ordering.lm"),
    "\n",
    include_str!("../../../core/tuple.lm"),
    "\n",
    include_str!("../../../core/range.lm"),
    "\n",
    include_str!("../../../core/errors.lm"),
    "\n",
    include_str!("../../../core/io.lm"),
    "\n",
    include_str!("../../../core/tty.lm"),
    "\n",
    include_str!("../../../core/signal.lm"),
    "\n",
    include_str!("../../../core/fs.lm"),
    "\n",
    include_str!("../../../core/exec.lm"),
    "\n",
    include_str!("../../../core/network.lm"),
    "\n",
    include_str!("../../../core/tls.lm"),
    "\n",
    include_str!("../../../core/vm.lm"),
    "\n",
    include_str!("../../../core/code.lm"),
    "\n",
    include_str!("../../../core/syntax.lm"),
    "\n",
    include_str!("../../../core/compiler.lm"),
    "\n",
    include_str!("../../../core/proc.lm"),
    "\n",
    include_str!("../../../core/wait.lm"),
    "\n",
    include_str!("../../../core/snapshot.lm"),
    "\n",
    include_str!("../../../core/primitives.lm"),
    "\n",
    include_str!("../../../core/string.lm"),
    "\n",
    include_str!("../../../core/bytes.lm"),
    "\n",
    include_str!("../../../core/collections.lm"),
    "\n",
);

/// The type names the prelude places into unqualified scope.
pub const PRELUDE_TYPES: [&str; 140] = [
    "Option",
    "Result",
    "Ordering",
    "Unit",
    "Tuple2",
    "Tuple3",
    "Tuple4",
    "Tuple5",
    "Tuple6",
    "Tuple7",
    "Tuple8",
    "Tuple9",
    "Tuple10",
    "Tuple11",
    "Tuple12",
    "Tuple13",
    "Tuple14",
    "Tuple15",
    "Tuple16",
    "Range",
    "StepEvent",
    "DriveEvent",
    "Proc",
    "Recv",
    "SendResult",
    "ProcError",
    "Choice",
    "SnapshotError",
    "RestoreError",
    "BranchError",
    "ByteReader",
    "ByteWriter",
    "HexError",
    "StdStream",
    "TtySize",
    "TtyError",
    "RawMode",
    "Tty",
    "SignalKind",
    "SignalError",
    "SignalStream",
    "Signal",
    "Artifact",
    "VerifiedModule",
    "FunctionCode",
    "ClassCode",
    "DefinitionIdentity",
    "DefinitionSpec",
    "DefinitionSource",
    "SlotSpec",
    "Instance",
    "Slot",
    "SlotChange",
    "FunctionDef",
    "ClassDef",
    "FunctionBinding",
    "ClassBinding",
    "CodeError",
    "GrammarVersion",
    "SourceRange",
    "CodeLocation",
    "SyntaxTree",
    "SyntaxElement",
    "SyntaxNode",
    "SyntaxToken",
    "SyntaxTrivia",
    "SyntaxBuilder",
    "ParseStatus",
    "SyntaxDiagnostic",
    "SyntaxParse",
    "LinkEnv",
    "CompileEnv",
    "CompileOptions",
    "CompileErrors",
    "DynValue",
    "FsError",
    "IoError",
    "EnvError",
    "EntropyError",
    "OpenOptions",
    "SeekFrom",
    "FileKind",
    "FileInfo",
    "DirEntry",
    "RenameMode",
    "PipeError",
    "PipeEnd",
    "PipeReader",
    "PipeWriter",
    "ChildInput",
    "ChildOutput",
    "ChildEnv",
    "ExecSpec",
    "ChildStatus",
    "ExecError",
    "Child",
    "Pipe",
    "Exec",
    "IpAddress",
    "SocketAddress",
    "NetError",
    "TcpRead",
    "Shutdown",
    "TcpResource",
    "TcpStream",
    "TcpListener",
    "Tcp",
    "TlsError",
    "TlsStream",
    "UdpDatagram",
    "UdpSocket",
    "Udp",
    "Text",
    "String",
    "Substring",
    "Char",
    "Utf8Error",
    "IndexError",
    "ParseIntError",
    "ParseFloatError",
    "FloatToIntError",
    "Bytes",
    "StringBuilder",
    "ByteBuffer",
    "List",
    "Map",
    "Set",
    "ListIterator",
    "MapIterator",
    "SetIterator",
    "TextIterator",
    "RangeIterator",
    "ListSlice",
    "ListSliceIterator",
    "MapKeys",
    "MapValues",
    "MapEntries",
    "MapKeysIterator",
    "MapValuesIterator",
    "MapEntriesIterator",
];

/// The constructor names the prelude places into unqualified scope.
pub const PRELUDE_CTORS: [&str; 18] = [
    "Some",
    "None",
    "Ok",
    "Err",
    "ReadOnly",
    "WriteOnly",
    "ReadWrite",
    "Create",
    "CreateTruncate",
    "CreateNew",
    "Append",
    "Start",
    "Current",
    "End",
    "InteractionExpression",
    "InteractionDefinitions",
    "InteractionIncomplete",
    "InteractionInvalid",
];

fn is_prelude_type(name: &str) -> bool {
    static NAMES: OnceLock<HashSet<&'static str>> = OnceLock::new();
    NAMES
        .get_or_init(|| PRELUDE_TYPES.into_iter().collect())
        .contains(name)
}

fn tuple_core_arity(name: &str) -> Option<usize> {
    let arity = name.strip_prefix("Tuple")?.parse().ok()?;
    (2..=16).contains(&arity).then_some(arity)
}

pub(crate) fn core_native_repr(name: &str) -> Option<NativeRepr> {
    match name {
        "Unit" => Some(NativeRepr::Unit),
        "Int" => Some(NativeRepr::Int),
        "Float" => Some(NativeRepr::Float),
        "Bool" => Some(NativeRepr::Bool),
        "Text" => Some(NativeRepr::Text),
        "String" => Some(NativeRepr::String),
        "Substring" => Some(NativeRepr::Substring),
        "Char" => Some(NativeRepr::Char),
        "Bytes" => Some(NativeRepr::Bytes),
        "StringBuilder" => Some(NativeRepr::StringBuilder),
        "ByteBuffer" => Some(NativeRepr::ByteBuffer),
        "List" => Some(NativeRepr::List),
        "Map" => Some(NativeRepr::Map),
        "FileHandle" => Some(NativeRepr::FileHandle),
        "TcpResource" => Some(NativeRepr::TcpResource),
        "TcpStream" => Some(NativeRepr::TcpStream),
        "TcpListener" => Some(NativeRepr::TcpListener),
        "TlsStream" => Some(NativeRepr::TlsStream),
        "UdpSocket" => Some(NativeRepr::UdpSocket),
        "Artifact" => Some(NativeRepr::Artifact),
        "VerifiedModule" => Some(NativeRepr::VerifiedModule),
        "FunctionCode" => Some(NativeRepr::FunctionCode),
        "ClassCode" => Some(NativeRepr::ClassCode),
        "SlotSpec" => Some(NativeRepr::SlotSpec),
        "Instance" => Some(NativeRepr::CodeInstance),
        "Slot" => Some(NativeRepr::Slot),
        "FunctionDef" => Some(NativeRepr::FunctionDef),
        "ClassDef" => Some(NativeRepr::ClassDef),
        "FunctionBinding" => Some(NativeRepr::FunctionBinding),
        "ClassBinding" => Some(NativeRepr::ClassBinding),
        "DynValue" => Some(NativeRepr::DynValue),
        name => tuple_core_arity(name).map(|arity| NativeRepr::Tuple(arity as u8)),
    }
}

pub(crate) fn register_core_native_class(store: &mut TypeStore, name: &str, class: ClassId) {
    let primitive = match name {
        "Unit" => Some(lm_types::UNIT),
        "Int" => Some(lm_types::INT),
        "Float" => Some(lm_types::FLOAT),
        "Bool" => Some(lm_types::BOOL),
        "String" => Some(lm_types::STRING),
        "Bytes" => Some(lm_types::BYTES),
        "FileHandle" => Some(lm_types::FILE_HANDLE),
        _ => None,
    };
    if let Some(ty) = primitive {
        store.set_native_class(ty, class);
    }
    match name {
        "List" => store.set_native_list_class(class),
        "Map" => store.set_native_map_class(class),
        name => {
            if let Some(arity) = tuple_core_arity(name) {
                store.set_native_tuple_class(arity, class);
            }
        }
    }
}

pub(crate) fn class_self_type(
    store: &mut TypeStore,
    class: ClassId,
    type_params: u32,
    native_repr: Option<NativeRepr>,
) -> TypeId {
    match native_repr {
        Some(NativeRepr::Unit) => lm_types::UNIT,
        Some(NativeRepr::Int) => lm_types::INT,
        Some(NativeRepr::Float) => lm_types::FLOAT,
        Some(NativeRepr::Bool) => lm_types::BOOL,
        Some(NativeRepr::String) => lm_types::STRING,
        Some(NativeRepr::Bytes) => lm_types::BYTES,
        Some(NativeRepr::FileHandle) => lm_types::FILE_HANDLE,
        Some(NativeRepr::List) => {
            let element = store.intern(Type::Var(0));
            store.intern(Type::List(element))
        }
        Some(NativeRepr::Map) => {
            let key = store.intern(Type::Var(0));
            let value = store.intern(Type::Var(1));
            store.intern(Type::Map(key, value))
        }
        Some(NativeRepr::Tuple(arity)) => {
            let elements = (0..arity)
                .map(|index| store.intern(Type::Var(index as u32)))
                .collect();
            store.intern(Type::Tuple(elements))
        }
        _ if type_params == 0 => store.intern(Type::Class(class)),
        _ => {
            let args = (0..type_params)
                .map(|index| store.intern(Type::Var(index)))
                .collect();
            store.intern(Type::Inst(class, args))
        }
    }
}

/// Checker options. `prelude` controls only unqualified name
/// resolution; the core image itself never depends on it.
#[derive(Debug, Clone)]
pub struct CheckOptions {
    pub prelude: bool,
    /// Build the complete export surface of the pinned core provider.
    pub build_core_provider: bool,
    /// The operation bundle available to this module.
    pub bundle: std::sync::Arc<lm_abi::AbiBundle>,
    /// The module path of the source under compilation, for example
    /// `mathlib.matrix`. It names this module's classes inside the
    /// emitted interface, and it forms the qualified key of every
    /// class this module declares. A structural hash that names one
    /// of those classes therefore follows the module path.
    pub module_path: String,
    /// The interfaces this module may import. The build tool
    /// constructs it from the manifest and the dependency interfaces.
    pub imports: crate::import::ImportEnv,
    /// The compiled core dependency used by an ordinary module.
    /// Core-provider compilation leaves this value empty.
    pub core: Option<std::sync::Arc<lm_bytecode::artifact::LinkUnit>>,
    /// Trusted direct intrinsic summaries from the compiled core.
    pub core_intrinsics: std::sync::Arc<[Option<lm_abi::IntrinsicSlot>]>,
    /// Extra core definitions required by a lowering option.
    pub core_roots: BTreeSet<String>,
}

impl Default for CheckOptions {
    fn default() -> CheckOptions {
        CheckOptions {
            prelude: true,
            build_core_provider: false,
            bundle: lm_abi::standard_bundle(),
            module_path: String::new(),
            imports: crate::import::ImportEnv::new(),
            core: None,
            core_intrinsics: std::sync::Arc::from([]),
            core_roots: BTreeSet::new(),
        }
    }
}

fn core_ast() -> &'static ast::Module {
    static CORE: OnceLock<ast::Module> = OnceLock::new();
    CORE.get_or_init(|| {
        let module = lm_source::parse::parse(CORE_SOURCE).expect("the core sources parse");
        assert!(module.entry.is_empty(), "the core has no entry expression");
        module
    })
}

fn empty_ast() -> &'static ast::Module {
    static EMPTY: OnceLock<ast::Module> = OnceLock::new();
    EMPTY.get_or_init(|| lm_source::parse::parse("").expect("the empty module parses"))
}

#[derive(Default)]
struct CoreDemand {
    names: HashSet<String>,
    methods: HashSet<String>,
    sys_groups: HashMap<String, String>,
    sys_members: HashMap<String, (String, String)>,
}

impl CoreDemand {
    fn for_module(module: &ast::Module, bundle: &lm_abi::AbiBundle) -> CoreDemand {
        let mut demand = CoreDemand::default();
        demand.name("PartialEq");
        demand.name("Hashable");
        for use_decl in &module.uses {
            if use_decl.path.first().map(String::as_str) != Some("sys") {
                continue;
            }
            let Some(surface_group) = use_decl.path.get(1) else {
                continue;
            };
            let Some(group) = bundle.surface_group(surface_group) else {
                continue;
            };
            match use_decl.path.get(2) {
                Some(member) => {
                    demand
                        .sys_members
                        .insert(member.clone(), (group.to_string(), member.clone()));
                    demand.add_sys_member(bundle, group, member);
                }
                None => {
                    demand
                        .sys_groups
                        .insert(surface_group.clone(), group.to_string());
                }
            }
        }
        for interface in &module.interfaces {
            demand.add_generics(&interface.generics);
            for parent in &interface.parents {
                demand.add_interface(parent);
            }
            for associated in &interface.associated {
                demand.add_associated(associated);
            }
            for method in &interface.methods {
                demand.add_generics(&method.generics);
                demand.add_params(&method.params);
                demand.add_optional_type(method.ret.as_ref());
                for premise in &method.premises {
                    demand.add_type(&premise.subject);
                    for bound in &premise.bounds {
                        demand.add_interface(bound);
                    }
                }
                if let Some(body) = &method.body {
                    demand.add_stmts(bundle, body);
                }
            }
        }
        for class in &module.classes {
            demand.add_generics(&class.generics);
            if let Some(parent) = &class.parent {
                demand.name(&parent.name);
                for argument in &parent.args {
                    demand.add_type(argument);
                }
            }
            for conformance in &class.interfaces {
                demand.add_conformance(conformance);
            }
            for associated in &class.associated {
                demand.add_associated(associated);
            }
            for field in &class.fields {
                demand.add_type(&field.ty);
                if let Some(default) = &field.default {
                    demand.add_expr(bundle, default);
                }
            }
            for method in &class.methods {
                demand.add_method(bundle, method);
            }
        }
        for enum_def in &module.enums {
            demand.add_generics(&enum_def.generics);
            for conformance in &enum_def.interfaces {
                demand.add_conformance(conformance);
            }
            for associated in &enum_def.associated {
                demand.add_associated(associated);
            }
            for arm in &enum_def.arms {
                for (_, ty) in &arm.fields {
                    demand.add_type(ty);
                }
            }
            for method in &enum_def.methods {
                demand.add_method(bundle, method);
            }
        }
        for function in &module.funcs {
            demand.add_generics(&function.generics);
            demand.add_params(&function.params);
            demand.add_optional_type(function.ret.as_ref());
            demand.add_stmts(bundle, &function.body);
        }
        demand.add_stmts(bundle, &module.entry);
        demand
    }

    fn name(&mut self, name: &str) {
        self.names.insert(name.to_string());
    }

    fn method(&mut self, name: &str) {
        self.methods.insert(name.to_string());
    }

    fn add_generics(&mut self, generics: &[ast::GenericParam]) {
        for generic in generics {
            for bound in &generic.bounds {
                self.add_interface(bound);
            }
        }
    }

    fn add_interface(&mut self, interface: &ast::InterfaceRef) {
        self.name(&interface.name);
        for argument in &interface.type_args {
            self.add_type(argument);
        }
    }

    fn add_conformance(&mut self, conformance: &ast::ConformanceRef) {
        self.add_interface(&conformance.application);
        self.add_generics(&conformance.premises);
    }

    fn add_associated(&mut self, associated: &ast::AssociatedType) {
        for bound in &associated.bounds {
            self.add_interface(bound);
        }
        self.add_optional_type(associated.value.as_ref());
    }

    fn add_optional_type(&mut self, ty: Option<&ast::TypeExpr>) {
        if let Some(ty) = ty {
            self.add_type(ty);
        }
    }

    fn add_type(&mut self, ty: &ast::TypeExpr) {
        match &ty.kind {
            ast::TypeExprKind::Name(name) => self.name(name),
            ast::TypeExprKind::Unit => {}
            ast::TypeExprKind::Apply(name, arguments) => {
                self.name(name);
                for argument in arguments {
                    self.add_type(argument);
                }
            }
            ast::TypeExprKind::ListShort(element) => {
                self.name("List");
                self.add_type(element);
            }
            ast::TypeExprKind::MapShort(key, value) => {
                self.name("Map");
                self.add_type(key);
                self.add_type(value);
            }
            ast::TypeExprKind::Tuple(elements) => {
                self.add_tuple(elements.len());
                for element in elements {
                    self.add_type(element);
                }
            }
            ast::TypeExprKind::Fn(params, _, ret, _) => {
                for param in params {
                    self.add_type(param);
                }
                self.add_type(ret);
            }
        }
    }

    fn add_tuple(&mut self, arity: usize) {
        if (2..=16).contains(&arity) {
            self.name(&format!("Tuple{arity}"));
        }
    }

    fn add_params(&mut self, params: &[ast::Param]) {
        for param in params {
            self.add_type(&param.ty);
        }
    }

    fn add_method(&mut self, bundle: &lm_abi::AbiBundle, method: &ast::MethodDef) {
        self.add_generics(&method.generics);
        self.add_params(&method.params);
        self.add_optional_type(method.ret.as_ref());
        self.add_generics(&method.premises);
        self.add_stmts(bundle, &method.body);
    }

    fn add_stmts(&mut self, bundle: &lm_abi::AbiBundle, statements: &[ast::Stmt]) {
        for statement in statements {
            match &statement.kind {
                ast::StmtKind::Assign { ty, value, .. } => {
                    self.add_optional_type(ty.as_ref());
                    self.add_expr(bundle, value);
                }
                ast::StmtKind::AssignField { recv, value, .. } => {
                    self.add_expr(bundle, recv);
                    self.add_expr(bundle, value);
                }
                ast::StmtKind::While { cond, body } => {
                    self.add_expr(bundle, cond);
                    self.add_stmts(bundle, body);
                }
                ast::StmtKind::For { value, body, .. } => {
                    self.name("Iterable");
                    self.name("Iterator");
                    self.name("Option");
                    self.name("Char");
                    self.add_expr(bundle, value);
                    self.add_stmts(bundle, body);
                }
                ast::StmtKind::Return { value } => {
                    if let Some(value) = value {
                        self.add_expr(bundle, value);
                    }
                }
                ast::StmtKind::Expr(expr) => self.add_expr(bundle, expr),
                ast::StmtKind::Break | ast::StmtKind::Continue => {}
            }
        }
    }

    fn add_expr(&mut self, bundle: &lm_abi::AbiBundle, expression: &ast::Expr) {
        use ast::ExprKind;
        match &expression.kind {
            ExprKind::Str(_) => self.name("String"),
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bytes(_)
            | ExprKind::Bool(_)
            | ExprKind::Unit
            | ExprKind::SelfRef => {}
            ExprKind::Name(name) => {
                self.name(name);
                if let Some((group, member)) = self.sys_members.get(name).cloned() {
                    self.add_sys_member(bundle, &group, &member);
                }
            }
            ExprKind::Interp(parts) => {
                for name in [
                    "Display",
                    "StringBuilder",
                    "Text",
                    "Char",
                    "Int",
                    "Float",
                    "Bool",
                    "String",
                    "Substring",
                    "Bytes",
                ] {
                    self.name(name);
                }
                for part in parts {
                    if let ast::InterpPart::Expr(expr) = part {
                        self.add_expr(bundle, expr);
                    }
                }
            }
            ExprKind::Not(value) => {
                self.name("Bool");
                self.method("__not__");
                self.add_expr(bundle, value);
            }
            ExprKind::Neg(value) => {
                self.method("__neg__");
                self.add_expr(bundle, value);
            }
            ExprKind::Invert(value) => {
                self.method("__invert__");
                self.add_expr(bundle, value);
            }
            ExprKind::Binary { op, left, right } => {
                let hook = match op {
                    ast::BinOp::Add => "__add__",
                    ast::BinOp::Sub => "__sub__",
                    ast::BinOp::Mul => "__mul__",
                    ast::BinOp::Div => "__div__",
                    ast::BinOp::Rem => "__rem__",
                    ast::BinOp::Eq => "__eq__",
                    ast::BinOp::Ne => "__ne__",
                    ast::BinOp::Lt => "__lt__",
                    ast::BinOp::Le => "__le__",
                    ast::BinOp::Gt => "__gt__",
                    ast::BinOp::Ge => "__ge__",
                    ast::BinOp::BitAnd => "__and__",
                    ast::BinOp::BitOr => "__or__",
                    ast::BinOp::BitXor => "__xor__",
                    ast::BinOp::Shl => "__shl__",
                    ast::BinOp::Shr => "__shr__",
                    ast::BinOp::Ushr => "__ushr__",
                };
                self.method(hook);
                self.add_expr(bundle, left);
                self.add_expr(bundle, right);
            }
            ExprKind::And(left, right) | ExprKind::Or(left, right) => {
                self.add_expr(bundle, left);
                self.add_expr(bundle, right);
            }
            ExprKind::Is { value, ty } | ExprKind::Cast { value, ty } => {
                self.add_expr(bundle, value);
                self.add_type(ty);
            }
            ExprKind::Call {
                name,
                type_args,
                args,
                ..
            } => {
                self.name(name);
                if name == "codeof" {
                    self.name("FunctionCode");
                    self.name("ClassCode");
                }
                if let Some((group, member)) = self.sys_members.get(name).cloned() {
                    self.add_sys_member(bundle, &group, &member);
                }
                for ty in type_args {
                    self.add_type(ty);
                }
                for argument in args {
                    self.add_expr(bundle, argument);
                }
            }
            ExprKind::CallExpr { callee, args } => {
                self.add_expr(bundle, callee);
                for argument in args {
                    self.add_expr(bundle, argument);
                }
            }
            ExprKind::Field { recv, name, .. } => {
                if let Some(group) = self.sys_group(recv) {
                    self.add_sys_member(bundle, &group, name);
                }
                self.add_expr(bundle, recv);
            }
            ExprKind::MethodCall {
                recv,
                name,
                type_args,
                args,
                ..
            } => {
                self.add_native_method_surface(name);
                if name == "spawn" {
                    self.name("Proc");
                }
                if let Some(group) = self.sys_group(recv) {
                    self.add_sys_member(bundle, &group, name);
                } else {
                    self.method(name);
                }
                for ty in type_args {
                    self.add_type(ty);
                }
                self.add_expr(bundle, recv);
                for argument in args {
                    self.add_expr(bundle, argument);
                }
            }
            ExprKind::SuperCall { name, args, .. } => {
                self.method(name);
                for argument in args {
                    self.add_expr(bundle, argument);
                }
            }
            ExprKind::Index { recv, index } => {
                self.add_expr(bundle, recv);
                self.add_expr(bundle, index);
            }
            ExprKind::Propagate(value) => {
                self.name("Result");
                self.add_expr(bundle, value);
            }
            ExprKind::TupleLit(elements) => {
                self.add_tuple(elements.len());
                for element in elements {
                    self.add_expr(bundle, element);
                }
            }
            ExprKind::ListLit(elements) => {
                self.name("List");
                for element in elements {
                    self.add_expr(bundle, element);
                }
            }
            ExprKind::MapLit(entries) => {
                self.name("Map");
                for (key, value) in entries {
                    self.add_expr(bundle, key);
                    self.add_expr(bundle, value);
                }
            }
            ExprKind::Closure {
                params, ret, body, ..
            } => {
                self.add_params(params);
                self.add_optional_type(ret.as_ref());
                self.add_stmts(bundle, body);
            }
            ExprKind::If { arms, else_body } => {
                for (condition, body) in arms {
                    self.add_expr(bundle, condition);
                    self.add_stmts(bundle, body);
                }
                if let Some(body) = else_body {
                    self.add_stmts(bundle, body);
                }
            }
            ExprKind::Case { scrut, arms } => {
                self.add_expr(bundle, scrut);
                for arm in arms {
                    self.add_pattern(bundle, &arm.pattern);
                    self.add_stmts(bundle, &arm.body);
                }
            }
            ExprKind::Select { arms } => {
                self.name("Choice");
                for arm in arms {
                    self.add_expr(bundle, &arm.wait);
                    self.add_stmts(bundle, &arm.body);
                }
            }
            ExprKind::Labeled { value, .. } => self.add_expr(bundle, value),
        }
    }

    fn add_pattern(&mut self, bundle: &lm_abi::AbiBundle, pattern: &ast::Pattern) {
        match &pattern.kind {
            ast::PatternKind::Name(name) => self.name(name),
            ast::PatternKind::Ctor {
                qualifier,
                name,
                args,
                ..
            } => {
                if let Some(qualifier) = qualifier {
                    self.name(qualifier);
                    if let Some(group) = bundle
                        .group_by_name(qualifier)
                        .and_then(|slot| bundle.group_name(slot))
                    {
                        self.add_sys_member(bundle, group, name);
                    }
                }
                self.name(name);
                if name == "Call" {
                    self.name("Option");
                }
                for argument in args {
                    self.add_pattern(bundle, argument);
                }
            }
            ast::PatternKind::Tuple(elements) => {
                self.add_tuple(elements.len());
                for element in elements {
                    self.add_pattern(bundle, element);
                }
            }
            ast::PatternKind::Wildcard
            | ast::PatternKind::Int(_)
            | ast::PatternKind::Bool(_)
            | ast::PatternKind::Str(_) => {}
        }
    }

    fn sys_group(&self, expression: &ast::Expr) -> Option<String> {
        match &expression.kind {
            ast::ExprKind::Field { recv, name, .. } if matches!(recv.kind, ast::ExprKind::Name(ref root) if root == "sys") => {
                self.sys_groups
                    .get(name)
                    .cloned()
                    .or_else(|| Some(camel_member(name)))
            }
            ast::ExprKind::Name(name) => self.sys_groups.get(name).cloned(),
            _ => None,
        }
    }

    fn add_sys_member(&mut self, bundle: &lm_abi::AbiBundle, group: &str, member: &str) {
        match (group, member) {
            ("Vm", "Vm") => self.add_vm_surface(),
            ("Vm", "artifact") => self.name("Artifact"),
            ("Vm", "snapshot_self" | "load_snapshot") => {
                self.name("Result");
                self.name("SnapshotError");
            }
            ("Vm", "restore_vm") => {
                self.name("Result");
                self.name("RestoreError");
            }
            ("Proc", "recv" | "recv_wait") => {
                self.name("Proc");
                self.name("Recv");
            }
            ("Proc", "run") => self.add_proc_surface(),
            _ => {}
        }
        let fixed = if member
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_uppercase())
        {
            member.to_string()
        } else {
            camel_member(member)
        };
        let Some(operation) = bundle.fixed_member(group, &fixed) else {
            return;
        };
        let Some(operation) = bundle.op(operation) else {
            return;
        };
        for ty in operation.params.iter().chain([&operation.reply]) {
            self.add_abi_type(*ty);
        }
    }

    fn add_abi_type(&mut self, ty: lm_abi::AbiType) {
        match ty {
            lm_abi::AbiType::Core(core) => self.name(core.text()),
            lm_abi::AbiType::Native(native) => self.name(native.text()),
            lm_abi::AbiType::List(element) => {
                self.name("List");
                self.add_abi_type(*element);
            }
            lm_abi::AbiType::Map(key, value) => {
                self.name("Map");
                self.add_abi_type(*key);
                self.add_abi_type(*value);
            }
            lm_abi::AbiType::Tuple(elements) => {
                self.add_tuple(elements.len());
                for element in elements {
                    self.add_abi_type(*element);
                }
            }
            lm_abi::AbiType::Apply(constructor, arguments) => {
                self.name(constructor.text());
                for argument in arguments {
                    self.add_abi_type(*argument);
                }
            }
            lm_abi::AbiType::Primitive(_)
            | lm_abi::AbiType::Var(_)
            | lm_abi::AbiType::Resource(_) => {}
        }
    }

    fn add_vm_surface(&mut self) {
        for name in [
            "Artifact",
            "VerifiedModule",
            "FunctionCode",
            "ClassCode",
            "SlotSpec",
            "Instance",
            "Slot",
            "FunctionDef",
            "ClassDef",
            "FunctionBinding",
            "ClassBinding",
            "DynValue",
            "CodeError",
            "DefinitionSource",
            "DefinitionSpec",
            "LinkEnv",
            "SlotChange",
            "SnapshotError",
            "RestoreError",
            "BranchError",
            "StepEvent",
            "DriveEvent",
            "Choice",
            "SendResult",
            "ProcError",
            "CodeLocation",
            "Result",
            "Option",
            "Recv",
            "TcpResource",
            "TlsStream",
            "SocketAddress",
        ] {
            self.name(name);
        }
    }

    fn add_proc_surface(&mut self) {
        for name in [
            "Recv",
            "SendResult",
            "ProcError",
            "SnapshotError",
            "Result",
            "Option",
            "DriveEvent",
        ] {
            self.name(name);
        }
    }

    fn add_names(&mut self, names: &[&str]) {
        for name in names {
            self.name(name);
        }
    }

    /// Add the types that a native method result can name.
    fn add_native_method_surface(&mut self, method: &str) {
        match method {
            "from_hex" => self.add_names(&["Bytes", "_bytes_from_hex"]),
            "value" => self.add_names(&["Result", "_result_fault_value"]),
            "source" => self.add_names(&["CodeError", "Result", "DefinitionSource", "Option"]),
            "definition" => self.add_names(&["CodeError", "Result", "DefinitionSpec"]),
            "verify" => self.add_names(&["CodeError", "Result", "VerifiedModule"]),
            "entry_code" | "function_code" => {
                self.add_names(&["CodeError", "Result", "FunctionCode"])
            }
            "class_code" => self.add_names(&["CodeError", "Result", "ClassCode"]),
            "dynamic_entry" => self.add_names(&["CodeError", "Result", "DynValue", "FunctionDef"]),
            "entry" | "function" | "entry_binding" | "function_binding" => {
                self.add_names(&["CodeError", "Result", "FunctionDef", "FunctionBinding"])
            }
            "class_def" | "class_binding" => {
                self.add_names(&["CodeError", "Result", "ClassDef", "ClassBinding"])
            }
            "slot_for" | "slot_spec" | "slot" | "spec" | "instance" | "target" => {
                self.add_names(&[
                    "CodeError",
                    "Result",
                    "Slot",
                    "SlotSpec",
                    "Instance",
                    "FunctionDef",
                    "ClassDef",
                ])
            }
            "to_bytes" | "snapshot" => self.add_names(&["SnapshotError", "Result"]),
            "activate" => {
                self.add_names(&["CodeError", "Result", "FunctionDef", "FunctionBinding"])
            }
            "install" => self.add_names(&[
                "CodeError",
                "Result",
                "VerifiedModule",
                "Instance",
                "FunctionCode",
                "FunctionDef",
                "ClassCode",
                "ClassDef",
                "LinkEnv",
            ]),
            "replace" | "replace_function" | "replace_class" | "replace_value"
            | "replace_process" | "change" | "change_function" | "change_class"
            | "change_value" | "change_process" | "replace_all" => self.add_names(&[
                "CodeError",
                "Result",
                "Slot",
                "SlotChange",
                "FunctionDef",
                "FunctionBinding",
                "ClassDef",
                "ClassBinding",
            ]),
            "snapshot_wait" => self.add_names(&["SnapshotError", "ProcError", "Result"]),
            "drive_for" => self.add_names(&["DriveEvent", "Option"]),
            "run" | "done" => self.name("Result"),
            "step" => self.name("StepEvent"),
            "drive" | "drive_wait" => self.name("DriveEvent"),
            "branch" | "branch_answer" => self.add_names(&["BranchError", "Result"]),
            "restore" => self.add_names(&["RestoreError", "Result"]),
            "resource" => self.add_names(&["TcpResource", "TlsStream"]),
            "serve_tcp_stream" => self.name("SocketAddress"),
            "choose" => self.name("Choice"),
            "send" | "close" => self.name("SendResult"),
            "pause" | "resume" => self.add_names(&["ProcError", "Result"]),
            "site" => self.add_names(&["CodeLocation", "Option"]),
            "trace" => self.name("CodeLocation"),
            _ => {}
        }
    }
}

/// One callable signature. Methods include `self` as parameter zero.
/// For a method, the type parameters hold the class parameters first
/// and the method's own parameters after them.
#[derive(Clone)]
pub(crate) struct FnSig {
    pub(crate) type_params: Vec<String>,
    pub(crate) type_bounds: Vec<Vec<InterfaceUse>>,
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
    pub(crate) class_type_bounds: Vec<Vec<InterfaceUse>>,
    pub(crate) own_type_params: Vec<String>,
    pub(crate) own_type_bounds: Vec<Vec<InterfaceUse>>,
    pub(crate) own_effect_params: Vec<String>,
}

/// One resolved application of a nominal interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InterfaceUse {
    pub(crate) interface: u32,
    pub(crate) type_args: Vec<TypeId>,
    pub(crate) row_args: Vec<Row>,
}

/// One associated type of a nominal interface.
#[derive(Clone)]
pub(crate) struct AssociatedInfo {
    pub(crate) name: String,
    pub(crate) bounds: Vec<InterfaceUse>,
}

/// One method requirement of a nominal interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InterfaceMethodSig {
    pub(crate) name: String,
    pub(crate) mut_self: bool,
    pub(crate) own_type_params: Vec<String>,
    pub(crate) own_type_bounds: Vec<Vec<InterfaceUse>>,
    pub(crate) own_effect_params: Vec<String>,
    pub(crate) premises: Vec<TypePremise>,
    pub(crate) params: Vec<TypeId>,
    pub(crate) param_muts: Vec<bool>,
    pub(crate) param_names: Vec<String>,
    pub(crate) ret: TypeId,
    pub(crate) row: Row,
    /// The verified default function, when this module can call it.
    pub(crate) default_func: Option<u32>,
    /// The stable hidden binding of the default function.
    pub(crate) default_binding: Option<String>,
}

/// One resolved method from an interface bound or default.
pub(crate) type ResolvedInterfaceMethod = (InterfaceUse, u32, u32, Rc<InterfaceMethodSig>);

/// One checked type premise on an interface method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TypePremise {
    pub(crate) subject: TypeId,
    pub(crate) bounds: Vec<InterfaceUse>,
}

/// One declared nominal interface.
#[derive(Clone)]
pub(crate) struct InterfaceInfo {
    pub(crate) origin: Option<(String, String)>,
    pub(crate) name: String,
    pub(crate) type_params: Vec<String>,
    pub(crate) effect_params: Vec<String>,
    pub(crate) generic_is_effect: Vec<bool>,
    pub(crate) parents: Vec<InterfaceUse>,
    pub(crate) type_bounds: Vec<Vec<InterfaceUse>>,
    pub(crate) associated: Vec<AssociatedInfo>,
    pub(crate) methods: Vec<Rc<InterfaceMethodSig>>,
    pub(crate) method_index: Vec<usize>,
}

impl InterfaceInfo {
    /// Find one method by its surface name.
    pub(crate) fn find_method(&self, name: &str) -> Option<usize> {
        if self.method_index.is_empty() {
            self.methods.iter().position(|method| method.name == name)
        } else {
            self.method_index
                .binary_search_by(|index| self.methods[*index].name.as_str().cmp(name))
                .ok()
                .map(|position| self.method_index[position])
        }
    }
}

pub(crate) fn index_interface_methods(methods: &[Rc<InterfaceMethodSig>]) -> Vec<usize> {
    if methods.len() < 8 {
        return Vec::new();
    }
    let mut index: Vec<usize> = (0..methods.len()).collect();
    index.sort_unstable_by(|left, right| methods[*left].name.cmp(&methods[*right].name));
    index
}

/// One explicit class conformance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConformancePremise {
    pub(crate) param: u32,
    pub(crate) bounds: Vec<InterfaceUse>,
}

/// One explicit class conformance.
#[derive(Clone)]
pub(crate) struct ConformanceInfo {
    pub(crate) application: InterfaceUse,
    pub(crate) premises: Vec<ConformancePremise>,
    pub(crate) associated: Vec<TypeId>,
    /// One entry per interface method.
    /// True selects compatible class dispatch. False selects the default.
    pub(crate) method_overrides: Vec<bool>,
}

fn hir_interface_use(application: &InterfaceUse) -> HirInterfaceUse {
    HirInterfaceUse {
        interface: application.interface,
        types: application.type_args.clone(),
        rows: application.row_args.clone(),
    }
}

fn into_hir_interface_use(application: InterfaceUse) -> HirInterfaceUse {
    HirInterfaceUse {
        interface: application.interface,
        types: application.type_args,
        rows: application.row_args,
    }
}

pub(crate) fn hir_bounds(bounds: &[Vec<InterfaceUse>]) -> Vec<Vec<HirInterfaceUse>> {
    bounds
        .iter()
        .map(|items| items.iter().map(hir_interface_use).collect())
        .collect()
}

fn into_hir_bounds(bounds: Vec<Vec<InterfaceUse>>) -> Vec<Vec<HirInterfaceUse>> {
    bounds
        .into_iter()
        .map(|items| items.into_iter().map(into_hir_interface_use).collect())
        .collect()
}

/// Checker-side class information with the full field layout.
pub(crate) struct ClassInfo {
    /// True for an imported declaration: a shape with no body.
    pub(crate) imported: bool,
    pub(crate) source_span: Option<Span>,
    pub(crate) is_final: bool,
    pub(crate) is_frozen: bool,
    pub(crate) native_repr: Option<NativeRepr>,
    pub(crate) name: String,
    pub(crate) parent: Option<u32>,
    pub(crate) type_params: Vec<String>,
    pub(crate) type_bounds: Vec<Vec<InterfaceUse>>,
    pub(crate) conformances: Vec<Rc<ConformanceInfo>>,
    pub(crate) kind: ClassKind,
    /// The instance type seen by method bodies: `Class(c)` or
    /// `Inst(c, [Var 0..])`.
    pub(crate) self_ty: TypeId,
    pub(crate) field_names: Vec<String>,
    pub(crate) field_tys: Vec<TypeId>,
    pub(crate) has_default: Vec<bool>,
    /// The layout index where own fields start.
    pub(crate) own_start: usize,
    pub(crate) methods: Vec<Rc<MethodSig>>,
    pub(crate) method_index: Vec<usize>,
    pub(crate) init: Option<Rc<MethodSig>>,
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
            source_span: None,
            is_final: false,
            is_frozen: false,
            native_repr: None,
            name: String::new(),
            parent: None,
            type_params: Vec::new(),
            type_bounds: Vec::new(),
            conformances: Vec::new(),
            kind: ClassKind::Normal,
            self_ty: UNIT,
            field_names: Vec::new(),
            field_tys: Vec::new(),
            has_default: Vec::new(),
            own_start: 0,
            methods: Vec::new(),
            method_index: Vec::new(),
            init: None,
            family: None,
            arms: Vec::new(),
            arm_short: String::new(),
        }
    }
}

pub(crate) fn index_methods(methods: &[Rc<MethodSig>]) -> Vec<usize> {
    if methods.len() < 8 {
        return Vec::new();
    }
    let mut index: Vec<usize> = (0..methods.len()).collect();
    index.sort_unstable_by(|left, right| methods[*left].name.cmp(&methods[*right].name));
    index
}

/// One resolved `use` binding.
#[derive(Clone)]
pub(crate) enum UseBinding {
    /// A `sys` group object, by manifest group name (`Io`).
    SysGroup(String),
    /// A callable `sys` member: the manifest group name plus the
    /// surface member name (`print`, `read_line`, or `Vm`).
    SysMember { group: String, member: String },
    /// A whole module bound to a short name. A member of it resolves
    /// through the qualified key `alias.member`.
    Module(String),
}

/// Map a surface `sys` member name to its manifest group name.
pub(crate) fn sys_group_name(ctx: &Ctx, name: &str) -> Option<String> {
    ctx.bundle.surface_group(name).map(str::to_string)
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
    let Some(prefix) = env.roots.get(root) else {
        let mut known: Vec<&str> = env.roots.keys().map(|k| k.as_str()).collect();
        known.push("sys");
        return Err(Diagnostic::new(
            "E1052",
            if env.has_no_package_roots() {
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
            } else if export.kind.is_interface() {
                let id = mat.reserve_interface(ctx, &full, &export.name, decl.span)?;
                bind_interface(ctx, &bound_name, id, decl.name_span)?;
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
            } else if export.kind.is_interface() {
                let id = mat.reserve_interface(ctx, &module, &bound, decl.span)?;
                bind_interface(ctx, &bound, id, decl.name_span)?;
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

/// Bind one imported interface name in the module scope.
fn bind_interface(ctx: &mut Ctx, name: &str, interface: u32, span: Span) -> Result<(), Diagnostic> {
    if ctx
        .user_interfaces
        .insert(name.to_string(), interface)
        .is_some()
    {
        return Err(Diagnostic::new(
            "E1052",
            format!("the interface name `{name}` already has a definition"),
            span,
        ));
    }
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
                     for example `use sys.io` or `use sys.io.write`",
                    decl.span,
                ));
            }
            2 => {
                let Some(group) = sys_group_name(ctx, &decl.path[1]) else {
                    return Err(Diagnostic::new(
                        "E1052",
                        format!("`sys` has no group named `{}`", decl.path[1]),
                        decl.name_span,
                    ));
                };
                UseBinding::SysGroup(group)
            }
            3 => {
                let Some(group) = sys_group_name(ctx, &decl.path[1]) else {
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
                    if ctx.bundle.fixed_member(&group, &member).is_some() {
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
                if !is_ctor
                    && ctx
                        .bundle
                        .fixed_member(&group, &camel_member(&member))
                        .is_none()
                {
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
    pub(crate) type_bounds: Vec<Vec<InterfaceUse>>,
    /// Bounds on types that are not direct type parameters.
    pub(crate) extra_bounds: Vec<TypePremise>,
    pub(crate) effect_names: Vec<String>,
    /// The first type-variable index used by `type_names`.
    pub(crate) type_offset: u32,
    /// The interface that gives meaning to `Self.Item`.
    pub(crate) self_interface: Option<u32>,
    /// The type that bare `Self` names in this declaration.
    pub(crate) self_ty: Option<TypeId>,
    /// True while checking core sources: names resolve against the
    /// core definitions only.
    pub(crate) core_scope: bool,
}

/// Shared module state for all function checkers.
pub(crate) struct Ctx {
    pub(crate) bundle: std::sync::Arc<lm_abi::AbiBundle>,
    pub(crate) store: TypeStore,
    pub(crate) classes: Vec<ClassInfo>,
    pub(crate) user_types: HashMap<String, u32>,
    pub(crate) core_types: HashMap<String, u32>,
    pub(crate) interfaces: Vec<InterfaceInfo>,
    pub(crate) user_interfaces: HashMap<String, u32>,
    pub(crate) core_interfaces: HashMap<String, u32>,
    /// Default methods grouped by their surface name.
    interface_defaults: HashMap<String, Rc<[(u32, u32)]>>,
    pub(crate) prelude: bool,
    pub(crate) func_index: HashMap<String, u32>,
    pub(crate) core_func_index: HashMap<String, u32>,
    pub(crate) sigs: Vec<FnSig>,
    pub(crate) funcs: Vec<Option<HirFunc>>,
    /// Local functions that one expression installs or reifies.
    pub(crate) reified_functions: BTreeSet<u32>,
    /// Local classes that one expression reifies.
    pub(crate) reified_classes: BTreeSet<u32>,
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
    pub(crate) user_interface_start: u32,
    pub(crate) import_interface_start: u32,
    /// Map-key proofs that wait for the complete class table.
    deferred_map_keys: Vec<(TyEnv, TypeId, Span)>,
    /// True while class conformances can still be unresolved.
    defer_map_keys: bool,
}

impl Ctx {
    /// Test whether every value of one type is deeply frozen.
    pub(crate) fn type_always_frozen(&self, ty: TypeId, allow_var: bool) -> bool {
        self.type_always_frozen_inner(ty, allow_var, 0)
    }

    fn type_always_frozen_inner(&self, ty: TypeId, allow_var: bool, depth: usize) -> bool {
        if depth >= 128 {
            return false;
        }
        match self.store.get(ty) {
            Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::String
            | Type::Never
            | Type::Bytes
            | Type::Digest
            | Type::Fault
            | Type::Op(_, _) => true,
            Type::Var(_) => allow_var,
            Type::Tuple(items) => items
                .iter()
                .all(|item| self.type_always_frozen_inner(*item, allow_var, depth + 1)),
            Type::Class(class) => self.class_always_frozen(class.0),
            Type::Inst(class, args) => {
                self.class_always_frozen(class.0)
                    && args
                        .iter()
                        .all(|arg| self.type_always_frozen_inner(*arg, allow_var, depth + 1))
            }
            _ => false,
        }
    }

    fn class_always_frozen(&self, class: u32) -> bool {
        let info = &self.classes[class as usize];
        info.is_frozen
            || matches!(
                info.native_repr,
                Some(
                    NativeRepr::Unit
                        | NativeRepr::Int
                        | NativeRepr::Float
                        | NativeRepr::Bool
                        | NativeRepr::Text
                        | NativeRepr::String
                        | NativeRepr::Substring
                        | NativeRepr::Char
                        | NativeRepr::Bytes
                )
            )
    }

    /// Return the stable key of one class.
    pub(crate) fn class_key(&self, class: u32, module_path: &str) -> String {
        let info = &self.classes[class as usize];
        if class < self.user_start {
            return lm_bytecode::qualified_key(lm_bytecode::CORE_MODULE, &info.name);
        }
        if class < self.import_start {
            return lm_bytecode::qualified_key(module_path, &info.name);
        }
        for import in &self.imports {
            if import.kind == lm_bytecode::ImportKind::Class {
                if let HirImportDef::Class(c) = import.def {
                    if c == class {
                        return lm_bytecode::qualified_key(&import.module, &import.name);
                    }
                }
            }
        }
        lm_bytecode::qualified_key(module_path, &info.name)
    }

    /// Render one type with names from its lexical scope.
    pub(crate) fn display_type(&self, env: &TyEnv, ty: TypeId) -> String {
        self.store.display_with_names(
            ty,
            &|index| {
                if index == 0 && env.self_interface.is_some() {
                    return Some("Self".to_string());
                }
                let position = index.checked_sub(env.type_offset)? as usize;
                env.type_names.get(position).cloned()
            },
            &|index| env.effect_names.get(index as usize).cloned(),
            &|interface, assoc| {
                self.interfaces
                    .get(interface.0 as usize)?
                    .associated
                    .get(assoc as usize)
                    .map(|item| item.name.clone())
            },
        )
    }

    /// Render one effect row with names from its lexical scope.
    pub(crate) fn display_row(&self, env: &TyEnv, row: &Row) -> String {
        self.store
            .display_row_with_names(row, &|index| env.effect_names.get(index as usize).cloned())
    }

    /// Render one interface application with source generic names.
    pub(crate) fn display_interface_use(&self, env: &TyEnv, application: &InterfaceUse) -> String {
        let mut text = self.interfaces[application.interface as usize].name.clone();
        if !application.type_args.is_empty() {
            let arguments: Vec<String> = application
                .type_args
                .iter()
                .map(|argument| self.display_type(env, *argument))
                .collect();
            text.push('[');
            text.push_str(&arguments.join(", "));
            text.push(']');
        }
        if application.row_args.iter().any(|row| !row.is_empty()) {
            for row in &application.row_args {
                text.push_str(" with ");
                if row.is_empty() {
                    text.push_str("()");
                    continue;
                }
                let row = self.display_row(env, row);
                if row.contains(',') {
                    text.push('(');
                    text.push_str(&row);
                    text.push(')');
                } else {
                    text.push_str(&row);
                }
            }
        }
        text
    }

    /// Look up a class or enum type name in the given scope.
    pub(crate) fn lookup_type(&self, name: &str, env: &TyEnv) -> Option<u32> {
        if env.core_scope {
            return self.core_types.get(name).copied();
        }
        if let Some(idx) = self.user_types.get(name) {
            return Some(*idx);
        }
        if self.prelude && is_prelude_type(name) {
            return self.core_types.get(name).copied();
        }
        None
    }

    /// Look up an interface name in the given scope.
    pub(crate) fn lookup_interface(&self, name: &str, env: &TyEnv) -> Option<u32> {
        if env.core_scope {
            return self.core_interfaces.get(name).copied();
        }
        if let Some(idx) = self.user_interfaces.get(name) {
            return Some(*idx);
        }
        if self.prelude {
            return self.core_interfaces.get(name).copied();
        }
        None
    }

    /// Find one associated type through the bounds on a type variable.
    fn projection(
        &mut self,
        env: &TyEnv,
        base: TypeId,
        variable: usize,
        name: &str,
        span: Span,
    ) -> Result<TypeId, Diagnostic> {
        let Some(bounds) = env.type_bounds.get(variable) else {
            return Err(Diagnostic::new(
                "E1053",
                format!("the type parameter has no associated type named `{name}`"),
                span,
            ));
        };
        let mut found = Vec::new();
        for application in bounds {
            let interface = &self.interfaces[application.interface as usize];
            if let Some(index) = interface
                .associated
                .iter()
                .position(|item| item.name == name)
            {
                found.push((application.interface, index as u32));
            }
        }
        match found.as_slice() {
            [(interface, assoc)] => Ok(self.store.project(base, InterfaceId(*interface), *assoc)),
            [] => Err(Diagnostic::new(
                "E1053",
                format!("the type parameter has no associated type named `{name}`"),
                span,
            )),
            _ => Err(Diagnostic::new(
                "E1053",
                format!("the associated type name `{name}` is ambiguous"),
                span,
            )),
        }
    }

    /// Find one associated type through an interface inheritance graph.
    fn interface_associated(&self, interface: u32, name: &str) -> Vec<(u32, u32)> {
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![interface];
        while let Some(current) = stack.pop() {
            if !seen.insert(current) {
                continue;
            }
            let contract = &self.interfaces[current as usize];
            if let Some(index) = contract
                .associated
                .iter()
                .position(|item| item.name == name)
            {
                found.push((current, index as u32));
            }
            stack.extend(contract.parents.iter().map(|parent| parent.interface));
        }
        found
    }

    /// Substitute all arguments of one interface application.
    pub(crate) fn substitute_interface_use(
        &mut self,
        application: &InterfaceUse,
        types: &[TypeId],
        rows: &[Row],
    ) -> InterfaceUse {
        InterfaceUse {
            interface: application.interface,
            type_args: application
                .type_args
                .iter()
                .map(|item| self.store.substitute(*item, types, rows))
                .collect(),
            row_args: application
                .row_args
                .iter()
                .map(|item| self.store.substitute_row(item, rows))
                .collect(),
        }
    }

    pub(crate) fn instantiate_interface_method(
        &mut self,
        receiver: TypeId,
        application: &InterfaceUse,
        method: &Rc<InterfaceMethodSig>,
    ) -> Rc<InterfaceMethodSig> {
        let types_are_closed = method
            .params
            .iter()
            .chain(std::iter::once(&method.ret))
            .all(|ty| !self.store.contains_var(*ty) && !self.store.contains_effect_var(*ty));
        let row_is_closed = !method
            .row
            .iter()
            .any(|item| matches!(item, RowElem::Var(_)));
        if types_are_closed && row_is_closed {
            return Rc::clone(method);
        }
        let mut types = vec![receiver];
        types.extend(application.type_args.iter().copied());
        Rc::new(InterfaceMethodSig {
            name: method.name.clone(),
            mut_self: method.mut_self,
            own_type_params: method.own_type_params.clone(),
            own_type_bounds: method
                .own_type_bounds
                .iter()
                .map(|bounds| {
                    bounds
                        .iter()
                        .map(|bound| {
                            self.substitute_interface_use(bound, &types, &application.row_args)
                        })
                        .collect()
                })
                .collect(),
            own_effect_params: method.own_effect_params.clone(),
            premises: method
                .premises
                .iter()
                .map(|premise| TypePremise {
                    subject: self
                        .store
                        .substitute(premise.subject, &types, &application.row_args),
                    bounds: premise
                        .bounds
                        .iter()
                        .map(|bound| {
                            self.substitute_interface_use(bound, &types, &application.row_args)
                        })
                        .collect(),
                })
                .collect(),
            params: method
                .params
                .iter()
                .map(|item| self.store.substitute(*item, &types, &application.row_args))
                .collect(),
            param_muts: method.param_muts.clone(),
            param_names: method.param_names.clone(),
            ret: self
                .store
                .substitute(method.ret, &types, &application.row_args),
            row: self
                .store
                .substitute_row(&method.row, &application.row_args),
            default_func: method.default_func,
            default_binding: method.default_binding.clone(),
        })
    }

    /// Find a satisfied conformance and its declaring type arguments.
    fn conformance_entry(
        &mut self,
        env: &TyEnv,
        ty: TypeId,
        interface: u32,
        depth: u32,
    ) -> Option<(Rc<ConformanceInfo>, Vec<TypeId>)> {
        if depth >= 128 {
            return None;
        }
        let (mut class, mut args) = self.store.nominal_class(ty)?;
        loop {
            let conformance = self
                .classes
                .get(class.0 as usize)?
                .conformances
                .iter()
                .find(|item| item.application.interface == interface)
                .cloned();
            if let Some(conformance) = conformance {
                let mut satisfied = true;
                for premise in &conformance.premises {
                    let actual = args.get(premise.param as usize).copied()?;
                    for bound in &premise.bounds {
                        let required = self.substitute_interface_use(bound, &args, &[]);
                        if !self.type_conforms_depth(env, actual, &required, depth + 1) {
                            satisfied = false;
                            break;
                        }
                    }
                    if !satisfied {
                        break;
                    }
                }
                if satisfied {
                    return Some((conformance, args));
                }
            }
            let meta = self.store.class_meta(class).clone();
            let parent = meta.parent?;
            if meta.kind != ClassKind::EnumCase || !meta.parent_args.is_empty() {
                args = meta
                    .parent_args
                    .iter()
                    .map(|item| self.store.substitute(*item, &args, &[]))
                    .collect();
            }
            class = parent;
        }
    }

    /// Find a concrete or inherited conformance for one nominal type.
    fn conformance_use(
        &mut self,
        env: &TyEnv,
        ty: TypeId,
        interface: u32,
        depth: u32,
    ) -> Option<InterfaceUse> {
        let (conformance, args) = self.conformance_entry(env, ty, interface, depth)?;
        Some(self.substitute_interface_use(&conformance.application, &args, &[]))
    }

    /// Resolve one associated type through a satisfied conformance.
    pub(crate) fn conformance_associated(
        &mut self,
        env: &TyEnv,
        ty: TypeId,
        interface: u32,
        assoc: u32,
    ) -> Option<TypeId> {
        let (conformance, args) = self.conformance_entry(env, ty, interface, 0)?;
        conformance
            .associated
            .get(assoc as usize)
            .map(|item| self.store.substitute(*item, &args, &[]))
    }

    /// Find one interface application for a type in this scope.
    pub(crate) fn type_conformance(
        &mut self,
        env: &TyEnv,
        ty: TypeId,
        interface: u32,
    ) -> Option<InterfaceUse> {
        self.type_conformance_depth(env, ty, interface, 0)
    }

    /// Find one interface application with bounded premise recursion.
    fn type_conformance_depth(
        &mut self,
        env: &TyEnv,
        ty: TypeId,
        interface: u32,
        depth: u32,
    ) -> Option<InterfaceUse> {
        if depth >= 128 {
            return None;
        }
        if let Some(bound) = env
            .extra_bounds
            .iter()
            .find(|premise| premise.subject == ty)
            .and_then(|premise| {
                premise
                    .bounds
                    .iter()
                    .find(|bound| bound.interface == interface)
            })
        {
            return Some(bound.clone());
        }
        match self.store.get(ty).clone() {
            Type::Var(index) if index >= env.type_offset => env
                .type_bounds
                .get((index - env.type_offset) as usize)
                .and_then(|bounds| {
                    bounds
                        .iter()
                        .find(|item| item.interface == interface)
                        .cloned()
                }),
            Type::Projection {
                interface: owner,
                assoc,
                ..
            } => self.interfaces[owner.0 as usize]
                .associated
                .get(assoc as usize)
                .and_then(|item| {
                    item.bounds
                        .iter()
                        .find(|bound| bound.interface == interface)
                })
                .cloned(),
            _ => self.conformance_use(env, ty, interface, depth),
        }
    }

    /// Test one type against one required interface application.
    pub(crate) fn type_conforms(
        &mut self,
        env: &TyEnv,
        ty: TypeId,
        required: &InterfaceUse,
    ) -> bool {
        self.type_conforms_depth(env, ty, required, 0)
    }

    /// Resolve associated projections that have concrete conformances.
    pub(crate) fn normalize_associated(&mut self, env: &TyEnv, ty: TypeId) -> TypeId {
        if !self.store.has_projection(ty) {
            return ty;
        }
        self.normalize_associated_depth(env, ty, 0)
    }

    fn normalize_associated_depth(&mut self, env: &TyEnv, ty: TypeId, depth: u32) -> TypeId {
        if depth >= 128 {
            return ty;
        }
        match self.store.get(ty).clone() {
            Type::Projection {
                base,
                interface,
                assoc,
            } => {
                let base = self.normalize_associated_depth(env, base, depth + 1);
                if let Some(alias) = self.iterable_item_alias(base, interface, assoc) {
                    return self.normalize_associated_depth(env, alias, depth + 1);
                }
                match self.conformance_associated(env, base, interface.0, assoc) {
                    Some(resolved) if resolved != ty => {
                        self.normalize_associated_depth(env, resolved, depth + 1)
                    }
                    _ => self.store.project(base, interface, assoc),
                }
            }
            Type::Inst(class, args) => {
                let args = args
                    .into_iter()
                    .map(|item| self.normalize_associated_depth(env, item, depth + 1))
                    .collect();
                self.store.intern(Type::Inst(class, args))
            }
            Type::List(item) => {
                let item = self.normalize_associated_depth(env, item, depth + 1);
                self.store.intern(Type::List(item))
            }
            Type::Map(key, value) => {
                let key = self.normalize_associated_depth(env, key, depth + 1);
                let value = self.normalize_associated_depth(env, value, depth + 1);
                self.store.intern(Type::Map(key, value))
            }
            Type::Tuple(items) => {
                let items = items
                    .into_iter()
                    .map(|item| self.normalize_associated_depth(env, item, depth + 1))
                    .collect();
                self.store.intern(Type::Tuple(items))
            }
            Type::Fn(params, muts, ret, row) => {
                let params = params
                    .into_iter()
                    .map(|item| self.normalize_associated_depth(env, item, depth + 1))
                    .collect();
                let ret = self.normalize_associated_depth(env, ret, depth + 1);
                self.store.intern(Type::Fn(params, muts, ret, row))
            }
            Type::Callback(params, muts, ret, row) => {
                let params = params
                    .into_iter()
                    .map(|item| self.normalize_associated_depth(env, item, depth + 1))
                    .collect();
                let ret = self.normalize_associated_depth(env, ret, depth + 1);
                self.store.intern(Type::Callback(params, muts, ret, row))
            }
            Type::Run(item) => {
                let item = self.normalize_associated_depth(env, item, depth + 1);
                self.store.intern(Type::Run(item))
            }
            Type::Wait(item) => {
                let item = self.normalize_associated_depth(env, item, depth + 1);
                self.store.intern(Type::Wait(item))
            }
            Type::PendingCall(argument, reply) => {
                let argument = self.normalize_associated_depth(env, argument, depth + 1);
                let reply = self.normalize_associated_depth(env, reply, depth + 1);
                self.store.intern(Type::PendingCall(argument, reply))
            }
            Type::Handle(message, result) => {
                let message = self.normalize_associated_depth(env, message, depth + 1);
                let result = self.normalize_associated_depth(env, result, depth + 1);
                self.store.intern(Type::Handle(message, result))
            }
            Type::RunSnapshot(item) => {
                let item = self.normalize_associated_depth(env, item, depth + 1);
                self.store.intern(Type::RunSnapshot(item))
            }
            Type::Op(operation, function) => {
                let function = self.normalize_associated_depth(env, function, depth + 1);
                self.store.intern(Type::Op(operation, function))
            }
            _ => ty,
        }
    }

    /// Normalize the checked item equality of the core iteration interfaces.
    fn iterable_item_alias(
        &mut self,
        base: TypeId,
        interface: InterfaceId,
        assoc: u32,
    ) -> Option<TypeId> {
        let iterable = *self.core_interfaces.get("Iterable")?;
        let iterator = *self.core_interfaces.get("Iterator")?;
        if interface.0 != iterator {
            return None;
        }
        let iterator_item = self.interfaces[iterator as usize]
            .associated
            .iter()
            .position(|item| item.name == "Item")? as u32;
        if assoc != iterator_item {
            return None;
        }
        let Type::Projection {
            base: source,
            interface: owner,
            assoc: iter_assoc,
        } = self.store.get(base).clone()
        else {
            return None;
        };
        let iterable_iter = self.interfaces[iterable as usize]
            .associated
            .iter()
            .position(|item| item.name == "Iter")? as u32;
        if owner.0 != iterable || iter_assoc != iterable_iter {
            return None;
        }
        let iterable_item = self.interfaces[iterable as usize]
            .associated
            .iter()
            .position(|item| item.name == "Item")? as u32;
        Some(
            self.store
                .project(source, InterfaceId(iterable), iterable_item),
        )
    }

    /// Find the first failed premise of one conditional conformance.
    pub(crate) fn conformance_failure(
        &mut self,
        env: &TyEnv,
        ty: TypeId,
        interface: u32,
    ) -> Option<(TypeId, InterfaceUse)> {
        let (mut class, mut args) = self.store.nominal_class(ty)?;
        loop {
            let conformance = self
                .classes
                .get(class.0 as usize)?
                .conformances
                .iter()
                .find(|item| item.application.interface == interface)
                .cloned();
            if let Some(conformance) = conformance {
                for premise in &conformance.premises {
                    let actual = args.get(premise.param as usize).copied()?;
                    for bound in &premise.bounds {
                        let required = self.substitute_interface_use(bound, &args, &[]);
                        if !self.type_conforms(env, actual, &required) {
                            return Some((actual, required));
                        }
                    }
                }
            }
            let meta = self.store.class_meta(class).clone();
            let parent = meta.parent?;
            if meta.kind != ClassKind::EnumCase || !meta.parent_args.is_empty() {
                args = meta
                    .parent_args
                    .iter()
                    .map(|item| self.store.substitute(*item, &args, &[]))
                    .collect();
            }
            class = parent;
        }
    }

    /// Test one type against one application with bounded recursion.
    fn type_conforms_depth(
        &mut self,
        env: &TyEnv,
        ty: TypeId,
        required: &InterfaceUse,
        depth: u32,
    ) -> bool {
        let found = self.type_conformance_depth(env, ty, required.interface, depth);
        found.is_some_and(|application| {
            application.type_args == required.type_args && application.row_args == required.row_args
        })
    }

    /// Test the type arguments of one interface application.
    fn interface_arguments_meet_bounds(
        &mut self,
        env: &TyEnv,
        receiver: TypeId,
        application: &InterfaceUse,
    ) -> bool {
        let Some(contract) = self.interfaces.get(application.interface as usize) else {
            return false;
        };
        let type_bounds = contract.type_bounds.clone();
        let mut types = Vec::with_capacity(application.type_args.len() + 1);
        types.push(receiver);
        types.extend(application.type_args.iter().copied());
        for (actual, bounds) in application.type_args.iter().zip(type_bounds) {
            for bound in bounds {
                let required = self.substitute_interface_use(&bound, &types, &application.row_args);
                if !self.type_conforms(env, *actual, &required) {
                    return false;
                }
            }
        }
        true
    }

    /// Resolve one method through the interfaces on a static type.
    pub(crate) fn bound_method(
        &mut self,
        env: &TyEnv,
        ty: TypeId,
        name: &str,
        span: Span,
    ) -> Result<Option<ResolvedInterfaceMethod>, Diagnostic> {
        enum BoundSource {
            Variable(u32),
            Projection(InterfaceId, u32),
            None,
        }
        let source = match self.store.get(ty) {
            Type::Var(index) if *index >= env.type_offset => BoundSource::Variable(*index),
            Type::Projection {
                interface, assoc, ..
            } => BoundSource::Projection(*interface, *assoc),
            _ => BoundSource::None,
        };
        match source {
            BoundSource::Variable(index) => {
                let mut applications = env
                    .type_bounds
                    .get((index - env.type_offset) as usize)
                    .cloned()
                    .unwrap_or_default();
                for premise in env
                    .extra_bounds
                    .iter()
                    .filter(|premise| premise.subject == ty)
                {
                    applications.extend(premise.bounds.iter().cloned());
                }
                self.bound_method_in(&applications, ty, name, span)
            }
            BoundSource::Projection(interface, assoc) => {
                let mut applications = self.interfaces[interface.0 as usize]
                    .associated
                    .get(assoc as usize)
                    .map(|item| item.bounds.clone())
                    .unwrap_or_default();
                for premise in env
                    .extra_bounds
                    .iter()
                    .filter(|premise| premise.subject == ty)
                {
                    applications.extend(premise.bounds.iter().cloned());
                }
                self.bound_method_in(&applications, ty, name, span)
            }
            BoundSource::None => Ok(None),
        }
    }

    fn bound_method_in(
        &mut self,
        applications: &[InterfaceUse],
        _ty: TypeId,
        name: &str,
        span: Span,
    ) -> Result<Option<ResolvedInterfaceMethod>, Diagnostic> {
        let mut found: Option<ResolvedInterfaceMethod> = None;
        for application in applications {
            let contract = &self.interfaces[application.interface as usize];
            if let Some(method) = contract.find_method(name) {
                let requirement = Rc::clone(&contract.methods[method]);
                let candidate = (
                    application.clone(),
                    application.interface,
                    method as u32,
                    requirement,
                );
                if let Some(first) = &found {
                    if *first.3 != *candidate.3 {
                        return Err(Diagnostic::new(
                            "E1053",
                            format!("the interface method `{name}` is ambiguous"),
                            span,
                        ));
                    }
                } else {
                    found = Some(candidate);
                }
            }
        }
        Ok(found)
    }

    /// Find one interface default for a concrete receiver.
    pub(crate) fn default_method(
        &mut self,
        env: &TyEnv,
        ty: TypeId,
        name: &str,
        span: Span,
    ) -> Result<Option<ResolvedInterfaceMethod>, Diagnostic> {
        let Some(candidates) = self.interface_defaults.get(name).cloned() else {
            return Ok(None);
        };
        let mut found: Option<ResolvedInterfaceMethod> = None;
        for &(interface, method) in candidates.iter() {
            let Some(application) = self.type_conformance(env, ty, interface) else {
                continue;
            };
            let requirement =
                Rc::clone(&self.interfaces[interface as usize].methods[method as usize]);
            let candidate = (application, interface, method, requirement);
            if let Some(first) = &found {
                if first.3.default_binding != candidate.3.default_binding {
                    return Err(Diagnostic::new(
                        "E1053",
                        format!(
                            "the method `{name}` has defaults from `{}` and `{}`; add an explicit override",
                            self.interfaces[first.1 as usize].name,
                            self.interfaces[candidate.1 as usize].name
                        ),
                        span,
                    ));
                }
            } else {
                found = Some(candidate);
            }
        }
        Ok(found)
    }

    /// Find a method by name and return the declaring class with its
    /// type arguments, seen from `class`. The argument list is empty
    /// when the declaring class has no type parameters.
    pub(crate) fn find_method_owner(
        &mut self,
        class: u32,
        name: &str,
    ) -> Option<(Rc<MethodSig>, Vec<TypeId>, u32)> {
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
    ) -> Option<(Rc<MethodSig>, Vec<TypeId>, u32)> {
        let mut cur = start;
        let mut cur_args = args;
        loop {
            let info = &self.classes[cur as usize];
            let found = if info.method_index.is_empty() {
                info.methods
                    .iter()
                    .find(|method| method.name == name)
                    .cloned()
            } else {
                info.method_index
                    .binary_search_by(|index| info.methods[*index].name.as_str().cmp(name))
                    .ok()
                    .map(|position| info.methods[info.method_index[position]].clone())
            };
            if let Some(sig) = found {
                if cur_args.is_empty() {
                    return Some((sig, cur_args, cur));
                }
                let sig = self.substitute_method(&sig, &cur_args, arity)?;
                return Some((sig, cur_args, cur));
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
    fn substitute_method(
        &mut self,
        sig: &MethodSig,
        args: &[TypeId],
        arity: usize,
    ) -> Option<Rc<MethodSig>> {
        let env = TyEnv::default();
        for (actual, bounds) in args.iter().zip(&sig.class_type_bounds) {
            for bound in bounds {
                let required = self.substitute_interface_use(bound, args, &[]);
                if !self.type_conforms(&env, *actual, &required) {
                    return None;
                }
            }
        }
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
        let own_type_bounds = sig
            .own_type_bounds
            .iter()
            .map(|bounds| {
                bounds
                    .iter()
                    .map(|bound| self.substitute_interface_use(bound, &targs, &[]))
                    .collect()
            })
            .collect();
        Some(Rc::new(MethodSig {
            params,
            ret,
            class_type_bounds: vec![Vec::new(); arity],
            own_type_bounds,
            ..sig.clone()
        }))
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
            if name == "Self" {
                return env.self_ty.ok_or_else(|| {
                    Diagnostic::new(
                        "E1053",
                        "`Self` is available only in a class, enum, or interface",
                        ty.span,
                    )
                });
            }
            if let Some((base_name, associated)) = name.split_once('.') {
                if base_name == "Self" {
                    let Some(interface) = env.self_interface else {
                        return Err(Diagnostic::new(
                            "E1053",
                            "`Self` is available only in an interface contract",
                            ty.span,
                        ));
                    };
                    let found = ctx.interface_associated(interface, associated);
                    let (owner, assoc) = match found.as_slice() {
                        [(owner, assoc)] => (*owner, *assoc),
                        [] => {
                            return Err(Diagnostic::new(
                                "E1053",
                                format!(
                                    "the interface has no associated type named `{associated}`"
                                ),
                                ty.span,
                            ));
                        }
                        _ => {
                            return Err(Diagnostic::new(
                                "E1053",
                                format!("the associated type name `{associated}` is ambiguous"),
                                ty.span,
                            ));
                        }
                    };
                    let base = ctx.store.intern(Type::Var(0));
                    return Ok(ctx.store.project(base, InterfaceId(owner), assoc));
                }
                if let Some(position) = env.type_names.iter().position(|item| item == base_name) {
                    let base = ctx
                        .store
                        .intern(Type::Var(env.type_offset + position as u32));
                    return ctx.projection(env, base, position, associated, ty.span);
                }
            }
            if let Some(pos) = env.type_names.iter().position(|n| n == name) {
                return Ok(ctx.store.intern(Type::Var(env.type_offset + pos as u32)));
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
                return Ok(ctx.store.type_for_nominal(ClassId(class), Vec::new()));
            }
            if ctx.lookup_interface(name, env).is_some() {
                return Err(Diagnostic::new(
                    "E1013",
                    format!("`{name}` is an interface; use it as a generic bound"),
                    ty.span,
                ));
            }
            if let Some(interface) = env.self_interface {
                if !ctx.interface_associated(interface, name).is_empty() {
                    return Err(Diagnostic::new(
                        "E1013",
                        format!("`{name}` is an associated type; write `Self.{name}`"),
                        ty.span,
                    ));
                }
            }
            if matches!(
                name.as_str(),
                "List" | "Map" | "Run" | "Wait" | "RunSnapshot"
            ) {
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
            "Vm" => Err(Diagnostic::new(
                "E1024",
                "`Vm` takes no type arguments; use `Run[T]` for an active invocation",
                ty.span,
            )),
            "Run" => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("`Run` takes 1 type argument, found {}", args.len()),
                        ty.span,
                    ));
                }
                let result = resolve_type(ctx, env, &args[0])?;
                Ok(ctx.store.intern(Type::Run(result)))
            }
            "Wait" => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("`Wait` takes 1 type argument, found {}", args.len()),
                        ty.span,
                    ));
                }
                let result = resolve_type(ctx, env, &args[0])?;
                Ok(ctx.store.intern(Type::Wait(result)))
            }
            "RunSnapshot" => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("`RunSnapshot` takes 1 type argument, found {}", args.len()),
                        ty.span,
                    ));
                }
                let result = resolve_type(ctx, env, &args[0])?;
                Ok(ctx.store.intern(Type::RunSnapshot(result)))
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
                check_key_type(ctx, env, key, args[0].span)?;
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
                    return Ok(ctx.store.type_for_nominal(ClassId(class), resolved));
                }
                if ctx.lookup_interface(other, env).is_some() {
                    return Err(Diagnostic::new(
                        "E1013",
                        format!("`{other}` is an interface; use it as a generic bound"),
                        ty.span,
                    ));
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
            check_key_type(ctx, env, key_ty, key.span)?;
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
        if ctx.bundle.row_name_valid(&item.name) {
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

pub(crate) fn check_key_type(
    ctx: &mut Ctx,
    env: &TyEnv,
    key: TypeId,
    span: Span,
) -> Result<(), Diagnostic> {
    let native_class = ctx
        .store
        .nominal_class(key)
        .map(|(class, _)| class.0)
        .is_some_and(|class| {
            ["Text", "String", "Substring", "Char"]
                .iter()
                .any(|name| ctx.core_types.get(*name).copied() == Some(class))
        });
    let native = native_class
        || matches!(
            key,
            lm_types::BOOL | lm_types::INT | lm_types::FLOAT | lm_types::STRING | lm_types::BYTES
        );
    let unresolved = ctx.store.nominal_class(key).is_some_and(|(class, _)| {
        ctx.classes
            .get(class.0 as usize)
            .is_none_or(|info| info.name.is_empty())
    });
    if unresolved && ctx.defer_map_keys {
        ctx.deferred_map_keys.push((env.clone(), key, span));
        return Ok(());
    }
    let hashable = ctx.core_interfaces["Hashable"];
    if native || ctx.type_conformance(env, key, hashable).is_some() {
        Ok(())
    } else {
        Err(Diagnostic::new(
            "E1033",
            format!(
                "the type {} does not implement Hashable and cannot be a map key",
                ctx.display_type(env, key)
            ),
            span,
        ))
    }
}

/// Validate map keys after every class conformance is available.
fn check_deferred_map_keys(ctx: &mut Ctx) -> Result<(), Diagnostic> {
    ctx.defer_map_keys = false;
    let deferred = std::mem::take(&mut ctx.deferred_map_keys);
    for (env, key, span) in deferred {
        check_key_type(ctx, &env, key, span)?;
    }
    Ok(())
}

/// Check the mailbox message type of every proc class of one module.
///
/// `link_class_parents` recorded the message type as the parent
/// argument. The span comes from the same parent clause, so the
/// diagnostic points at the written type.
fn check_mailbox_types(ctx: &Ctx, module: &ast::Module) -> Result<(), Diagnostic> {
    let Some(proc_class) = ctx.core_types.get("Proc").copied() else {
        return Ok(());
    };
    for class in &module.classes {
        let Some(clause) = &class.parent else {
            continue;
        };
        // A bare `Proc` parent means `Proc[Never]`, which carries no
        // written type and no message.
        let Some(arg) = clause.args.first() else {
            continue;
        };
        let idx = ctx.user_types[&class.name];
        let meta = ctx.store.class_meta(ClassId(idx));
        if meta.parent != Some(ClassId(proc_class)) {
            continue;
        }
        let mailbox = meta.parent_args[0];
        check_mailbox_type(ctx, mailbox, arg.span)?;
    }
    Ok(())
}

/// Reject a mailbox message type that names a holder-local class.
///
/// Every message crosses a machine boundary as a copy. A holder-local
/// native class has no copy, so a mailbox of that type could never
/// accept one message. The rule rejects the declaration instead of
/// the send.
pub(crate) fn check_mailbox_type(ctx: &Ctx, mailbox: TypeId, span: Span) -> Result<(), Diagnostic> {
    let mut seen: Vec<u32> = Vec::new();
    match holder_local_part(ctx, mailbox, &mut seen) {
        None => Ok(()),
        Some(part) => Err(Diagnostic::new(
            "E1056",
            format!(
                "a mailbox message type must not name the holder-local type {}",
                ctx.store.display(part)
            ),
            span,
        )),
    }
}

/// The first holder-local part of one message type.
///
/// The walk reads the parts of a composite type, the type arguments
/// of a generic application, and the declared fields of every class
/// it reaches. A message carries the whole graph, so a holder-local
/// field is a holder-local message.
///
/// The walk stops at `Handle`, because a handle crosses as a typed
/// designator and its type arguments name no part of the handle
/// value.
fn holder_local_part(ctx: &Ctx, ty: TypeId, seen: &mut Vec<u32>) -> Option<TypeId> {
    if ctx.store.is_holder_local_native(ty) {
        return Some(ty);
    }
    if let Type::Class(class) = ctx.store.get(ty) {
        if matches!(
            ctx.classes[class.0 as usize].native_repr,
            Some(NativeRepr::StringBuilder | NativeRepr::ByteBuffer)
        ) {
            return Some(ty);
        }
    }
    match ctx.store.get(ty).clone() {
        Type::List(item) => holder_local_part(ctx, item, seen),
        Type::Map(key, value) => {
            holder_local_part(ctx, key, seen).or_else(|| holder_local_part(ctx, value, seen))
        }
        Type::Tuple(items) => items.iter().find_map(|t| holder_local_part(ctx, *t, seen)),
        Type::Class(class) => holder_local_class(ctx, class.0, seen),
        Type::Inst(class, args) => args
            .iter()
            .find_map(|t| holder_local_part(ctx, *t, seen))
            .or_else(|| holder_local_class(ctx, class.0, seen)),
        _ => None,
    }
}

/// The first holder-local field type of one class.
///
/// The walk covers the declared fields, inherited fields included,
/// and every arm of an enum family. A class graph may hold a cycle,
/// so `seen` stops a second visit of one class.
///
/// A field of a generic parameter type resolves to one type argument
/// of the application, and `holder_local_part` reads those arguments
/// before it reaches this walk.
fn holder_local_class(ctx: &Ctx, class: u32, seen: &mut Vec<u32>) -> Option<TypeId> {
    if seen.contains(&class) {
        return None;
    }
    seen.push(class);
    let info = &ctx.classes[class as usize];
    for field in &info.field_tys {
        if let Some(part) = holder_local_part(ctx, *field, seen) {
            return Some(part);
        }
    }
    for arm in &info.arms {
        if let Some(part) = holder_local_class(ctx, *arm, seen) {
            return Some(part);
        }
    }
    None
}

/// Split generic parameters into type names and effect names.
fn split_generics(generics: &[ast::GenericParam]) -> (Vec<String>, Vec<String>) {
    let effect_count = generics.iter().filter(|generic| generic.is_effect).count();
    let mut type_names = Vec::with_capacity(generics.len() - effect_count);
    let mut effect_names = Vec::with_capacity(effect_count);
    for g in generics {
        if g.is_effect {
            effect_names.push(g.name.clone());
        } else {
            type_names.push(g.name.clone());
        }
    }
    (type_names, effect_names)
}

fn interface_default_binding(interface: &str, method: &str) -> String {
    format!("$default.{interface}.{method}")
}

/// Index each interface default after all interfaces resolve.
fn index_interface_defaults(ctx: &mut Ctx) {
    let mut defaults: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    for (interface, contract) in ctx.interfaces.iter().enumerate() {
        for (method, requirement) in contract.methods.iter().enumerate() {
            if requirement.default_binding.is_some() {
                defaults
                    .entry(requirement.name.clone())
                    .or_default()
                    .push((interface as u32, method as u32));
            }
        }
    }
    ctx.interface_defaults = defaults
        .into_iter()
        .map(|(name, methods)| (name, Rc::from(methods)))
        .collect();
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
    let compiled_core = if options.build_core_provider {
        None
    } else {
        options.core.as_deref()
    };
    if let Some(core) = compiled_core {
        if core.module_path() != lm_bytecode::CORE_MODULE {
            return Err(Diagnostic::new(
                "E0290",
                "the implicit core dependency has another module path",
                Span::new(0, 0),
            ));
        }
        if core.interface().bundle_digest != options.bundle.digest() {
            return Err(Diagnostic::new(
                "E0290",
                "the implicit core dependency uses another ABI bundle",
                Span::new(0, 0),
            ));
        }
    }
    let core = if compiled_core.is_some() {
        empty_ast()
    } else {
        core_ast()
    };
    let class_count = |module: &ast::Module| {
        module.classes.len()
            + module
                .enums
                .iter()
                .map(|item| item.arms.len() + 1)
                .sum::<usize>()
    };
    let method_count = |module: &ast::Module| {
        module
            .classes
            .iter()
            .map(|item| item.methods.len())
            .sum::<usize>()
            + module
                .enums
                .iter()
                .map(|item| item.methods.len())
                .sum::<usize>()
    };
    let interface_default_count = |module: &ast::Module| {
        module
            .interfaces
            .iter()
            .flat_map(|interface| &interface.methods)
            .filter(|method| method.body.is_some())
            .count()
    };
    let compiled_core_exports = compiled_core
        .map(|unit| unit.interface().exports.len())
        .unwrap_or(0);
    let total_classes = class_count(core) + class_count(module) + compiled_core_exports;
    let total_funcs = core.funcs.len()
        + module.funcs.len()
        + method_count(core)
        + method_count(module)
        + interface_default_count(core)
        + interface_default_count(module)
        + 1;
    let mut store = TypeStore::new_with_bundle(options.bundle.clone());
    store.reserve_classes(total_classes);
    store.reserve_types(total_funcs);
    let mut ctx = Ctx {
        bundle: options.bundle.clone(),
        store,
        classes: Vec::new(),
        user_types: HashMap::with_capacity(module.classes.len() + module.enums.len()),
        core_types: HashMap::with_capacity(
            core.classes.len() + core.enums.len() + compiled_core_exports,
        ),
        interfaces: Vec::with_capacity(
            core.interfaces.len() + module.interfaces.len() + compiled_core_exports,
        ),
        user_interfaces: HashMap::with_capacity(module.interfaces.len()),
        core_interfaces: HashMap::with_capacity(core.interfaces.len() + compiled_core_exports),
        interface_defaults: HashMap::new(),
        prelude: options.prelude,
        func_index: HashMap::with_capacity(module.funcs.len()),
        core_func_index: HashMap::with_capacity(core.funcs.len() + compiled_core_exports),
        sigs: Vec::with_capacity(total_funcs),
        funcs: Vec::with_capacity(total_funcs),
        reified_functions: BTreeSet::new(),
        reified_classes: BTreeSet::new(),
        core: CoreIds {
            option_class: 0,
            some_class: 0,
            none_class: 0,
            partial_eq_interface: 0,
            partial_eq_method: 0,
            hashable_interface: 0,
            hashable_method: 0,
        },
        uses: HashMap::new(),
        imports: Vec::new(),
        user_start: 0,
        import_start: 0,
        user_interface_start: 0,
        import_interface_start: 0,
        deferred_map_keys: Vec::new(),
        defer_map_keys: true,
    };
    // Pass 1: predeclare all type names. The core comes first, so a
    // core class that a module class inherits always keeps the lower
    // class index. Every later table reads that order: the verifier,
    // the dispatch builder, and the linker all require a parent to
    // precede its child.
    let mut core_imports = crate::import::ImportEnv::new();
    if let Some(unit) = compiled_core {
        core_imports.modules.insert(
            lm_bytecode::CORE_MODULE.to_string(),
            unit.interface().clone(),
        );
    }
    let mut core_materializer = crate::import::Materializer::new_core(&core_imports);
    if let Some(unit) = compiled_core {
        let mut demand = CoreDemand::for_module(module, &options.bundle);
        demand.names.extend(options.core_roots.iter().cloned());
        crate::import::add_used_core_names(&options.imports, &module.uses, &mut demand.names);
        core_materializer.reserve_unit(
            &mut ctx,
            unit,
            &options.core_intrinsics,
            &demand.names,
            &demand.methods,
            Span::new(0, 0),
        )?;
    } else {
        register_interface_names(&mut ctx, core, true).expect("the core interfaces register");
        register_type_names(&mut ctx, core, true).expect("the core type names register");
    }
    ctx.user_interface_start = ctx.interfaces.len() as u32;
    register_interface_names(&mut ctx, module, false)?;
    ctx.import_interface_start = ctx.interfaces.len() as u32;
    ctx.user_start = ctx.store.class_count() as u32;
    register_type_names(&mut ctx, module, false)?;
    ctx.import_start = ctx.store.class_count() as u32;
    // Import phase A: reserve the imported class indices before any
    // signature resolves, so a user signature may name an imported
    // type. Phase B fills the declarations after the core lands.
    let mut materializer = crate::import::Materializer::new(&options.imports);
    ctx.uses = resolve_uses(&mut ctx, &mut materializer, &options.imports, &module.uses)?;
    let import_span = module
        .uses
        .first()
        .map(|use_decl| use_decl.span)
        .unwrap_or(Span::new(0, 0));
    if compiled_core.is_none() {
        link_class_parents(&mut ctx, core, true).expect("the core class parents link");
    }
    link_class_parents(&mut ctx, module, false)?;
    if compiled_core.is_some() {
        core_materializer.finish_interfaces(&mut ctx, Span::new(0, 0))?;
    } else {
        resolve_all_interfaces(&mut ctx, core, true).map_err(core_defect)?;
    }
    materializer.finish_interfaces(&mut ctx, import_span)?;
    resolve_all_interfaces(&mut ctx, module, false)?;
    index_interface_defaults(&mut ctx);
    let option_class = ctx.core_types.get("Option").copied().unwrap_or(u32::MAX);
    let some_class = option_class.saturating_add(1);
    let none_class = option_class.saturating_add(2);
    let partial_eq_interface = ctx.core_interfaces["PartialEq"];
    let hashable_interface = ctx.core_interfaces["Hashable"];
    ctx.core = CoreIds {
        option_class,
        some_class,
        none_class,
        partial_eq_interface,
        partial_eq_method: ctx.interfaces[partial_eq_interface as usize]
            .methods
            .iter()
            .position(|method| method.name == "__eq__")
            .expect("PartialEq declares __eq__") as u32,
        hashable_interface,
        hashable_method: ctx.interfaces[hashable_interface as usize]
            .methods
            .iter()
            .position(|method| method.name == "__hash__")
            .expect("Hashable declares __hash__") as u32,
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
        let mut env = TyEnv {
            type_names: type_names.clone(),
            type_bounds: vec![Vec::new(); type_names.len()],
            extra_bounds: Vec::new(),
            effect_names: effect_names.clone(),
            type_offset: 0,
            self_interface: None,
            self_ty: None,
            core_scope: false,
        };
        env.type_bounds = resolve_generic_bounds(&mut ctx, &env, &func.generics)?;
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
    for func in &core.funcs {
        if ctx.core_func_index.contains_key(&func.name) || ctx.core_types.contains_key(&func.name) {
            return Err(core_defect(Diagnostic::new(
                "E1010",
                format!("the name `{}` has more than one core definition", func.name),
                func.name_span,
            )));
        }
        let (type_names, effect_names) = split_generics(&func.generics);
        let mut env = TyEnv {
            type_names: type_names.clone(),
            type_bounds: vec![Vec::new(); type_names.len()],
            extra_bounds: Vec::new(),
            effect_names: effect_names.clone(),
            type_offset: 0,
            self_interface: None,
            self_ty: None,
            core_scope: true,
        };
        env.type_bounds =
            resolve_generic_bounds(&mut ctx, &env, &func.generics).map_err(core_defect)?;
        let sig = resolve_sig(
            &mut ctx,
            &env,
            type_names,
            effect_names,
            &func.params,
            &func.ret,
            &func.row,
            None,
        )
        .map_err(core_defect)?;
        let index = ctx.funcs.len() as u32;
        ctx.core_func_index.insert(func.name.clone(), index);
        ctx.sigs.push(sig);
        ctx.funcs.push(None);
    }
    reserve_interface_defaults(&mut ctx, core, true).map_err(core_defect)?;
    reserve_interface_defaults(&mut ctx, module, false)?;
    // The class table is index addressed from here on. Registration
    // fixed every index, so a later pass may fill the entries in any
    // order. The core resolves first, because a user class may name a
    // core class as its parent.
    ctx.classes = (0..ctx.store.class_count() as u32)
        .map(ClassInfo::placeholder)
        .collect();
    // Pass 2b: fill dependency declarations before local classes.
    // A local class can inherit one dependency class.
    let core_fields = if compiled_core.is_some() {
        core_materializer.finish(&mut ctx, Span::new(0, 0))?
    } else {
        resolve_all_classes(&mut ctx, core, true).map_err(core_defect)?;
        Vec::new()
    };
    let import_fields = materializer.finish(&mut ctx, import_span)?;
    // Pass 2c: resolve user classes and enums in class-index order.
    resolve_all_classes(&mut ctx, module, false)?;
    if compiled_core.is_none() {
        check_frozen_classes(&ctx, core, true).map_err(core_defect)?;
    }
    check_frozen_classes(&ctx, module, false)?;
    let self_dependent_interfaces = interface_self_dependencies(&ctx);
    if compiled_core.is_none() {
        check_all_conformances(&mut ctx, core, true, &self_dependent_interfaces)
            .map_err(core_defect)?;
    }
    check_all_conformances(&mut ctx, module, false, &self_dependent_interfaces)?;
    // Pass 2d: check every mailbox message type. The walk reads the
    // declared fields, so it runs after every class resolves.
    check_mailbox_types(&ctx, module)?;
    // Reserve the entry function index.
    let entry_idx = ctx.funcs.len();
    ctx.funcs.push(None);
    ctx.sigs.push(FnSig {
        type_params: vec![],
        type_bounds: vec![],
        effect_params: vec![],
        params: vec![],
        param_muts: vec![],
        param_names: vec![],
        ret: UNIT,
        row: vec![],
    });
    check_deferred_map_keys(&mut ctx)?;
    // Pass 3: check field defaults. The table is index addressed, like
    // the class table.
    let mut own_defaults: Vec<Vec<(Option<HExpr>, Vec<TypeId>)>> =
        vec![Vec::new(); ctx.classes.len()];
    check_defaults(&mut ctx, module, false, &mut own_defaults)?;
    if compiled_core.is_none() {
        check_defaults(&mut ctx, core, true, &mut own_defaults).map_err(core_defect)?;
    }
    // An imported declaration carries no default expression: the
    // provider construction function evaluates its own defaults. The
    // entries follow the user and the core classes, so the table
    // stays aligned with the class indices.
    for (class, count) in core_fields.into_iter().chain(import_fields) {
        own_defaults[class as usize] = vec![(None, Vec::new()); count];
    }
    // Pass 4: check top-level function bodies.
    for (idx, func) in module.funcs.iter().enumerate() {
        let mut sig = ctx.sigs[idx].clone();
        let type_param_count = sig.type_params.len() as u32;
        let effect_param_count = sig.effect_params.len() as u32;
        let env = TyEnv {
            type_names: std::mem::take(&mut sig.type_params),
            type_bounds: std::mem::take(&mut sig.type_bounds),
            extra_bounds: Vec::new(),
            effect_names: std::mem::take(&mut sig.effect_params),
            type_offset: 0,
            self_interface: None,
            self_ty: None,
            core_scope: false,
        };
        let mut checker = FnChecker::top_level(RetKind::Known(sig.ret), env, sig.row.clone());
        checker.reserve_parameters(func.params.len());
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
        let type_bounds = into_hir_bounds(checked.type_bounds);
        ctx.funcs[idx] = Some(HirFunc {
            imported: false,
            core: false,
            source_span: Some(func.span),
            name: func.name.clone(),
            type_params: type_param_count,
            type_bounds,
            effect_params: effect_param_count,
            params: sig.params,
            param_muts: sig.param_muts,
            param_names: sig.param_names,
            ret: sig.ret,
            row: sig.row,
            captures: vec![],
            locals: checked.locals,
            body: checked.body,
        });
    }
    for func in &core.funcs {
        let index = ctx.core_func_index[&func.name] as usize;
        let mut sig = ctx.sigs[index].clone();
        let type_param_count = sig.type_params.len() as u32;
        let effect_param_count = sig.effect_params.len() as u32;
        let env = TyEnv {
            type_names: std::mem::take(&mut sig.type_params),
            type_bounds: std::mem::take(&mut sig.type_bounds),
            extra_bounds: Vec::new(),
            effect_names: std::mem::take(&mut sig.effect_params),
            type_offset: 0,
            self_interface: None,
            self_ty: None,
            core_scope: true,
        };
        let mut checker = FnChecker::top_level(RetKind::Known(sig.ret), env, sig.row.clone());
        checker.reserve_parameters(func.params.len());
        for (slot, param) in func.params.iter().enumerate() {
            checker
                .locals
                .push((sig.params[slot], sig.param_muts[slot]));
            if checker.scopes[0]
                .insert(param.name.clone(), slot as u32)
                .is_some()
            {
                return Err(core_defect(Diagnostic::new(
                    "E1014",
                    format!("duplicate parameter name `{}`", param.name),
                    param.span,
                )));
            }
        }
        let checked = checker
            .check_callable(&mut ctx, &func.body, sig.ret, func.span)
            .map_err(core_defect)?;
        let type_bounds = into_hir_bounds(checked.type_bounds);
        ctx.funcs[index] = Some(HirFunc {
            imported: false,
            core: true,
            source_span: None,
            name: func.name.clone(),
            type_params: type_param_count,
            type_bounds,
            effect_params: effect_param_count,
            params: sig.params,
            param_muts: sig.param_muts,
            param_names: sig.param_names,
            ret: sig.ret,
            row: sig.row,
            captures: vec![],
            locals: checked.locals,
            body: checked.body,
        });
    }
    // Pass 5: check user method bodies, then core method bodies.
    check_all_methods(&mut ctx, module, false)?;
    check_all_methods(&mut ctx, core, true).map_err(core_defect)?;
    check_interface_defaults(&mut ctx, module, false)?;
    check_interface_defaults(&mut ctx, core, true).map_err(core_defect)?;
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
    let exports = collect_exports(&ctx, module, &options.module_path, false)?;
    ctx.funcs[entry_idx] = Some(HirFunc {
        imported: false,
        core: false,
        source_span: module
            .entry
            .first()
            .zip(module.entry.last())
            .map(|(first, last)| first.span.to(last.span)),
        name: "<entry>".to_string(),
        type_params: 0,
        type_bounds: Vec::new(),
        effect_params: 0,
        params: vec![],
        param_muts: vec![],
        param_names: vec![],
        ret: entry_ty,
        row: entry_row,
        captures: vec![],
        locals,
        body,
    });
    let core_exports = if options.build_core_provider {
        collect_core_exports(&ctx, core)?
    } else {
        Vec::new()
    };
    assemble(
        ctx,
        own_defaults,
        entry_idx,
        ExportSets {
            module: exports,
            core: core_exports,
        },
        &options.module_path,
        module.funcs.len(),
        core.funcs.len(),
    )
}

struct ExportSets {
    module: Vec<HirExport>,
    core: Vec<HirExport>,
}

/// Collect the exported top-level definitions of the source module,
/// in declaration order. Core and imported declarations stay out. A
/// module exports only what it defines.
fn collect_exports(
    ctx: &Ctx,
    module: &ast::Module,
    _module_path: &str,
    is_core: bool,
) -> Result<Vec<HirExport>, Diagnostic> {
    let mut out: Vec<(lm_bytecode::ExportKind, String, u32)> = Vec::new();
    let class_index = |name: &str| -> u32 {
        let types = if is_core {
            &ctx.core_types
        } else {
            &ctx.user_types
        };
        *types
            .get(name)
            .expect("every declared class name registers")
    };
    for interface in &module.interfaces {
        let interfaces = if is_core {
            &ctx.core_interfaces
        } else {
            &ctx.user_interfaces
        };
        let interface_index = interfaces[&interface.name];
        out.push((
            lm_bytecode::ExportKind::Interface,
            interface.name.clone(),
            interface_index,
        ));
        let info = &ctx.interfaces[interface_index as usize];
        for method in &info.methods {
            if let (Some(binding), Some(func)) = (&method.default_binding, method.default_func) {
                out.push((lm_bytecode::ExportKind::Function, binding.clone(), func));
            }
        }
    }
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
        let functions = if is_core {
            &ctx.core_func_index
        } else {
            &ctx.func_index
        };
        out.push((
            lm_bytecode::ExportKind::Function,
            func.name.clone(),
            functions[&func.name],
        ));
    }
    Ok(out
        .into_iter()
        .map(|(kind, name, def)| HirExport { kind, name, def })
        .collect())
}

/// Collect the complete provider surface of the pinned core.
fn collect_core_exports(ctx: &Ctx, module: &ast::Module) -> Result<Vec<HirExport>, Diagnostic> {
    let mut exports = collect_exports(ctx, module, lm_bytecode::CORE_MODULE, true)?;
    let exported: BTreeSet<u32> = exports
        .iter()
        .filter(|item| item.kind == lm_bytecode::ExportKind::Function)
        .map(|item| item.def)
        .collect();
    let methods: BTreeSet<u32> = ctx.classes[..ctx.user_start as usize]
        .iter()
        .flat_map(|class| class.methods.iter().map(|method| method.func))
        .collect();
    let mut ordinal = 0u32;
    for (index, function) in ctx.funcs.iter().enumerate() {
        let Some(function) = function else {
            continue;
        };
        if !function.core {
            continue;
        }
        let current = ordinal;
        ordinal += 1;
        let index = index as u32;
        if methods.contains(&index) || exported.contains(&index) {
            continue;
        }
        exports.push(HirExport {
            kind: lm_bytecode::ExportKind::Function,
            name: format!("$internal.function.{current}"),
            def: index,
        });
    }
    Ok(exports)
}

/// Reserve one verified function for each local interface default.
fn reserve_interface_defaults(
    ctx: &mut Ctx,
    module: &ast::Module,
    is_core: bool,
) -> Result<(), Diagnostic> {
    for declaration in &module.interfaces {
        let interface = if is_core {
            ctx.core_interfaces[&declaration.name]
        } else {
            ctx.user_interfaces[&declaration.name]
        };
        let contract = ctx.interfaces[interface as usize].clone();
        for (index, method) in declaration.methods.iter().enumerate() {
            if method.body.is_none() {
                continue;
            }
            let requirement = Rc::clone(&contract.methods[index]);
            let self_ty = ctx.store.intern(Type::Var(0));
            let mut type_names = Vec::with_capacity(
                1 + contract.type_params.len() + requirement.own_type_params.len(),
            );
            type_names.push("Self".to_string());
            type_names.extend(contract.type_params.iter().cloned());
            type_names.extend(requirement.own_type_params.iter().cloned());
            let application = InterfaceUse {
                interface,
                type_args: (0..contract.type_params.len())
                    .map(|at| ctx.store.intern(Type::Var(at as u32 + 1)))
                    .collect(),
                row_args: (0..contract.effect_params.len())
                    .map(|at| vec![RowElem::Var(at as u32)])
                    .collect(),
            };
            let mut type_bounds = Vec::with_capacity(type_names.len());
            type_bounds.push(vec![application]);
            type_bounds.extend(contract.type_bounds.iter().cloned());
            type_bounds.extend(requirement.own_type_bounds.iter().cloned());
            let mut effect_names = contract.effect_params.clone();
            effect_names.extend(requirement.own_effect_params.iter().cloned());
            let mut params = Vec::with_capacity(requirement.params.len() + 1);
            params.push(self_ty);
            params.extend(requirement.params.iter().copied());
            let mut param_muts = Vec::with_capacity(requirement.param_muts.len() + 1);
            param_muts.push(requirement.mut_self);
            param_muts.extend(requirement.param_muts.iter().copied());
            let mut param_names = Vec::with_capacity(requirement.param_names.len() + 1);
            param_names.push("self".to_string());
            param_names.extend(requirement.param_names.iter().cloned());
            let func = ctx.funcs.len() as u32;
            ctx.sigs.push(FnSig {
                type_params: type_names,
                type_bounds,
                effect_params: effect_names,
                params,
                param_muts,
                param_names,
                ret: requirement.ret,
                row: requirement.row.clone(),
            });
            ctx.funcs.push(None);
            Rc::make_mut(&mut ctx.interfaces[interface as usize].methods[index]).default_func =
                Some(func);
        }
    }
    Ok(())
}

/// A defect in the pinned core sources is an implementation defect,
/// not a user diagnostic.
fn core_defect(d: Diagnostic) -> Diagnostic {
    let source = lm_source::SourceFile::new("core", CORE_SOURCE);
    panic!(
        "the pinned core sources do not check: {}",
        d.render(&source)
    );
}

fn take_method(method: Rc<MethodSig>) -> MethodSig {
    match Rc::try_unwrap(method) {
        Ok(method) => method,
        Err(method) => (*method).clone(),
    }
}

fn take_interface_method(method: Rc<InterfaceMethodSig>) -> InterfaceMethodSig {
    match Rc::try_unwrap(method) {
        Ok(method) => method,
        Err(method) => (*method).clone(),
    }
}

fn take_conformance(conformance: Rc<ConformanceInfo>) -> ConformanceInfo {
    match Rc::try_unwrap(conformance) {
        Ok(conformance) => conformance,
        Err(conformance) => (*conformance).clone(),
    }
}

/// Build one checked module.
///
/// Source functions come first. Core functions follow them.
fn assemble(
    mut ctx: Ctx,
    own_defaults: Vec<Vec<(Option<HExpr>, Vec<TypeId>)>>,
    entry_idx: usize,
    exports: ExportSets,
    module_path: &str,
    source_funcs: usize,
    core_funcs: usize,
) -> Result<HirModule, Diagnostic> {
    let keys: Vec<String> = (0..ctx.classes.len() as u32)
        .map(|class| ctx.class_key(class, module_path))
        .collect();
    let parents: Vec<Option<u32>> = ctx.classes.iter().map(|info| info.parent).collect();
    let classes = std::mem::take(&mut ctx.classes);
    let mut hir_classes: Vec<HirClass> = Vec::with_capacity(classes.len());
    let mut conformances = Vec::new();
    for (idx, (mut info, key)) in classes.into_iter().zip(keys).enumerate() {
        // A class inherits the field defaults of its ancestors. The
        // parent index may be greater than the child index, because a
        // module class may inherit a core class, so the walk collects
        // the chain instead of reading an earlier result.
        let mut chain: Vec<usize> = vec![idx];
        let mut cur = info.parent;
        while let Some(p) = cur {
            chain.push(p as usize);
            cur = parents[p as usize];
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
        let mut init = info.init.take().map(take_method);
        let (ctor_params, ctor_param_muts, ctor_param_names) =
            match (&mut init, info.kind, info.native_repr) {
                (_, _, Some(NativeRepr::Tuple(_))) => {
                    let Type::Tuple(params) = ctx.store.get(info.self_ty) else {
                        unreachable!("a tuple carrier has a tuple self type")
                    };
                    (params.clone(), vec![false; params.len()], Vec::new())
                }
                (_, ClassKind::EnumCase, _) => {
                    let count = info.field_tys.len();
                    (
                        info.field_tys.clone(),
                        vec![false; count],
                        info.field_names.clone(),
                    )
                }
                (Some(init), _, _) => (
                    std::mem::take(&mut init.params),
                    std::mem::take(&mut init.param_muts),
                    std::mem::take(&mut init.param_names),
                ),
                (None, _, _) => (vec![], vec![], vec![]),
            };
        let ctor_row = init
            .as_mut()
            .map(|method| std::mem::take(&mut method.row))
            .unwrap_or_default();
        let init_func = init.as_ref().map(|method| method.func);
        for conformance in info.conformances.drain(..) {
            let conformance = take_conformance(conformance);
            conformances.push(HirConformance {
                class: idx as u32,
                application: into_hir_interface_use(conformance.application),
                premises: conformance
                    .premises
                    .into_iter()
                    .map(|premise| HirConformancePremise {
                        param: premise.param,
                        bounds: premise
                            .bounds
                            .into_iter()
                            .map(into_hir_interface_use)
                            .collect(),
                    })
                    .collect(),
                associated: conformance.associated,
                method_overrides: conformance.method_overrides,
            });
        }
        hir_classes.push(HirClass {
            imported: info.imported,
            source_span: info.source_span,
            is_final: info.is_final,
            is_frozen: info.is_frozen,
            native_repr: info.native_repr,
            name: info.name,
            key,
            parent: info.parent,
            parent_args: ctx
                .store
                .class_meta(ClassId(idx as u32))
                .parent_args
                .clone(),
            type_params: info.type_params.len() as u32,
            type_bounds: into_hir_bounds(info.type_bounds),
            kind: info.kind,
            ctor_kind,
            field_names: info.field_names,
            field_tys: info.field_tys,
            field_defaults: info.has_default,
            own_start: info.own_start as u32,
            defaults,
            default_locals,
            methods: info
                .methods
                .into_iter()
                .map(|method| {
                    let method = take_method(method);
                    (method.name, method.func)
                })
                .collect(),
            init: init_func,
            ctor_param_names,
            ctor_params,
            ctor_param_muts,
            ctor_row,
        });
    }
    let interfaces: Vec<HirInterface> = std::mem::take(&mut ctx.interfaces)
        .into_iter()
        .enumerate()
        .map(|(index, info)| HirInterface {
            key: if (index as u32) < ctx.user_interface_start {
                lm_bytecode::qualified_key(lm_bytecode::CORE_MODULE, &info.name)
            } else if let Some((origin, name)) = &info.origin {
                lm_bytecode::qualified_key(origin, name)
            } else {
                lm_bytecode::qualified_key(module_path, &info.name)
            },
            name: info.name,
            type_params: info.type_params.len() as u32,
            effect_params: info.effect_params.len() as u32,
            generic_is_effect: info.generic_is_effect,
            parents: info
                .parents
                .into_iter()
                .map(into_hir_interface_use)
                .collect(),
            type_bounds: into_hir_bounds(info.type_bounds),
            associated: info
                .associated
                .into_iter()
                .map(|item| HirAssociated {
                    name: item.name,
                    bounds: item
                        .bounds
                        .into_iter()
                        .map(into_hir_interface_use)
                        .collect(),
                })
                .collect(),
            methods: info
                .methods
                .into_iter()
                .map(|method| {
                    let method = take_interface_method(method);
                    HirInterfaceMethod {
                        selector: method.name,
                        mut_self: method.mut_self,
                        type_params: method.own_type_params.len() as u32,
                        type_bounds: into_hir_bounds(method.own_type_bounds),
                        effect_params: method.own_effect_params.len() as u32,
                        premises: method
                            .premises
                            .into_iter()
                            .map(|premise| crate::hir::HirTypePremise {
                                subject: premise.subject,
                                bounds: premise
                                    .bounds
                                    .into_iter()
                                    .map(into_hir_interface_use)
                                    .collect(),
                            })
                            .collect(),
                        params: method.params,
                        param_muts: method.param_muts,
                        param_names: method.param_names,
                        ret: method.ret,
                        row: method.row,
                        default: method.default_func,
                        default_binding: method.default_binding,
                    }
                })
                .collect(),
        })
        .collect();
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
    let reified_functions = ctx.reified_functions;
    let reified_classes = ctx.reified_classes;
    let funcs: Vec<HirFunc> = ctx
        .funcs
        .into_iter()
        .map(|f| f.expect("every reserved function is checked"))
        .collect();
    // The named function bindings this module declares. A name points
    // at a function value; it is never a part of that value. A free
    // function takes the module path as its root. A class member takes
    // the qualified key of its class.
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
    for (idx, func) in funcs.iter().enumerate().skip(source_funcs).take(core_funcs) {
        bindings.push(lm_bytecode::FuncBinding {
            key: lm_bytecode::qualified_key(lm_bytecode::CORE_MODULE, &func.name),
            func: idx as u32,
            class: lm_bytecode::NO_CLASS,
        });
    }
    for interface in &interfaces {
        for method in &interface.methods {
            let (Some(func), Some(binding)) = (method.default, &method.default_binding) else {
                continue;
            };
            if funcs[func as usize].imported {
                continue;
            }
            let module = if interface.key.starts_with("core.") {
                lm_bytecode::CORE_MODULE
            } else {
                module_path
            };
            bindings.push(lm_bytecode::FuncBinding {
                key: lm_bytecode::qualified_key(module, binding),
                func,
                class: lm_bytecode::NO_CLASS,
            });
        }
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
    let mut binding_keys: HashSet<String> =
        bindings.iter().map(|binding| binding.key.clone()).collect();
    for export in &exports.core {
        if export.kind != lm_bytecode::ExportKind::Function {
            continue;
        }
        let key = lm_bytecode::qualified_key(lm_bytecode::CORE_MODULE, &export.name);
        if binding_keys.insert(key.clone()) {
            bindings.push(lm_bytecode::FuncBinding {
                key,
                func: export.def,
                class: lm_bytecode::NO_CLASS,
            });
        }
    }
    Ok(HirModule {
        bundle: ctx.bundle,
        store: ctx.store,
        interfaces,
        conformances,
        classes: hir_classes,
        funcs,
        entry: entry_idx,
        core: ctx.core,
        core_roles,
        exports: exports.module,
        core_exports: exports.core,
        imports: ctx.imports,
        bindings,
        reified_functions,
        reified_classes,
    })
}

/// Register all interface names before any interface signature resolves.
fn register_interface_names(
    ctx: &mut Ctx,
    module: &ast::Module,
    is_core: bool,
) -> Result<(), Diagnostic> {
    for interface in &module.interfaces {
        if ctx.interfaces.len() > lm_bytecode::MAX_INTERFACE_CALL_INDEX as usize {
            return Err(Diagnostic::new(
                "E1024",
                "the module has too many interfaces for compact calls",
                interface.name_span,
            ));
        }
        let map = if is_core {
            &mut ctx.core_interfaces
        } else {
            &mut ctx.user_interfaces
        };
        if map.contains_key(&interface.name) {
            return Err(Diagnostic::new(
                "E1010",
                format!("the name `{}` has more than one definition", interface.name),
                interface.name_span,
            ));
        }
        let (type_params, effect_params) = split_generics(&interface.generics);
        let id = ctx.interfaces.len() as u32;
        map.insert(interface.name.clone(), id);
        ctx.interfaces.push(InterfaceInfo {
            origin: None,
            name: interface.name.clone(),
            type_params,
            effect_params,
            generic_is_effect: interface
                .generics
                .iter()
                .map(|item| item.is_effect)
                .collect(),
            parents: Vec::new(),
            type_bounds: Vec::new(),
            associated: Vec::new(),
            methods: Vec::new(),
            method_index: Vec::new(),
        });
    }
    Ok(())
}

/// Resolve one interface application in a type environment.
fn resolve_interface_use(
    ctx: &mut Ctx,
    env: &TyEnv,
    reference: &ast::InterfaceRef,
) -> Result<InterfaceUse, Diagnostic> {
    let interface = ctx.lookup_interface(&reference.name, env).ok_or_else(|| {
        Diagnostic::new(
            "E1053",
            format!("unknown interface name `{}`", reference.name),
            reference.span,
        )
    })?;
    let kinds = ctx.interfaces[interface as usize].generic_is_effect.clone();
    let type_count = kinds.iter().filter(|is_effect| !**is_effect).count();
    let effect_count = kinds.iter().filter(|is_effect| **is_effect).count();
    if type_count != reference.type_args.len() {
        return Err(Diagnostic::new(
            "E1053",
            format!(
                "the interface `{}` takes {type_count} type argument(s), found {}",
                reference.name,
                reference.type_args.len()
            ),
            reference.span,
        ));
    }
    if !reference.row_args.is_empty() && effect_count != reference.row_args.len() {
        return Err(Diagnostic::new(
            "E1053",
            format!(
                "the interface `{}` takes {effect_count} effect argument(s), found {}",
                reference.name,
                reference.row_args.len()
            ),
            reference.span,
        ));
    }
    let mut type_args = Vec::with_capacity(reference.type_args.len());
    for argument in &reference.type_args {
        type_args.push(resolve_type(ctx, env, argument)?);
    }
    let row_args = if reference.row_args.is_empty() {
        vec![Vec::new(); effect_count]
    } else {
        reference
            .row_args
            .iter()
            .map(|argument| resolve_row(ctx, env, &argument.row))
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(InterfaceUse {
        interface,
        type_args,
        row_args,
    })
}

/// Resolve one conjunction of interface bounds.
fn resolve_interface_bounds(
    ctx: &mut Ctx,
    env: &TyEnv,
    references: &[ast::InterfaceRef],
) -> Result<Vec<InterfaceUse>, Diagnostic> {
    let mut bounds = Vec::with_capacity(references.len());
    for reference in references {
        let application = resolve_interface_use(ctx, env, reference)?;
        if bounds
            .iter()
            .any(|item: &InterfaceUse| item.interface == application.interface)
        {
            return Err(Diagnostic::new(
                "E1053",
                format!("duplicate interface bound `{}`", reference.name),
                reference.span,
            ));
        }
        bounds.push(application);
    }
    Ok(bounds)
}

/// Add one interface and all parent interfaces to one bound set.
fn expand_interface_application(
    ctx: &mut Ctx,
    base: TypeId,
    application: InterfaceUse,
    span: Span,
    visiting: &mut Vec<u32>,
    out: &mut Vec<InterfaceUse>,
) -> Result<(), Diagnostic> {
    if visiting.len() >= 128 {
        return Err(Diagnostic::new(
            "E1053",
            "interface inheritance exceeds 128 levels",
            span,
        ));
    }
    if visiting.contains(&application.interface) {
        return Err(Diagnostic::new(
            "E1053",
            "interface inheritance contains a cycle",
            span,
        ));
    }
    if let Some(existing) = out
        .iter()
        .find(|existing| existing.interface == application.interface)
    {
        if existing == &application {
            return Ok(());
        }
        return Err(Diagnostic::new(
            "E1053",
            format!(
                "interface `{}` has conflicting inherited arguments",
                ctx.interfaces[application.interface as usize].name
            ),
            span,
        ));
    }
    if ctx.interfaces[application.interface as usize]
        .parents
        .is_empty()
    {
        out.push(application);
        return Ok(());
    }
    out.push(application.clone());
    visiting.push(application.interface);
    let parents = ctx.interfaces[application.interface as usize]
        .parents
        .clone();
    let mut types = Vec::with_capacity(application.type_args.len() + 1);
    types.push(base);
    types.extend(application.type_args.iter().copied());
    for parent in parents {
        let parent = ctx.substitute_interface_use(&parent, &types, &application.row_args);
        expand_interface_application(ctx, base, parent, span, visiting, out)?;
    }
    visiting.pop();
    Ok(())
}

/// Expand direct bounds with their transitive parent interfaces.
fn expand_interface_bounds(
    ctx: &mut Ctx,
    base: TypeId,
    direct: Vec<InterfaceUse>,
    span: Span,
) -> Result<Vec<InterfaceUse>, Diagnostic> {
    if direct.iter().all(|application| {
        ctx.interfaces[application.interface as usize]
            .parents
            .is_empty()
    }) {
        return Ok(direct);
    }
    let mut out = Vec::with_capacity(direct.len());
    let mut visiting = Vec::new();
    for application in direct {
        expand_interface_application(ctx, base, application, span, &mut visiting, &mut out)?;
    }
    Ok(out)
}

fn type_uses_interface_self(store: &TypeStore, ty: TypeId, seen: &mut HashSet<TypeId>) -> bool {
    if !seen.insert(ty) {
        return false;
    }
    match store.get(ty) {
        Type::Var(0) => true,
        Type::Var(_) => false,
        Type::Inst(_, args) | Type::Tuple(args) => args
            .iter()
            .any(|item| type_uses_interface_self(store, *item, seen)),
        Type::List(item) | Type::Run(item) | Type::Wait(item) | Type::RunSnapshot(item) => {
            type_uses_interface_self(store, *item, seen)
        }
        Type::Map(key, value) | Type::PendingCall(key, value) | Type::Handle(key, value) => {
            type_uses_interface_self(store, *key, seen)
                || type_uses_interface_self(store, *value, seen)
        }
        Type::Fn(params, _, ret, _) | Type::Callback(params, _, ret, _) => {
            params
                .iter()
                .any(|item| type_uses_interface_self(store, *item, seen))
                || type_uses_interface_self(store, *ret, seen)
        }
        Type::Projection { .. } => false,
        Type::Op(_, callable) => type_uses_interface_self(store, *callable, seen),
        _ => false,
    }
}

fn interface_uses_self(ctx: &Ctx, interface: u32, visiting: &mut HashSet<u32>) -> bool {
    if !visiting.insert(interface) {
        return false;
    }
    let info = &ctx.interfaces[interface as usize];
    let direct = info.methods.iter().any(|method| {
        method
            .params
            .iter()
            .any(|ty| type_uses_interface_self(&ctx.store, *ty, &mut HashSet::new()))
            || type_uses_interface_self(&ctx.store, method.ret, &mut HashSet::new())
    });
    direct
        || info
            .parents
            .iter()
            .any(|parent| interface_uses_self(ctx, parent.interface, visiting))
}

fn interface_self_dependencies(ctx: &Ctx) -> Vec<bool> {
    (0..ctx.interfaces.len() as u32)
        .map(|interface| interface_uses_self(ctx, interface, &mut HashSet::new()))
        .collect()
}

/// Resolve bounds for type parameters in declaration order.
fn resolve_generic_bounds(
    ctx: &mut Ctx,
    env: &TyEnv,
    generics: &[ast::GenericParam],
) -> Result<Vec<Vec<InterfaceUse>>, Diagnostic> {
    let capacity = generics.iter().filter(|generic| !generic.is_effect).count();
    let mut out = Vec::with_capacity(capacity);
    let mut type_index = 0u32;
    for generic in generics {
        if !generic.is_effect {
            let direct = resolve_interface_bounds(ctx, env, &generic.bounds)?;
            let base = ctx.store.intern(Type::Var(env.type_offset + type_index));
            out.push(expand_interface_bounds(ctx, base, direct, generic.span)?);
            type_index += 1;
        }
    }
    Ok(out)
}

/// Resolve all interface contracts of one source module.
fn resolve_all_interfaces(
    ctx: &mut Ctx,
    module: &ast::Module,
    is_core: bool,
) -> Result<(), Diagnostic> {
    for declaration in &module.interfaces {
        let interface = if is_core {
            ctx.core_interfaces[&declaration.name]
        } else {
            ctx.user_interfaces[&declaration.name]
        };
        let (type_names, effect_names) = split_generics(&declaration.generics);
        if declaration.methods.len() > lm_bytecode::MAX_INTERFACE_CALL_INDEX as usize + 1 {
            return Err(Diagnostic::new(
                "E1024",
                "the interface has too many methods for compact calls",
                declaration.name_span,
            ));
        }
        let mut env = TyEnv {
            type_names: type_names.clone(),
            type_bounds: vec![Vec::new(); type_names.len()],
            extra_bounds: Vec::new(),
            effect_names: effect_names.clone(),
            type_offset: 1,
            self_interface: Some(interface),
            self_ty: Some(ctx.store.intern(Type::Var(0))),
            core_scope: is_core,
        };
        let bounds = resolve_generic_bounds(ctx, &env, &declaration.generics)?;
        env.type_bounds = bounds.clone();
        let parents = declaration
            .parents
            .iter()
            .map(|parent| resolve_interface_use(ctx, &env, parent))
            .collect::<Result<Vec<_>, _>>()?;
        if parents.iter().any(|parent| parent.interface == interface) {
            return Err(Diagnostic::new(
                "E1053",
                "an interface cannot extend itself",
                declaration.name_span,
            ));
        }
        let mut parent_ids = HashSet::new();
        if let Some(parent) = parents
            .iter()
            .find(|parent| !parent_ids.insert(parent.interface))
        {
            return Err(Diagnostic::new(
                "E1053",
                format!(
                    "duplicate parent interface `{}`",
                    ctx.interfaces[parent.interface as usize].name
                ),
                declaration.name_span,
            ));
        }
        ctx.interfaces[interface as usize].parents = parents;

        let mut associated = Vec::new();
        for item in &declaration.associated {
            if associated
                .iter()
                .any(|found: &AssociatedInfo| found.name == item.name)
            {
                return Err(Diagnostic::new(
                    "E1053",
                    format!("duplicate associated type `{}`", item.name),
                    item.name_span,
                ));
            }
            associated.push(AssociatedInfo {
                name: item.name.clone(),
                bounds: Vec::new(),
            });
        }
        ctx.interfaces[interface as usize].associated = associated;
        for (index, item) in declaration.associated.iter().enumerate() {
            let direct = resolve_interface_bounds(ctx, &env, &item.bounds)?;
            let self_ty = ctx.store.intern(Type::Var(0));
            let base = ctx
                .store
                .project(self_ty, InterfaceId(interface), index as u32);
            let bounds = expand_interface_bounds(ctx, base, direct, item.span)?;
            ctx.interfaces[interface as usize].associated[index].bounds = bounds;
        }

        let mut methods: Vec<Rc<InterfaceMethodSig>> = Vec::new();
        for method in &declaration.methods {
            if methods.iter().any(|found| found.name == method.name) {
                return Err(Diagnostic::new(
                    "E1053",
                    format!("duplicate interface method `{}`", method.name),
                    method.name_span,
                ));
            }
            let (own_type_params, own_effect_params) = split_generics(&method.generics);
            for name in own_type_params.iter().chain(&own_effect_params) {
                if env.type_names.contains(name) || env.effect_names.contains(name) {
                    return Err(Diagnostic::new(
                        "E1014",
                        format!("duplicate generic parameter name `{name}`"),
                        method.name_span,
                    ));
                }
            }
            let interface_type_count = env.type_names.len();
            let mut method_env = env.clone();
            method_env
                .type_names
                .extend(own_type_params.iter().cloned());
            method_env
                .type_bounds
                .extend(vec![Vec::new(); own_type_params.len()]);
            method_env
                .effect_names
                .extend(own_effect_params.iter().cloned());
            let mut own_type_bounds = Vec::with_capacity(own_type_params.len());
            let mut own_index = 0usize;
            for generic in &method.generics {
                if generic.is_effect {
                    continue;
                }
                let direct = resolve_interface_bounds(ctx, &method_env, &generic.bounds)?;
                let base = ctx.store.intern(Type::Var(
                    1 + interface_type_count as u32 + own_index as u32,
                ));
                own_type_bounds.push(expand_interface_bounds(ctx, base, direct, generic.span)?);
                own_index += 1;
            }
            method_env.type_bounds[interface_type_count..].clone_from_slice(&own_type_bounds);
            let mut premises = Vec::with_capacity(method.premises.len());
            for declaration in &method.premises {
                let subject = resolve_type(ctx, &method_env, &declaration.subject)?;
                let direct = resolve_interface_bounds(ctx, &method_env, &declaration.bounds)?;
                let bounds = expand_interface_bounds(ctx, subject, direct, declaration.span)?;
                premises.push(TypePremise { subject, bounds });
            }
            let mut params = Vec::with_capacity(method.params.len());
            let mut param_muts = Vec::with_capacity(method.params.len());
            let mut param_names = Vec::with_capacity(method.params.len());
            for param in &method.params {
                params.push(resolve_param_type(ctx, &method_env, param)?);
                param_muts.push(param.mutable);
                param_names.push(param.name.clone());
            }
            methods.push(Rc::new(InterfaceMethodSig {
                name: method.name.clone(),
                mut_self: method.mut_self,
                own_type_params,
                own_type_bounds,
                own_effect_params,
                premises,
                params,
                param_muts,
                param_names,
                ret: method
                    .ret
                    .as_ref()
                    .map(|ty| resolve_type(ctx, &method_env, ty))
                    .transpose()?
                    .unwrap_or(UNIT),
                row: resolve_row(ctx, &method_env, &method.row)?,
                default_func: None,
                default_binding: method
                    .body
                    .as_ref()
                    .map(|_| interface_default_binding(&declaration.name, &method.name)),
            }));
        }
        let method_index = index_interface_methods(&methods);
        let info = &mut ctx.interfaces[interface as usize];
        info.type_params = type_names;
        info.effect_params = effect_names;
        info.type_bounds = bounds;
        info.methods = methods;
        info.method_index = method_index;
    }
    for declaration in &module.interfaces {
        let interface = if is_core {
            ctx.core_interfaces[&declaration.name]
        } else {
            ctx.user_interfaces[&declaration.name]
        };
        let info = ctx.interfaces[interface as usize].clone();
        let base = ctx.store.intern(Type::Var(0));
        let application = InterfaceUse {
            interface,
            type_args: (0..info.type_params.len())
                .map(|index| ctx.store.intern(Type::Var(index as u32 + 1)))
                .collect(),
            row_args: (0..info.effect_params.len())
                .map(|index| vec![RowElem::Var(index as u32)])
                .collect(),
        };
        let mut closure = Vec::new();
        expand_interface_application(
            ctx,
            base,
            application,
            declaration.span,
            &mut Vec::new(),
            &mut closure,
        )?;
        let mut associated_names = HashMap::new();
        for application in &closure {
            for associated in &ctx.interfaces[application.interface as usize].associated {
                if let Some(previous) =
                    associated_names.insert(associated.name.clone(), application.interface)
                {
                    if previous != application.interface {
                        return Err(Diagnostic::new(
                            "E1053",
                            format!(
                                "inherited associated type `{}` is ambiguous",
                                associated.name
                            ),
                            declaration.span,
                        ));
                    }
                }
            }
        }
        let current_bounds = ctx.interfaces[interface as usize].type_bounds.clone();
        let normalized = current_bounds
            .into_iter()
            .enumerate()
            .map(|(index, bounds)| {
                let base = ctx.store.intern(Type::Var(index as u32 + 1));
                expand_interface_bounds(ctx, base, bounds, declaration.span)
            })
            .collect::<Result<Vec<_>, _>>()?;
        ctx.interfaces[interface as usize].type_bounds = normalized;
        let associated = ctx.interfaces[interface as usize].associated.clone();
        for (index, item) in associated.into_iter().enumerate() {
            let self_ty = ctx.store.intern(Type::Var(0));
            let base = ctx
                .store
                .project(self_ty, InterfaceId(interface), index as u32);
            let bounds = expand_interface_bounds(ctx, base, item.bounds, declaration.span)?;
            ctx.interfaces[interface as usize].associated[index].bounds = bounds;
        }
    }
    Ok(())
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
                   kind: ClassKind,
                   is_final: bool|
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
        if is_final {
            ctx.store.set_class_final(id);
        }
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
        let idx = declare(
            ctx,
            &class.name,
            class.name_span,
            type_names.len() as u32,
            ClassKind::Normal,
            class.is_final,
        )?;
        if is_core {
            register_core_native_class(&mut ctx.store, &class.name, ClassId(idx));
        }
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
            false,
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
                false,
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
                ..TyEnv::default()
            };
            let parent = ctx.lookup_type(pname, &env).ok_or_else(|| {
                Diagnostic::new("E1038", format!("unknown parent class `{pname}`"), *pspan)
            })?;
            let parent_meta = ctx.store.class_meta(ClassId(parent)).clone();
            if !is_core && pname == "Text" && ctx.core_types.get("Text") == Some(&parent) {
                return Err(Diagnostic::new(
                    "E1040",
                    "`Text` is sealed and permits only core text classes",
                    *pspan,
                ));
            }
            if !is_core
                && pname == "TcpResource"
                && ctx.core_types.get("TcpResource") == Some(&parent)
            {
                return Err(Diagnostic::new(
                    "E1040",
                    "`TcpResource` is sealed and permits only core TCP classes",
                    *pspan,
                ));
            }
            let opaque_syntax_parent = ["SyntaxElement", "SyntaxNode"]
                .iter()
                .any(|name| ctx.core_types.get(*name) == Some(&parent));
            if !is_core && opaque_syntax_parent {
                return Err(Diagnostic::new(
                    "E1040",
                    format!("`{pname}` is sealed and permits only core syntax classes"),
                    *pspan,
                ));
            }
            if parent_meta.is_final {
                return Err(Diagnostic::new(
                    "E1040",
                    format!("`{pname}` is final and cannot be a parent"),
                    *pspan,
                ));
            }
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
    let (ptys, muts, names) = resolve_parameters(ctx, env, params, self_ty)?;
    let ret = match ret {
        Some(ty) => resolve_type(ctx, env, ty)?,
        None => UNIT,
    };
    let row = resolve_row(ctx, env, row)?;
    Ok(FnSig {
        type_params,
        type_bounds: env.type_bounds.clone(),
        effect_params,
        params: ptys,
        param_muts: muts,
        param_names: names,
        ret,
        row,
    })
}

type ResolvedParameters = (Vec<TypeId>, Vec<bool>, Vec<String>);

fn resolve_parameters(
    ctx: &mut Ctx,
    env: &TyEnv,
    params: &[ast::Param],
    self_ty: Option<(TypeId, bool)>,
) -> Result<ResolvedParameters, Diagnostic> {
    let capacity = params.len() + usize::from(self_ty.is_some());
    let mut ptys = Vec::with_capacity(capacity);
    let mut muts = Vec::with_capacity(capacity);
    let mut names = Vec::with_capacity(capacity);
    if let Some((ty, mutable)) = self_ty {
        ptys.push(ty);
        muts.push(mutable);
        names.push("self".to_string());
    }
    let mut seen: Vec<&str> = Vec::with_capacity(params.len());
    for param in params {
        if seen.contains(&param.name.as_str()) {
            return Err(Diagnostic::new(
                "E1014",
                format!("duplicate parameter name `{}`", param.name),
                param.span,
            ));
        }
        seen.push(&param.name);
        ptys.push(resolve_param_type(ctx, env, param)?);
        muts.push(param.mutable);
        names.push(param.name.clone());
    }
    Ok((ptys, muts, names))
}

/// Resolve one parameter and apply its escape rule.
pub(crate) fn resolve_param_type(
    ctx: &mut Ctx,
    env: &TyEnv,
    param: &ast::Param,
) -> Result<TypeId, Diagnostic> {
    let ty = resolve_type(ctx, env, &param.ty)?;
    match (param.escaping, ctx.store.get(ty).clone()) {
        (false, Type::Fn(params, muts, ret, row)) => {
            Ok(ctx.store.intern_callback(params, muts, ret, row))
        }
        (true, Type::Fn(..)) => Ok(ty),
        (true, _) => Err(Diagnostic::new(
            "E1064",
            "an `escaping` parameter must have a function type",
            param.ty.span,
        )),
        (false, _) => Ok(ty),
    }
}

/// Resolve one method declaration into a signature and reserve its
/// function index.
fn resolve_method_sig(
    ctx: &mut Ctx,
    method: &ast::MethodDef,
    class_type_names: &[String],
    class_type_bounds: &[Vec<InterfaceUse>],
    self_ty: TypeId,
    is_core: bool,
    is_init: bool,
) -> Result<Rc<MethodSig>, Diagnostic> {
    let (own_type, own_effect) = split_generics(&method.generics);
    if is_init && (!own_type.is_empty() || !own_effect.is_empty()) {
        return Err(Diagnostic::new(
            "E1038",
            "`init` cannot declare generic parameters",
            method.name_span,
        ));
    }
    if is_init && !method.premises.is_empty() {
        return Err(Diagnostic::new(
            "E1038",
            "`init` cannot declare type premises",
            method.name_span,
        ));
    }
    let mut type_names = Vec::with_capacity(class_type_names.len() + own_type.len());
    type_names.extend_from_slice(class_type_names);
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
    let mut env = TyEnv {
        type_names,
        type_bounds: {
            let mut bounds = class_type_bounds.to_vec();
            bounds.extend(vec![Vec::new(); own_type.len()]);
            bounds
        },
        extra_bounds: Vec::new(),
        effect_names: own_effect.clone(),
        type_offset: 0,
        self_interface: None,
        self_ty: Some(self_ty),
        core_scope: is_core,
    };
    let own_type_bounds = resolve_generic_bounds(ctx, &env, &method.generics)?;
    let class_count = class_type_names.len();
    env.type_bounds[class_count..].clone_from_slice(&own_type_bounds);
    let premises = resolve_conformance_premises(ctx, &env, &method.premises)?;
    add_premises(&mut env, &premises);
    let (params, param_muts, param_names) = resolve_parameters(ctx, &env, &method.params, None)?;
    let ret = if is_init {
        UNIT
    } else {
        method
            .ret
            .as_ref()
            .map(|ty| resolve_type(ctx, &env, ty))
            .transpose()?
            .unwrap_or(UNIT)
    };
    let row = resolve_row(ctx, &env, &method.row)?;
    let func = ctx.funcs.len() as u32;
    let own_type_bounds = env.type_bounds.split_off(class_count);
    let class_type_bounds = env.type_bounds;
    let method_sig = MethodSig {
        name: method.name.clone(),
        func,
        mut_self: method.mut_self,
        params,
        param_muts,
        param_names,
        ret,
        row,
        class_type_bounds,
        own_type_params: own_type,
        own_type_bounds,
        own_effect_params: own_effect,
    };
    ctx.sigs.push(FnSig {
        type_params: Vec::new(),
        type_bounds: Vec::new(),
        effect_params: Vec::new(),
        params: Vec::new(),
        param_muts: Vec::new(),
        param_names: Vec::new(),
        ret: UNIT,
        row: Vec::new(),
    });
    ctx.funcs.push(None);
    Ok(Rc::new(method_sig))
}

/// Resolve the associated bindings of one class conformance list.
fn resolve_conformance_shapes(
    ctx: &mut Ctx,
    references: &[ast::ConformanceRef],
    associated_bindings: &[ast::AssociatedType],
    class_id: u32,
    self_ty: TypeId,
    env: &TyEnv,
) -> Result<Vec<Rc<ConformanceInfo>>, Diagnostic> {
    let mut bindings: HashMap<&str, (&ast::TypeExpr, Span)> = HashMap::new();
    for item in associated_bindings {
        let value = item.value.as_ref().expect("the parser requires a value");
        if bindings
            .insert(&item.name, (value, item.name_span))
            .is_some()
        {
            return Err(Diagnostic::new(
                "E1053",
                format!("duplicate associated type binding `{}`", item.name),
                item.name_span,
            ));
        }
    }
    let mut used: Vec<String> = Vec::new();
    let mut conformances = Vec::new();
    for reference in references {
        let mut premises = resolve_conformance_premises(ctx, env, &reference.premises)?;
        let direct = resolve_interface_use(ctx, env, &reference.application)?;
        let mut closure = Vec::new();
        expand_interface_application(
            ctx,
            self_ty,
            direct,
            reference.application.span,
            &mut Vec::new(),
            &mut closure,
        )?;
        let closure_len = closure.len();
        for (position, application) in closure.into_iter().enumerate() {
            if let Some(position) = conformances.iter().position(|item: &Rc<ConformanceInfo>| {
                item.application.interface == application.interface
            }) {
                let existing = &conformances[position];
                if existing.application != application {
                    return Err(Diagnostic::new(
                        "E1053",
                        format!(
                            "interface `{}` has conflicting conformance arguments",
                            ctx.interfaces[application.interface as usize].name
                        ),
                        reference.span,
                    ));
                }
                if premises_imply(&premises, &existing.premises) {
                    continue;
                }
                if !premises_imply(&existing.premises, &premises) {
                    return Err(Diagnostic::new(
                        "E1053",
                        format!(
                            "interface `{}` has incomparable conformance premises",
                            ctx.interfaces[application.interface as usize].name
                        ),
                        reference.span,
                    ));
                }
                conformances.remove(position);
            }
            let names: Vec<String> = ctx.interfaces[application.interface as usize]
                .associated
                .iter()
                .map(|item| item.name.clone())
                .collect();
            let mut associated = Vec::new();
            for name in names {
                let Some((value, _)) = bindings.get(name.as_str()).copied() else {
                    return Err(Diagnostic::new(
                        "E1053",
                        format!("the conformance needs `type {name} = ...`"),
                        reference.span,
                    ));
                };
                associated.push(resolve_type(ctx, env, value)?);
                used.push(name);
            }
            ctx.store.set_conformance(
                ClassId(class_id),
                InterfaceId(application.interface),
                associated.clone(),
            );
            conformances.push(Rc::new(ConformanceInfo {
                application,
                premises: if position + 1 == closure_len {
                    std::mem::take(&mut premises)
                } else {
                    premises.clone()
                },
                associated,
                method_overrides: Vec::new(),
            }));
        }
    }
    for (name, (_, span)) in bindings {
        if !used.iter().any(|item| item == name) {
            return Err(Diagnostic::new(
                "E1053",
                format!("the associated type `{name}` belongs to no conformance"),
                span,
            ));
        }
    }
    Ok(conformances)
}

/// Resolve class type parameter premises and their inherited bounds.
fn resolve_conformance_premises(
    ctx: &mut Ctx,
    env: &TyEnv,
    declarations: &[ast::GenericParam],
) -> Result<Vec<ConformancePremise>, Diagnostic> {
    let mut premises = Vec::with_capacity(declarations.len());
    let mut next_param = 0usize;
    let mut ordered = true;
    for declaration in declarations {
        let param = if env.type_names.get(next_param) == Some(&declaration.name) {
            next_param
        } else if let Some(found) = env
            .type_names
            .iter()
            .position(|name| name == &declaration.name)
        {
            found
        } else {
            return Err(Diagnostic::new(
                "E1053",
                format!(
                    "the conformance premise names unknown type parameter `{}`",
                    declaration.name
                ),
                declaration.span,
            ));
        };
        next_param = param + 1;
        if premises
            .last()
            .is_some_and(|previous: &ConformancePremise| previous.param >= param as u32)
        {
            ordered = false;
        }
        let direct = resolve_interface_bounds(ctx, env, &declaration.bounds)?;
        let base = ctx.store.intern(Type::Var(env.type_offset + param as u32));
        let bounds = expand_interface_bounds(ctx, base, direct, declaration.span)?;
        premises.push(ConformancePremise {
            param: param as u32,
            bounds,
        });
    }
    if !ordered {
        premises.sort_by_key(|premise| premise.param);
    }
    Ok(premises)
}

/// Test whether one premise set provides every bound in another set.
fn premises_imply(left: &[ConformancePremise], right: &[ConformancePremise]) -> bool {
    right.iter().all(|required| {
        left.iter()
            .find(|candidate| candidate.param == required.param)
            .is_some_and(|candidate| {
                required
                    .bounds
                    .iter()
                    .all(|bound| candidate.bounds.contains(bound))
            })
    })
}

/// Test whether one bound table provides every required bound.
fn bounds_imply(available: &[Vec<InterfaceUse>], required: &[Vec<InterfaceUse>]) -> bool {
    required.iter().enumerate().all(|(index, bounds)| {
        let Some(actual) = available.get(index) else {
            return false;
        };
        bounds.iter().all(|bound| actual.contains(bound))
    })
}

fn add_premises(env: &mut TyEnv, premises: &[ConformancePremise]) {
    for premise in premises {
        let Some(bounds) = env.type_bounds.get_mut(premise.param as usize) else {
            continue;
        };
        for bound in &premise.bounds {
            if !bounds.contains(bound) {
                bounds.push(bound.clone());
            }
        }
    }
}

/// Return the reason that one class method cannot override one requirement.
fn interface_override_error(
    ctx: &mut Ctx,
    env: &TyEnv,
    class_id: u32,
    method: &MethodSig,
    requirement: &InterfaceMethodSig,
    types: &[TypeId],
    rows: &[Row],
) -> Option<String> {
    if method.own_type_params.len() != requirement.own_type_params.len()
        || method.own_effect_params.len() != requirement.own_effect_params.len()
    {
        return Some("the generic parameter counts differ".to_string());
    }
    if requirement.own_type_params.is_empty()
        && requirement.own_effect_params.is_empty()
        && requirement.premises.is_empty()
    {
        if !bounds_imply(&env.type_bounds, &method.class_type_bounds) {
            return Some("the method needs an undeclared premise".to_string());
        }
        if method.mut_self != requirement.mut_self {
            let required = if requirement.mut_self {
                "`mut self`"
            } else {
                "`self`"
            };
            return Some(format!("the contract requires {required}"));
        }
        let parameters_match = method.params.len() == requirement.params.len()
            && method.param_muts == requirement.param_muts
            && method
                .params
                .iter()
                .zip(&requirement.params)
                .zip(&requirement.param_muts)
                .all(|((implementation, required), mutable)| {
                    let required = ctx.store.substitute(*required, types, rows);
                    let required = ctx.normalize_associated(env, required);
                    if *mutable {
                        *implementation == required
                    } else {
                        ctx.store.compatible(*implementation, required)
                    }
                });
        let required_ret = ctx.store.substitute(requirement.ret, types, rows);
        let required_ret = ctx.normalize_associated(env, required_ret);
        if !parameters_match || !ctx.store.compatible(required_ret, method.ret) {
            return Some("the parameter or result types differ".to_string());
        }
        let required_row = ctx.store.substitute_row(&requirement.row, rows);
        if !ctx.store.row_included(&method.row, &required_row) {
            return Some("the effect row is too wide".to_string());
        }
        return None;
    }
    let class_count = ctx.classes[class_id as usize].type_params.len();
    let own_types: Vec<TypeId> = (0..requirement.own_type_params.len())
        .map(|index| ctx.store.intern(Type::Var((class_count + index) as u32)))
        .collect();
    let mut required_types = types.to_vec();
    required_types.extend(own_types.iter().copied());
    let own_rows: Vec<Row> = (0..requirement.own_effect_params.len())
        .map(|index| vec![RowElem::Var(index as u32)])
        .collect();
    let mut required_rows = rows.to_vec();
    required_rows.extend(own_rows.iter().cloned());
    let mut method_env = env.clone();
    for premise in &requirement.premises {
        let subject = ctx
            .store
            .substitute(premise.subject, &required_types, &required_rows);
        let subject = ctx.normalize_associated(&method_env, subject);
        let bounds: Vec<InterfaceUse> = premise
            .bounds
            .iter()
            .map(|bound| ctx.substitute_interface_use(bound, &required_types, &required_rows))
            .collect();
        match ctx.store.get(subject) {
            Type::Var(index) if *index >= method_env.type_offset => {
                if let Some(target) = method_env
                    .type_bounds
                    .get_mut((*index - method_env.type_offset) as usize)
                {
                    for bound in bounds {
                        if !target.contains(&bound) {
                            target.push(bound);
                        }
                    }
                }
            }
            _ => method_env
                .extra_bounds
                .push(TypePremise { subject, bounds }),
        }
    }
    if !bounds_imply(&method_env.type_bounds, &method.class_type_bounds) {
        return Some("the method needs an undeclared premise".to_string());
    }
    let required_params: Vec<TypeId> = requirement
        .params
        .iter()
        .map(|item| {
            let ty = ctx.store.substitute(*item, &required_types, &required_rows);
            ctx.normalize_associated(&method_env, ty)
        })
        .collect();
    let required_ret = ctx
        .store
        .substitute(requirement.ret, &required_types, &required_rows);
    let required_ret = ctx.normalize_associated(&method_env, required_ret);
    let required_row = ctx.store.substitute_row(&requirement.row, &required_rows);
    if method.mut_self != requirement.mut_self {
        let required = if requirement.mut_self {
            "`mut self`"
        } else {
            "`self`"
        };
        return Some(format!("the contract requires {required}"));
    }
    let parameters_match = method.params.len() == required_params.len()
        && method
            .params
            .iter()
            .zip(&required_params)
            .zip(&requirement.param_muts)
            .all(|((implementation, required), mutable)| {
                if *mutable {
                    implementation == required
                } else {
                    ctx.store.compatible(*implementation, *required)
                }
            });
    let same_shape = parameters_match
        && method.param_muts == requirement.param_muts
        && method.own_type_bounds.len() == requirement.own_type_bounds.len()
        && method
            .own_type_bounds
            .iter()
            .zip(&requirement.own_type_bounds)
            .all(|(actual, required)| {
                actual.len() == required.len()
                    && required.iter().all(|bound| {
                        let bound =
                            ctx.substitute_interface_use(bound, &required_types, &required_rows);
                        actual.contains(&bound)
                    })
            });
    if !same_shape || !ctx.store.compatible(required_ret, method.ret) {
        return Some("the parameter or result types differ".to_string());
    }
    if !ctx.store.row_included(&method.row, &required_row) {
        return Some("the effect row is too wide".to_string());
    }
    None
}

/// Check every method and associated bound of one class conformance.
fn check_class_conformances(
    ctx: &mut Ctx,
    declaration_span: Span,
    class_id: u32,
    is_core: bool,
    self_dependent_interfaces: &[bool],
) -> Result<(), Diagnostic> {
    let info_type_names = ctx.classes[class_id as usize].type_params.clone();
    let mut env = TyEnv {
        type_names: info_type_names,
        type_bounds: ctx.classes[class_id as usize].type_bounds.clone(),
        extra_bounds: Vec::new(),
        effect_names: Vec::new(),
        type_offset: 0,
        self_interface: None,
        self_ty: Some(ctx.classes[class_id as usize].self_ty),
        core_scope: is_core,
    };
    let base_bound_lengths: Vec<usize> = env.type_bounds.iter().map(Vec::len).collect();
    let self_ty = ctx.classes[class_id as usize].self_ty;
    let conformances = ctx.classes[class_id as usize].conformances.clone();
    let mut selected_defaults: HashMap<String, (String, String)> = HashMap::new();
    for (conformance_index, conformance) in conformances.into_iter().enumerate() {
        add_premises(&mut env, &conformance.premises);
        let contract = ctx.interfaces[conformance.application.interface as usize].clone();
        let class_info = &ctx.classes[class_id as usize];
        let closed_native_family = class_info.native_repr == Some(NativeRepr::Text);
        if class_info.kind == ClassKind::Normal
            && !class_info.is_final
            && !closed_native_family
            && self_dependent_interfaces[conformance.application.interface as usize]
        {
            return Err(Diagnostic::new(
                "E1053",
                format!(
                    "a non-final class cannot conform to Self-dependent interface `{}`",
                    contract.name
                ),
                declaration_span,
            ));
        }
        let mut types = vec![self_ty];
        types.extend(conformance.application.type_args.iter().copied());
        let rows = conformance.application.row_args.clone();
        if !ctx.interface_arguments_meet_bounds(&env, self_ty, &conformance.application) {
            return Err(Diagnostic::new(
                "E1053",
                format!(
                    "the conformance arguments do not meet interface `{}` bounds",
                    contract.name
                ),
                declaration_span,
            ));
        }
        for (index, associated) in contract.associated.iter().enumerate() {
            for bound in &associated.bounds {
                let required = ctx.substitute_interface_use(bound, &types, &rows);
                let actual = conformance.associated[index];
                if !ctx.type_conforms(&env, actual, &required) {
                    return Err(Diagnostic::new(
                        "E1053",
                        format!(
                            "the associated type `{}` does not conform to `{}`",
                            associated.name, ctx.interfaces[required.interface as usize].name
                        ),
                        declaration_span,
                    ));
                }
            }
        }
        if conformance.application.interface == ctx.core_interfaces["Iterable"] {
            let iterator = ctx.core_interfaces["Iterator"];
            let item_index = contract
                .associated
                .iter()
                .position(|item| item.name == "Item")
                .expect("the core Iterable interface declares Item");
            let iter_index = contract
                .associated
                .iter()
                .position(|item| item.name == "Iter")
                .expect("the core Iterable interface declares Iter");
            let iterator_item = ctx.interfaces[iterator as usize]
                .associated
                .iter()
                .position(|item| item.name == "Item")
                .expect("the core Iterator interface declares Item")
                as u32;
            let item = conformance.associated[item_index];
            let iter = conformance.associated[iter_index];
            let actual = ctx
                .store
                .project(iter, InterfaceId(iterator), iterator_item);
            if actual != item {
                return Err(Diagnostic::new(
                    "E1053",
                    "Iterable.Item must equal Iterable.Iter.Item",
                    declaration_span,
                ));
            }
        }
        let mut method_overrides = Vec::with_capacity(contract.methods.len());
        for requirement in &contract.methods {
            let candidate = ctx.find_method_owner(class_id, &requirement.name);
            let mismatch = candidate.as_ref().and_then(|(method, _, _)| {
                interface_override_error(ctx, &env, class_id, method, requirement, &types, &rows)
            });
            let selected = candidate.is_some() && mismatch.is_none();
            if !selected {
                let Some(binding) = &requirement.default_binding else {
                    let reason = mismatch.unwrap_or_else(|| "the implementation is missing".into());
                    return Err(Diagnostic::new(
                        "E1053",
                        format!(
                            "the method `{}` does not satisfy interface `{}`: {reason}",
                            requirement.name, contract.name
                        ),
                        declaration_span,
                    ));
                };
                if let Some((seen_binding, seen_interface)) =
                    selected_defaults.get(&requirement.name)
                {
                    if seen_binding != binding {
                        return Err(Diagnostic::new(
                            "E1053",
                            format!(
                                "the method `{}` has defaults from `{seen_interface}` and `{}`; add an explicit override",
                                requirement.name, contract.name
                            ),
                            declaration_span,
                        ));
                    }
                } else {
                    selected_defaults.insert(
                        requirement.name.clone(),
                        (binding.clone(), contract.name.clone()),
                    );
                }
            }
            method_overrides.push(selected);
        }
        Rc::make_mut(&mut ctx.classes[class_id as usize].conformances[conformance_index])
            .method_overrides = method_overrides;
        for (bounds, base_len) in env.type_bounds.iter_mut().zip(&base_bound_lengths) {
            bounds.truncate(*base_len);
        }
    }
    Ok(())
}

/// Check all class conformances after every class signature resolves.
fn check_all_conformances(
    ctx: &mut Ctx,
    module: &ast::Module,
    is_core: bool,
    self_dependent_interfaces: &[bool],
) -> Result<(), Diagnostic> {
    for class in &module.classes {
        let class_id = if is_core {
            ctx.core_types[&class.name]
        } else {
            ctx.user_types[&class.name]
        };
        check_class_conformances(
            ctx,
            class.name_span,
            class_id,
            is_core,
            self_dependent_interfaces,
        )?;
    }
    for enum_def in &module.enums {
        let class_id = if is_core {
            ctx.core_types[&enum_def.name]
        } else {
            ctx.user_types[&enum_def.name]
        };
        check_class_conformances(
            ctx,
            enum_def.name_span,
            class_id,
            is_core,
            self_dependent_interfaces,
        )?;
    }
    Ok(())
}

/// Resolve one class declaration: layout, methods, and `init`.
/// Validate the static storage rules of local frozen classes.
fn check_frozen_classes(ctx: &Ctx, module: &ast::Module, is_core: bool) -> Result<(), Diagnostic> {
    for class in &module.classes {
        if !class.is_frozen {
            continue;
        }
        if let Some(parent) = &class.parent {
            return Err(Diagnostic::new(
                "E1038",
                "a frozen class cannot declare a parent",
                parent.span,
            ));
        }
        let classes = if is_core {
            &ctx.core_types
        } else {
            &ctx.user_types
        };
        let info = &ctx.classes[classes[&class.name] as usize];
        for (offset, field) in class.fields.iter().enumerate() {
            let ty = info.field_tys[info.own_start + offset];
            if !ctx.type_always_frozen(ty, true) {
                return Err(Diagnostic::new(
                    "E1038",
                    format!(
                        "the frozen class field `{}` has a type that is not always frozen",
                        field.name
                    ),
                    field.span,
                ));
            }
        }
    }
    Ok(())
}

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
    let native_repr = is_core.then(|| core_native_repr(&class.name)).flatten();
    let text_parent = ctx.core_types.get("Text").copied();
    let tcp_parent = ctx.core_types.get("TcpResource").copied();
    let valid_native_layout = match native_repr {
        Some(NativeRepr::Text) => !class.is_final && parent.is_none(),
        Some(NativeRepr::String | NativeRepr::Substring) => class.is_final && parent == text_parent,
        Some(NativeRepr::TcpResource) => !class.is_final && parent.is_none(),
        Some(NativeRepr::TcpStream | NativeRepr::TcpListener) => {
            class.is_final && parent == tcp_parent
        }
        Some(NativeRepr::TlsStream | NativeRepr::UdpSocket) => class.is_final && parent.is_none(),
        Some(_) => class.is_final && parent.is_none(),
        None => true,
    };
    let valid_native_arity = match native_repr {
        Some(NativeRepr::List) => type_names.len() == 1,
        Some(NativeRepr::Tuple(arity)) => type_names.len() == arity as usize,
        Some(
            NativeRepr::Map
            | NativeRepr::FunctionCode
            | NativeRepr::FunctionDef
            | NativeRepr::FunctionBinding,
        ) => type_names.len() == 2,
        Some(_) => type_names.is_empty(),
        None => true,
    };
    let valid_native_shape = native_repr.is_none()
        || valid_native_layout
            && valid_native_arity
            && class.fields.is_empty()
            && !class.methods.iter().any(|method| method.name == "init");
    if !valid_native_shape {
        return Err(Diagnostic::new(
            "E1040",
            format!(
                "native core class `{}` has an invalid inheritance or state layout",
                class.name
            ),
            class.span,
        ));
    }
    let self_ty = class_self_type(
        &mut ctx.store,
        ClassId(idx),
        type_names.len() as u32,
        native_repr,
    );
    let mut env = TyEnv {
        type_names: type_names.clone(),
        type_bounds: vec![Vec::new(); type_names.len()],
        extra_bounds: Vec::new(),
        effect_names: vec![],
        type_offset: 0,
        self_interface: None,
        self_ty: Some(self_ty),
        core_scope: is_core,
    };
    let type_bounds = resolve_generic_bounds(ctx, &env, &class.generics)?;
    env.type_bounds = type_bounds.clone();
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
    let mut methods: Vec<Rc<MethodSig>> = Vec::new();
    let mut init: Option<Rc<MethodSig>> = None;
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
            let msig = resolve_method_sig(
                ctx,
                method,
                &type_names,
                &type_bounds,
                self_ty,
                is_core,
                true,
            )?;
            init = Some(msig);
            continue;
        }
        if class.is_frozen && method.mut_self {
            return Err(Diagnostic::new(
                "E1038",
                "a frozen class permits `mut self` only in `init`",
                method.name_span,
            ));
        }
        let msig = resolve_method_sig(
            ctx,
            method,
            &type_names,
            &type_bounds,
            self_ty,
            is_core,
            false,
        )?;
        // Override compatibility with the nearest ancestor method. The
        // inherited signature is read in the subclass view, so a
        // generic parent compares with its arguments applied.
        if let Some(p) = parent {
            let inherited = ctx
                .lookup_method(p, parent_args.clone(), type_names.len(), &method.name)
                .map(|(sig, _, _)| sig);
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
                            display_row_or_empty(&ctx.store, &base.own_effect_params, &base.row)
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
    let conformances = resolve_conformance_shapes(
        ctx,
        &class.interfaces,
        &class.associated,
        idx,
        self_ty,
        &env,
    )?;
    let method_index = index_methods(&methods);
    Ok(ClassInfo {
        imported: false,
        source_span: (!is_core).then_some(class.span),
        is_final: class.is_final,
        is_frozen: class.is_frozen,
        native_repr,
        name: class.name.clone(),
        parent,
        type_params: type_names,
        type_bounds,
        conformances,
        kind: ClassKind::Normal,
        self_ty,
        field_names,
        field_tys,
        has_default,
        own_start,
        methods,
        method_index,
        init,
        family: None,
        arms: Vec::new(),
        arm_short: String::new(),
    })
}

fn display_row_or_empty(store: &TypeStore, names: &[String], row: &Row) -> String {
    if row.is_empty() {
        "empty".to_string()
    } else {
        store.display_row_with_names(row, &|index| names.get(index as usize).cloned())
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
    let mut env = TyEnv {
        type_names: type_names.clone(),
        type_bounds: vec![Vec::new(); type_names.len()],
        extra_bounds: Vec::new(),
        effect_names: vec![],
        type_offset: 0,
        self_interface: None,
        self_ty: Some(self_ty),
        core_scope: is_core,
    };
    let type_bounds = resolve_generic_bounds(ctx, &env, &enum_def.generics)?;
    env.type_bounds = type_bounds.clone();
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
    let mut methods: Vec<Rc<MethodSig>> = Vec::new();
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
        if methods
            .iter()
            .any(|signature| signature.name == method.name)
        {
            return Err(Diagnostic::new(
                "E1038",
                format!("the enum already has a member named `{}`", method.name),
                method.name_span,
            ));
        }
        let msig = resolve_method_sig(
            ctx,
            method,
            &type_names,
            &type_bounds,
            self_ty,
            is_core,
            false,
        )?;
        methods.push(msig);
    }
    let arms: Vec<u32> = arm_infos.iter().map(|(idx, _, _, _)| *idx).collect();
    let method_index = index_methods(&methods);
    ctx.classes[parent_idx as usize] = ClassInfo {
        imported: false,
        source_span: (!is_core).then_some(enum_def.span),
        is_final: false,
        is_frozen: false,
        native_repr: None,
        name: enum_def.name.clone(),
        parent: None,
        type_params: type_names.clone(),
        type_bounds: type_bounds.clone(),
        conformances: Vec::new(),
        kind: ClassKind::EnumParent,
        self_ty,
        field_names: Vec::new(),
        field_tys: Vec::new(),
        has_default: Vec::new(),
        own_start: 0,
        methods,
        method_index,
        init: None,
        family: None,
        arms,
        arm_short: String::new(),
    };
    let conformances = resolve_conformance_shapes(
        ctx,
        &enum_def.interfaces,
        &enum_def.associated,
        parent_idx,
        self_ty,
        &env,
    )?;
    ctx.classes[parent_idx as usize].conformances = conformances;
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
            source_span: (!is_core).then_some(enum_def.span),
            is_final: false,
            is_frozen: false,
            native_repr: None,
            name: format!("{}.{}", enum_def.name, short),
            parent: Some(parent_idx),
            type_params: type_names.clone(),
            type_bounds: type_bounds.clone(),
            conformances: Vec::new(),
            kind: ClassKind::EnumCase,
            self_ty: arm_self_ty,
            field_names,
            field_tys,
            has_default: vec![false; count],
            own_start: 0,
            methods: Vec::new(),
            method_index: Vec::new(),
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
                        type_bounds: ctx.classes[idx].type_bounds.clone(),
                        extra_bounds: Vec::new(),
                        effect_names: vec![],
                        type_offset: 0,
                        self_interface: None,
                        self_ty: Some(ctx.classes[idx].self_ty),
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
        let mut ordinary = 0;
        for method in &class.methods {
            let signature = if method.name == "init" {
                Rc::clone(
                    ctx.classes[cidx as usize]
                        .init
                        .as_ref()
                        .expect("init resolved"),
                )
            } else {
                let signature = &ctx.classes[cidx as usize].methods[ordinary];
                debug_assert_eq!(signature.name, method.name);
                ordinary += 1;
                Rc::clone(signature)
            };
            check_method(ctx, cidx, signature, method, is_core)?;
        }
    }
    for enum_def in &module.enums {
        let map = if is_core {
            &ctx.core_types
        } else {
            &ctx.user_types
        };
        let cidx = map[&enum_def.name];
        for (index, method) in enum_def.methods.iter().enumerate() {
            let signature = Rc::clone(&ctx.classes[cidx as usize].methods[index]);
            debug_assert_eq!(signature.name, method.name);
            check_method(ctx, cidx, signature, method, is_core)?;
        }
    }
    Ok(())
}

/// Check each interface default as one interface-owned function.
fn check_interface_defaults(
    ctx: &mut Ctx,
    module: &ast::Module,
    is_core: bool,
) -> Result<(), Diagnostic> {
    for declaration in &module.interfaces {
        let interface = if is_core {
            ctx.core_interfaces[&declaration.name]
        } else {
            ctx.user_interfaces[&declaration.name]
        };
        for (index, method) in declaration.methods.iter().enumerate() {
            let Some(body) = &method.body else {
                continue;
            };
            let requirement = Rc::clone(&ctx.interfaces[interface as usize].methods[index]);
            let func = requirement
                .default_func
                .expect("each local default reserves a function");
            let mut sig = ctx.sigs[func as usize].clone();
            let type_param_count = sig.type_params.len() as u32;
            let effect_param_count = sig.effect_params.len() as u32;
            let self_ty = ctx.store.intern(Type::Var(0));
            let env = TyEnv {
                type_names: std::mem::take(&mut sig.type_params),
                type_bounds: std::mem::take(&mut sig.type_bounds),
                extra_bounds: requirement.premises.clone(),
                effect_names: std::mem::take(&mut sig.effect_params),
                type_offset: 0,
                self_interface: Some(interface),
                self_ty: Some(self_ty),
                core_scope: is_core,
            };
            let mut checker = FnChecker::top_level(RetKind::Known(sig.ret), env, sig.row.clone());
            checker.reserve_parameters(sig.params.len());
            for (slot, ((ty, mutable), name)) in sig
                .params
                .iter()
                .zip(&sig.param_muts)
                .zip(&sig.param_names)
                .enumerate()
            {
                checker.locals.push((*ty, *mutable));
                if checker.scopes[0]
                    .insert(name.clone(), slot as u32)
                    .is_some()
                {
                    return Err(Diagnostic::new(
                        "E1014",
                        format!("duplicate parameter name `{name}`"),
                        method.span,
                    ));
                }
            }
            let checked = checker.check_callable(ctx, body, sig.ret, method.span)?;
            ctx.funcs[func as usize] = Some(HirFunc {
                imported: false,
                core: is_core,
                source_span: (!is_core).then_some(method.span),
                name: requirement
                    .default_binding
                    .clone()
                    .expect("each default has one hidden binding"),
                type_params: type_param_count,
                type_bounds: into_hir_bounds(checked.type_bounds),
                effect_params: effect_param_count,
                params: sig.params,
                param_muts: sig.param_muts,
                param_names: sig.param_names,
                ret: sig.ret,
                row: sig.row,
                captures: vec![],
                locals: checked.locals,
                body: checked.body,
            });
        }
    }
    Ok(())
}

/// Check one method body, with constructor tracking for `init`.
fn check_method(
    ctx: &mut Ctx,
    cidx: u32,
    sig: Rc<MethodSig>,
    method: &ast::MethodDef,
    is_core: bool,
) -> Result<(), Diagnostic> {
    let is_init = method.name == "init";
    let info = &ctx.classes[cidx as usize];
    let mut type_names = Vec::with_capacity(info.type_params.len() + sig.own_type_params.len());
    type_names.extend(info.type_params.iter().cloned());
    type_names.extend(sig.own_type_params.iter().cloned());
    let mut checker_bounds =
        Vec::with_capacity(sig.class_type_bounds.len() + sig.own_type_bounds.len());
    checker_bounds.extend(sig.class_type_bounds.iter().cloned());
    checker_bounds.extend(sig.own_type_bounds.iter().cloned());
    let type_param_count = type_names.len() as u32;
    let effect_param_count = sig.own_effect_params.len() as u32;
    let self_ty = info.self_ty;
    let self_mut = is_init || sig.mut_self;
    let mut params = Vec::with_capacity(sig.params.len() + 1);
    params.push(self_ty);
    params.extend_from_slice(&sig.params);
    let mut param_muts = Vec::with_capacity(sig.param_muts.len() + 1);
    param_muts.push(self_mut);
    param_muts.extend_from_slice(&sig.param_muts);
    let mut param_names = Vec::with_capacity(sig.param_names.len() + 1);
    param_names.push("self".to_string());
    param_names.extend(sig.param_names.iter().cloned());
    let env = TyEnv {
        type_names,
        type_bounds: checker_bounds,
        extra_bounds: Vec::new(),
        effect_names: sig.own_effect_params.clone(),
        type_offset: 0,
        self_interface: None,
        self_ty: Some(self_ty),
        core_scope: is_core,
    };
    let mut checker = FnChecker::top_level(RetKind::Known(sig.ret), env, sig.row.clone());
    checker.reserve_parameters(params.len());
    checker.self_class = Some(cidx);
    checker.locals.push((params[0], param_muts[0]));
    checker.scopes[0].insert("self".to_string(), 0);
    for (i, param) in method.params.iter().enumerate() {
        let slot = (i + 1) as u32;
        checker.locals.push((params[i + 1], param_muts[i + 1]));
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
    let type_bounds = into_hir_bounds(checked.type_bounds);
    // A constructor must complete on its normal exit.
    if is_init && !checked.diverges {
        let checker_state = checked.ctor.expect("ctor state present");
        crate::checkfn::require_complete(ctx, cidx, &checker_state, method.span)?;
    }
    ctx.funcs[sig.func as usize] = Some(HirFunc {
        imported: false,
        core: is_core,
        source_span: (!is_core).then_some(method.span),
        name: format!("{}.{}", ctx.classes[cidx as usize].name, method.name),
        type_params: type_param_count,
        type_bounds,
        effect_params: effect_param_count,
        params,
        param_muts,
        param_names,
        ret: sig.ret,
        row: sig.row.clone(),
        captures: vec![],
        locals: checked.locals,
        body: checked.body,
    });
    Ok(())
}
