//! Core surface demand collection.

use lm_source::ast;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(crate) struct CoreDemand {
    pub(crate) names: HashSet<String>,
    pub(crate) methods: HashSet<String>,
    sys_groups: HashMap<String, String>,
    sys_members: HashMap<String, (String, String)>,
}

impl CoreDemand {
    pub(crate) fn for_module(module: &ast::Module, bundle: &lm_abi::AbiBundle) -> CoreDemand {
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
                    demand.add_exprs(bundle, body);
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
            demand.add_exprs(bundle, &function.body);
        }
        demand.add_exprs(bundle, &module.entry);
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
        self.add_exprs(bundle, &method.body);
    }

    fn add_exprs(&mut self, bundle: &lm_abi::AbiBundle, expressions: &[ast::Expr]) {
        for expression in expressions {
            self.add_expr(bundle, expression);
        }
    }

    fn add_expr(&mut self, bundle: &lm_abi::AbiBundle, expression: &ast::Expr) {
        use ast::ExprKind;
        match &expression.kind {
            ExprKind::Assign { ty, value, .. } => {
                self.add_optional_type(ty.as_ref());
                self.add_expr(bundle, value);
            }
            ExprKind::AssignField { recv, value, .. } => {
                self.add_expr(bundle, recv);
                self.add_expr(bundle, value);
            }
            ExprKind::While { cond, body } => {
                self.add_expr(bundle, cond);
                self.add_exprs(bundle, body);
            }
            ExprKind::For { value, body, .. } => {
                self.name("Iterable");
                self.name("Iterator");
                self.name("Option");
                self.name("Char");
                self.add_expr(bundle, value);
                self.add_exprs(bundle, body);
            }
            ExprKind::Loop { body } => self.add_exprs(bundle, body),
            ExprKind::Return { value } | ExprKind::Break { value } => {
                if let Some(value) = value {
                    self.add_expr(bundle, value);
                }
            }
            ExprKind::Continue => {}
            ExprKind::Str(_) => self.name("String"),
            ExprKind::Regex(_) => self.name("Regex"),
            ExprKind::Char(_) => self.name("Char"),
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
                    self.name("ModuleCode");
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
                self.add_exprs(bundle, body);
            }
            ExprKind::If { arms, else_body } => {
                for (condition, body) in arms {
                    self.add_expr(bundle, condition);
                    self.add_exprs(bundle, body);
                }
                if let Some(body) = else_body {
                    self.add_exprs(bundle, body);
                }
            }
            ExprKind::Case { scrut, arms } => {
                self.add_expr(bundle, scrut);
                for arm in arms {
                    self.add_pattern(bundle, &arm.pattern);
                    self.add_exprs(bundle, &arm.body);
                }
            }
            ExprKind::Select { arms } => {
                self.name("Choice");
                for arm in arms {
                    self.add_expr(bundle, &arm.wait);
                    self.add_exprs(bundle, &arm.body);
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
            ast::PatternKind::Reflect {
                kind,
                generics,
                signature,
                binding,
            } => {
                match kind.as_str() {
                    "Class" | "Def" | "Const" => self.name("DeclarationCode"),
                    "Method" => self.name("MemberCode"),
                    _ => {}
                }
                self.add_generics(generics);
                self.add_type(signature);
                self.add_pattern(bundle, binding);
            }
            ast::PatternKind::Wildcard
            | ast::PatternKind::Int(_)
            | ast::PatternKind::Bool(_)
            | ast::PatternKind::Str(_) => {}
            ast::PatternKind::Char(_) => self.name("Char"),
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
            "compile" => self.add_names(&["Regex", "RegexError", "Result", "_regex_compile"]),
            "value" => self.add_names(&["Result", "_result_fault_value"]),
            "source" => self.add_names(&["CodeError", "Result", "DefinitionSource", "Option"]),
            "definition" => self.add_names(&["CodeError", "Result", "DefinitionSpec"]),
            "verify" => self.add_names(&["CodeError", "Result", "VerifiedModule"]),
            "entry_code" | "function_code" => {
                self.add_names(&["CodeError", "Result", "FunctionCode"])
            }
            "class_code" => self.add_names(&["CodeError", "Result", "ClassCode"]),
            "declarations" => self.add_names(&["List", "DeclarationCode"]),
            "members" => self.add_names(&["List", "MemberCode"]),
            "name" | "kind" => self.add_names(&["String"]),
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
            "restore_dynamic" => self.add_names(&["RestoreError", "Result", "DynValue"]),
            "resource" => self.add_names(&["TcpResource", "TlsStream"]),
            "serve_tcp_stream" => self.name("SocketAddress"),
            "choose" => self.name("Choice"),
            "send" | "close" => self.name("SendResult"),
            "pause" | "resume" => self.add_names(&["ProcError", "Result"]),
            "site" => self.add_names(&["CodeLocation", "Option"]),
            "trace" => self.name("CodeLocation"),
            "stack" => self.name("CodeLocation"),
            _ => {}
        }
    }
}

/// Convert one surface member name to its manifest form.
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
