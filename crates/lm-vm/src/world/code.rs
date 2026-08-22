//! Portable code values and installed code handles.
//!
//! This module implements the VM kernel operations for verification,
//! installation, typed lookup, activation, and slot replacement.

use super::*;
use lm_heap::{CodeHandleKind, PortableCode, PortableCodeKind};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy)]
struct CodeHandle {
    image: u32,
    generation: u32,
    instance: u32,
    kind: CodeHandleKind,
    index: u32,
}

impl CodeHandle {
    fn image_key(self) -> VmImageKey {
        VmImageKey {
            image: self.image,
            generation: self.generation,
        }
    }
}

struct CodeProvider {
    source: lm_bytecode::Module,
    interface: lm_bytecode::interface::Interface,
    funcs: Vec<u32>,
    classes: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstalledBindingTarget {
    Function(u32),
    Class { class: u32, constructor: u32 },
}

enum PreparedSlotTarget {
    Ready(ImageSlotTarget),
    Value(Value),
}

fn source_binding_slot(
    module: &lm_bytecode::Module,
    kind: PortableCodeKind,
    index: u32,
) -> Option<u32> {
    let position = module
        .slots
        .iter()
        .position(|slot| match (kind, slot.initial) {
            (PortableCodeKind::Function, Some(lm_bytecode::SlotTarget::Function(function))) => {
                function == index
            }
            (PortableCodeKind::Class, Some(lm_bytecode::SlotTarget::Class { class, .. })) => {
                class == index
            }
            _ => false,
        })?;
    u32::try_from(position).ok()
}

fn cached_binding_target(
    instance: &InstalledInstance,
    source_slot: u32,
) -> Option<InstalledBindingTarget> {
    match instance.binding_targets.get(source_slot as usize)? {
        ImageSlotTarget::Function(function) => Some(InstalledBindingTarget::Function(*function)),
        ImageSlotTarget::Class { class, constructor } => Some(InstalledBindingTarget::Class {
            class: *class,
            constructor: *constructor,
        }),
        ImageSlotTarget::Empty | ImageSlotTarget::Value(_) | ImageSlotTarget::Process { .. } => {
            None
        }
    }
}

fn installed_binding(
    bundle: &lm_abi::AbiBundle,
    instance: &InstalledInstance,
    kind: PortableCodeKind,
    source_index: u32,
) -> Option<(u32, InstalledBindingTarget)> {
    let module = lm_bytecode::decode_with_bundle(instance.artifact.as_slice(), bundle).ok()?;
    let source_slot = source_binding_slot(&module, kind, source_index)?;
    let target = cached_binding_target(instance, source_slot)?;
    Some((source_slot, target))
}

fn installed_binding_target(
    instance: &InstalledInstance,
    kind: CodeHandleKind,
    source_slot: u32,
) -> Option<InstalledBindingTarget> {
    instance.slots.get(source_slot as usize)?;
    let target = cached_binding_target(instance, source_slot)?;
    match (kind, target) {
        (CodeHandleKind::FunctionBinding, InstalledBindingTarget::Function(_))
        | (CodeHandleKind::ClassBinding, InstalledBindingTarget::Class { .. }) => Some(target),
        _ => None,
    }
}

fn reusable_definition_instance(
    image: &VmImageRecord,
    artifact: &[u8],
    interface: Option<&[u8]>,
    kind: CodeHandleKind,
    source_slot: u32,
) -> Option<u32> {
    image
        .instances
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, instance)| {
            if instance.artifact.as_slice() != artifact {
                return None;
            }
            if let Some(interface) = interface {
                let retained = instance.interface.as_ref()?.as_slice();
                if retained != interface {
                    return None;
                }
            }
            installed_binding_target(instance, kind, source_slot)?;
            u32::try_from(index).ok()
        })
}

fn source_origin(
    module: &lm_bytecode::Module,
    kind: lm_bytecode::debug::DefinitionKind,
    target: u32,
) -> Option<[u8; 32]> {
    let debug = lm_bytecode::debug::decode(&module.debug).ok()?;
    debug
        .definitions
        .iter()
        .rev()
        .find(|definition| definition.kind == kind && definition.target == target)
        .map(|definition| definition.origin)
}

fn closed_rows_match(
    left_module: &lm_bytecode::Module,
    left: &[u32],
    right_module: &lm_bytecode::Module,
    right: &[u32],
) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left_module.strings.get(*left as usize) == right_module.strings.get(*right as usize)
        })
}

#[derive(Clone, Copy)]
struct ClosedTypeSpace<'a> {
    module: &'a lm_bytecode::Module,
    types: &'a lm_bytecode::closed::TypeEnvs,
    identity: &'a lm_bytecode::identity::ModuleIdentity,
}

fn closed_classes_match(
    left_space: ClosedTypeSpace<'_>,
    left: u32,
    right_space: ClosedTypeSpace<'_>,
    right: u32,
) -> bool {
    let Some(left_class) = left_space.module.classes.get(left as usize) else {
        return false;
    };
    let Some(right_class) = right_space.module.classes.get(right as usize) else {
        return false;
    };
    if left_class.key != right_class.key {
        return false;
    }
    match (
        class_slot_abi(left_space.module, left),
        class_slot_abi(right_space.module, right),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => {
            left_space.identity.class_hashes.get(left as usize)
                == right_space.identity.class_hashes.get(right as usize)
        }
    }
}

fn class_slot_abi(module: &lm_bytecode::Module, class: u32) -> Option<[u8; 32]> {
    let key = &module.classes.get(class as usize)?.key;
    module.slots.iter().find_map(|slot| {
        let lm_bytecode::SlotContract::Class { abi, ty, .. } = &slot.contract else {
            return None;
        };
        let candidate = match module.types.get(*ty as usize)? {
            lm_bytecode::BcType::Class(candidate) | lm_bytecode::BcType::Inst(candidate, _) => {
                *candidate
            }
            _ => return None,
        };
        (module.classes.get(candidate as usize)?.key == key.as_str()).then_some(*abi)
    })
}

fn closed_types_match(
    bundle: &lm_abi::AbiBundle,
    left_space: ClosedTypeSpace<'_>,
    left: ClosedTypeId,
    right_space: ClosedTypeSpace<'_>,
    right: ClosedTypeId,
) -> bool {
    let mut pending = vec![(left, right)];
    while let Some((left, right)) = pending.pop() {
        let Some(left) = left_space.types.ty(left) else {
            return false;
        };
        let Some(right) = right_space.types.ty(right) else {
            return false;
        };
        match (left, right) {
            (ClosedType::Unit, ClosedType::Unit)
            | (ClosedType::Bool, ClosedType::Bool)
            | (ClosedType::Int, ClosedType::Int)
            | (ClosedType::Str, ClosedType::Str)
            | (ClosedType::Fault, ClosedType::Fault)
            | (ClosedType::Request, ClosedType::Request)
            | (ClosedType::PolicyTable, ClosedType::PolicyTable)
            | (ClosedType::Vm, ClosedType::Vm)
            | (ClosedType::Digest, ClosedType::Digest)
            | (ClosedType::VmSnapshot, ClosedType::VmSnapshot)
            | (ClosedType::Bytes, ClosedType::Bytes)
            | (ClosedType::FileHandle, ClosedType::FileHandle)
            | (ClosedType::ResourceHandle, ClosedType::ResourceHandle) => {}
            (ClosedType::HostResource, ClosedType::HostResource) => {}
            (ClosedType::Class(left), ClosedType::Class(right)) => {
                if !closed_classes_match(left_space, *left, right_space, *right) {
                    return false;
                }
            }
            (
                ClosedType::Inst(left_class, left_args),
                ClosedType::Inst(right_class, right_args),
            ) => {
                if left_args.len() != right_args.len()
                    || !closed_classes_match(left_space, *left_class, right_space, *right_class)
                {
                    return false;
                }
                pending.extend(left_args.iter().copied().zip(right_args.iter().copied()));
            }
            (ClosedType::List(left), ClosedType::List(right))
            | (ClosedType::Run(left), ClosedType::Run(right))
            | (ClosedType::Wait(left), ClosedType::Wait(right))
            | (ClosedType::RunSnapshot(left), ClosedType::RunSnapshot(right)) => {
                pending.push((*left, *right));
            }
            (ClosedType::Map(left_key, left_value), ClosedType::Map(right_key, right_value))
            | (
                ClosedType::PendingCall(left_key, left_value),
                ClosedType::PendingCall(right_key, right_value),
            )
            | (
                ClosedType::Handle(left_key, left_value),
                ClosedType::Handle(right_key, right_value),
            ) => {
                pending.push((*left_key, *right_key));
                pending.push((*left_value, *right_value));
            }
            (ClosedType::Tuple(left), ClosedType::Tuple(right)) => {
                if left.len() != right.len() {
                    return false;
                }
                pending.extend(left.iter().copied().zip(right.iter().copied()));
            }
            (
                ClosedType::Fn(left_params, left_muts, left_ret, left_row),
                ClosedType::Fn(right_params, right_muts, right_ret, right_row),
            )
            | (
                ClosedType::Callback(left_params, left_muts, left_ret, left_row),
                ClosedType::Callback(right_params, right_muts, right_ret, right_row),
            ) => {
                if left_params.len() != right_params.len()
                    || left_muts != right_muts
                    || !closed_rows_match(
                        left_space.module,
                        left_row,
                        right_space.module,
                        right_row,
                    )
                {
                    return false;
                }
                pending.extend(
                    left_params
                        .iter()
                        .copied()
                        .zip(right_params.iter().copied()),
                );
                pending.push((*left_ret, *right_ret));
            }
            (ClosedType::Op(left_op, left_fn), ClosedType::Op(right_op, right_fn)) => {
                if bundle.op_identity(*left_op) != bundle.op_identity(*right_op) {
                    return false;
                }
                pending.push((*left_fn, *right_fn));
            }
            _ => return false,
        }
    }
    true
}

impl CodeProvider {
    fn resolve(
        &self,
        import: &lm_bytecode::Import,
    ) -> Result<lm_bytecode::append::ResolvedImport, String> {
        let export_name = if import.kind == lm_bytecode::ImportKind::Method {
            import
                .name
                .rsplit_once('.')
                .map_or(import.name.as_str(), |(class, _)| class)
        } else {
            import.name.as_str()
        };
        let interface = self.interface.find(export_name).ok_or_else(|| {
            format!(
                "the module `{}` does not export `{export_name}`",
                import.module
            )
        })?;
        if interface.iface_hash != import.hash {
            return Err(format!(
                "the import `{}` pins another interface",
                import.name
            ));
        }
        let export = self
            .source
            .exports
            .iter()
            .find(|export| export.name == export_name)
            .ok_or_else(|| format!("the module does not export `{export_name}`"))?;
        match import.kind {
            lm_bytecode::ImportKind::Class => {
                if !export.kind.is_class() {
                    return Err(format!("the export `{export_name}` is not a class"));
                }
                self.classes
                    .get(export.def as usize)
                    .copied()
                    .map(lm_bytecode::append::ResolvedImport::Class)
                    .ok_or_else(|| format!("the class export `{export_name}` has no target"))
            }
            lm_bytecode::ImportKind::Func => {
                if export.kind != lm_bytecode::ExportKind::Function {
                    return Err(format!("the export `{export_name}` is not a function"));
                }
                self.function(export.def, export_name)
            }
            lm_bytecode::ImportKind::Ctor => {
                if !export.kind.is_class() || export.ctor == lm_bytecode::NO_CTOR {
                    return Err(format!("the class `{export_name}` has no constructor"));
                }
                self.function(export.ctor, export_name)
            }
            lm_bytecode::ImportKind::Method => {
                let (_, method) = import
                    .name
                    .rsplit_once('.')
                    .ok_or_else(|| format!("the import `{}` has no class name", import.name))?;
                if !export.kind.is_class() {
                    return Err(format!("the export `{export_name}` is not a class"));
                }
                let class = self
                    .source
                    .classes
                    .get(export.def as usize)
                    .ok_or_else(|| format!("the class `{export_name}` has no definition"))?;
                let selector = self
                    .source
                    .selectors
                    .iter()
                    .position(|name| name == method)
                    .ok_or_else(|| format!("the class `{export_name}` has no `{method}` method"))?;
                let function = class
                    .methods
                    .iter()
                    .find(|(found, _)| *found as usize == selector)
                    .map(|(_, function)| *function)
                    .ok_or_else(|| format!("the class `{export_name}` has no `{method}` method"))?;
                self.function(function, method)
            }
        }
    }

    fn function(
        &self,
        source: u32,
        name: &str,
    ) -> Result<lm_bytecode::append::ResolvedImport, String> {
        self.funcs
            .get(source as usize)
            .copied()
            .map(lm_bytecode::append::ResolvedImport::Function)
            .ok_or_else(|| format!("the function `{name}` has no target"))
    }
}

impl World {
    /// Execute one portable-code kernel operation.
    pub(super) fn code_exec(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        match op {
            lm_abi::OP_VM_ARTIFACT => self.code_artifact(vm, op, args[0]),
            lm_abi::OP_COMPILER_VERIFY => self.code_verify(vm, op, args[0]),
            lm_abi::OP_VM_INSTALL => self.code_install(vm, op, args[0], args[1], None),
            lm_abi::OP_VM_INSTALL_WITH => {
                self.code_install(vm, op, args[0], args[1], Some(args[2]))
            }
            lm_abi::OP_VM_INSTANCE_ENTRY => self.code_entry(vm, op, args[0]),
            lm_abi::OP_VM_INSTANCE_FUNCTION => self.code_function(vm, op, args[0], args[1]),
            lm_abi::OP_VM_INSTANCE_CLASS => self.code_class(vm, op, args[0], args[1]),
            lm_abi::OP_VM_INSTANCE_SLOT_FOR => self.code_slot_for(vm, op, args[0], args[1]),
            lm_abi::OP_VM_INSTANCE_SLOT_SPEC => self.code_slot_spec(vm, op, args[0], args[1]),
            lm_abi::OP_VM_INSTANCE_ENTRY_BINDING => self.code_entry_binding(vm, op, args[0]),
            lm_abi::OP_VM_INSTANCE_FUNCTION_BINDING => {
                self.code_function_binding(vm, op, args[0], args[1])
            }
            lm_abi::OP_VM_INSTANCE_CLASS_BINDING => {
                self.code_class_binding(vm, op, args[0], args[1])
            }
            lm_abi::OP_VM_BINDING_SLOT => self.code_binding_slot(vm, op, args[0]),
            lm_abi::OP_VM_BINDING_SPEC => self.code_binding_spec(vm, op, args[0]),
            lm_abi::OP_VM_BINDING_INSTANCE => self.code_binding_instance(vm, op, args[0]),
            lm_abi::OP_VM_BINDING_FUNCTION_TARGET => {
                self.code_binding_function_target(vm, op, args[0])
            }
            lm_abi::OP_VM_BINDING_CLASS_TARGET => self.code_binding_class_target(vm, op, args[0]),
            lm_abi::OP_VM_MODULE_ENTRY_CODE => self.code_module_entry(vm, op, args[0]),
            lm_abi::OP_VM_MODULE_FUNCTION_CODE => {
                self.code_module_function(vm, op, args[0], args[1])
            }
            lm_abi::OP_VM_MODULE_CLASS_CODE => self.code_module_class(vm, op, args[0], args[1]),
            lm_abi::OP_VM_ACTIVATE_DEF => self.code_activate(vm, op, args[0], args[1], args[2]),
            lm_abi::OP_VM_REPLACE_FUNCTION
            | lm_abi::OP_VM_REPLACE_CLASS
            | lm_abi::OP_VM_REPLACE_VALUE
            | lm_abi::OP_VM_REPLACE_PROCESS => self.code_replace(vm, op, args[0], args[1], args[2]),
            lm_abi::OP_VM_CHANGE_FUNCTION
            | lm_abi::OP_VM_CHANGE_CLASS
            | lm_abi::OP_VM_CHANGE_VALUE
            | lm_abi::OP_VM_CHANGE_PROCESS => self.code_change(vm, op, args[0], args[1], args[2]),
            lm_abi::OP_VM_REPLACE_ALL => self.code_replace_all(vm, op, args[0], args[1]),
            _ => self.fault_caller(
                vm,
                op,
                FaultCode::MalformedState,
                "the operation has no code rule",
            ),
        }
    }

    fn code_artifact(&mut self, vm: VmId, op: u32, value: Value) {
        let bytes = match value
            .as_obj()
            .map(|reference| self.machines[vm as usize].vm.heap.get(reference))
        {
            Some(Object::Bytes(bytes)) => bytes.clone(),
            _ => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::TypeMismatch,
                    "the artifact input is not Bytes",
                );
                return;
            }
        };
        let object = Object::NativeCode(Box::new(PortableCode {
            kind: PortableCodeKind::Artifact,
            bytes,
            interface: None,
            index: u32::MAX,
            origin: None,
        }));
        match self.machines[vm as usize].alloc(object) {
            Ok(value) => self.install_value_reply(vm, value),
            Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
        }
    }

    fn code_verify(&mut self, vm: VmId, op: u32, value: Value) {
        let code = match self.portable_code(vm, value, PortableCodeKind::Artifact) {
            Ok(code) => code,
            Err(code) => {
                self.fault_caller(vm, op, code, "the verify receiver is not an Artifact");
                return;
            }
        };
        let verified = lm_bytecode::decode_with_bundle(code.bytes.as_slice(), self.loaded.bundle())
            .map_err(|error| format!("the artifact did not decode: {error}"))
            .and_then(|module| {
                lm_verify::verify_module_with_bundle(&module, self.loaded.bundle())
                    .map_err(|error| format!("the artifact did not verify: {error}"))?;
                let identity = lm_bytecode::identity::module_identity_with_bundle(
                    &module,
                    self.loaded.bundle(),
                )
                .map_err(|error| format!("the artifact has no identity: {error}"))?;
                if let Some(bytes) = &code.interface {
                    let interface = lm_bytecode::interface::decode_interface(bytes.as_slice())
                        .map_err(|error| format!("the interface did not decode: {error}"))?;
                    if lm_bytecode::interface::encode_interface(&interface) != bytes.as_slice() {
                        return Err("the interface bytes are not canonical".to_string());
                    }
                    lm_bytecode::interface::validate_interface_with_bundle(
                        &module,
                        &identity,
                        &interface,
                        self.loaded.bundle(),
                    )
                    .map_err(|error| format!("the interface is invalid: {error}"))?;
                }
                Ok(())
            });
        match verified {
            Ok(()) => {
                let value =
                    self.machines[vm as usize].alloc(Object::NativeCode(Box::new(PortableCode {
                        kind: PortableCodeKind::VerifiedModule,
                        bytes: code.bytes,
                        interface: code.interface,
                        index: u32::MAX,
                        origin: None,
                    })));
                let result = value.and_then(|value| self.code_ok(vm, value));
                self.finish_code_result(vm, op, result);
            }
            Err(message) => {
                let value = self.code_error(vm, &message);
                self.finish_code_result(vm, op, value);
            }
        }
    }

    fn code_module_entry(&mut self, vm: VmId, op: u32, value: Value) {
        let code = match self.portable_code(vm, value, PortableCodeKind::VerifiedModule) {
            Ok(code) => code,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not a VerifiedModule");
                return;
            }
        };
        let module =
            match lm_bytecode::decode_with_bundle(code.bytes.as_slice(), self.loaded.bundle()) {
                Ok(module) => module,
                Err(error) => {
                    self.finish_code_error(
                        vm,
                        op,
                        &format!("the verified module did not decode: {error}"),
                    );
                    return;
                }
            };
        self.finish_portable_function_lookup(vm, op, code, module.entry, None);
    }

    fn code_module_function(&mut self, vm: VmId, op: u32, value: Value, name: Value) {
        let code = match self.portable_code(vm, value, PortableCodeKind::VerifiedModule) {
            Ok(code) => code,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not a VerifiedModule");
                return;
            }
        };
        let Some(name) = self.code_name(vm, op, name, "function") else {
            return;
        };
        let module =
            match lm_bytecode::decode_with_bundle(code.bytes.as_slice(), self.loaded.bundle()) {
                Ok(module) => module,
                Err(error) => {
                    self.finish_code_error(
                        vm,
                        op,
                        &format!("the verified module did not decode: {error}"),
                    );
                    return;
                }
            };
        let function = module.exports.iter().find_map(|export| {
            (export.name == name && export.kind == lm_bytecode::ExportKind::Function)
                .then_some(export.def)
        });
        let Some(function) = function else {
            self.finish_code_error(
                vm,
                op,
                "the verified module has no exported function with this name",
            );
            return;
        };
        let origin = source_origin(
            &module,
            lm_bytecode::debug::DefinitionKind::Function,
            function,
        );
        self.finish_portable_function_lookup(vm, op, code, function, origin);
    }

    fn code_module_class(&mut self, vm: VmId, op: u32, value: Value, name: Value) {
        let code = match self.portable_code(vm, value, PortableCodeKind::VerifiedModule) {
            Ok(code) => code,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not a VerifiedModule");
                return;
            }
        };
        let Some(name) = self.code_name(vm, op, name, "class") else {
            return;
        };
        let module =
            match lm_bytecode::decode_with_bundle(code.bytes.as_slice(), self.loaded.bundle()) {
                Ok(module) => module,
                Err(error) => {
                    self.finish_code_error(
                        vm,
                        op,
                        &format!("the verified module did not decode: {error}"),
                    );
                    return;
                }
            };
        let class = module.exports.iter().find_map(|export| {
            (export.name == name && export.kind.is_class()).then_some(export.def)
        });
        let Some(class) = class else {
            self.finish_code_error(
                vm,
                op,
                "the verified module has no exported class with this name",
            );
            return;
        };
        let value = self.machines[vm as usize].alloc(Object::NativeCode(Box::new(PortableCode {
            kind: PortableCodeKind::Class,
            bytes: code.bytes,
            interface: code.interface,
            index: class,
            origin: source_origin(&module, lm_bytecode::debug::DefinitionKind::Class, class),
        })));
        let result = value.and_then(|value| self.code_ok(vm, value));
        self.finish_code_result(vm, op, result);
    }

    fn code_name(&mut self, vm: VmId, op: u32, value: Value, kind: &str) -> Option<String> {
        match value
            .as_obj()
            .map(|reference| self.machines[vm as usize].vm.heap.get(reference))
        {
            Some(Object::Str(text)) => Some(text.as_str().to_string()),
            _ => {
                let message = format!("the {kind} name is not String");
                self.fault_caller(vm, op, FaultCode::TypeMismatch, &message);
                None
            }
        }
    }

    fn code_install(
        &mut self,
        vm: VmId,
        op: u32,
        image: Value,
        input: Value,
        links: Option<Value>,
    ) {
        let Some(key) = self.image_arg(vm, op, image) else {
            return;
        };
        let code = match input
            .as_obj()
            .map(|reference| self.machines[vm as usize].vm.heap.get(reference))
        {
            Some(Object::NativeCode(code))
                if matches!(
                    code.kind,
                    PortableCodeKind::VerifiedModule
                        | PortableCodeKind::Function
                        | PortableCodeKind::Class
                ) =>
            {
                (**code).clone()
            }
            Some(Object::Closure { .. }) => match self.portable_function_value(vm, input) {
                Ok(code) => code,
                Err(message) => {
                    self.finish_code_error(vm, op, &message);
                    return;
                }
            },
            _ => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::TypeMismatch,
                    "the install input is not portable code or a function",
                );
                return;
            }
        };
        let source = if matches!(
            code.kind,
            PortableCodeKind::Function | PortableCodeKind::Class
        ) {
            Some(
                match lm_bytecode::decode_with_bundle(code.bytes.as_slice(), self.loaded.bundle()) {
                    Ok(source) => source,
                    Err(error) => {
                        self.finish_code_error(
                            vm,
                            op,
                            &format!("the portable code did not decode: {error}"),
                        );
                        return;
                    }
                },
            )
        } else {
            None
        };
        let source_slot = if matches!(
            code.kind,
            PortableCodeKind::Function | PortableCodeKind::Class
        ) {
            match source
                .as_ref()
                .and_then(|source| source_binding_slot(source, code.kind, code.index))
            {
                Some(slot) => Some(slot),
                None => {
                    self.finish_code_error(
                        vm,
                        op,
                        "the portable definition has no published binding",
                    );
                    return;
                }
            }
        } else {
            None
        };
        if code.kind == PortableCodeKind::Function {
            let Some(function_class) = self.core.function_binding else {
                self.machines[vm as usize].set_fault(FaultCode::MalformedState, "", Some(op));
                return;
            };
            let contract = self
                .requested_function_contract(vm, function_class)
                .and_then(|(input, output)| {
                    self.portable_function_matches_contract(
                        source.as_ref().expect("function code has a decoded module"),
                        code.index,
                        input,
                        output,
                    )
                });
            match contract {
                Ok(true) => {}
                Ok(false) => {
                    self.finish_code_error(
                        vm,
                        op,
                        "the function code does not match the requested contract",
                    );
                    return;
                }
                Err(fault) => {
                    self.machines[vm as usize].set_fault(fault, "", Some(op));
                    return;
                }
            }
        }
        let kind = code.kind;
        if matches!(kind, PortableCodeKind::Function | PortableCodeKind::Class)
            && source
                .as_ref()
                .is_some_and(|source| source.imports.is_empty())
        {
            let source_slot = source_slot.expect("portable definition has one source slot");
            let handle_kind = if kind == PortableCodeKind::Function {
                CodeHandleKind::FunctionBinding
            } else {
                CodeHandleKind::ClassBinding
            };
            let existing = reusable_definition_instance(
                &self.vm_images[key.image as usize],
                code.bytes.as_slice(),
                code.interface.as_ref().map(|bytes| bytes.as_slice()),
                handle_kind,
                source_slot,
            );
            if let Some(instance) = existing {
                let value = self.machines[vm as usize].alloc(Object::NativeCodeHandle {
                    image: key.image,
                    generation: key.generation,
                    instance,
                    kind: handle_kind,
                    index: source_slot,
                });
                let result = value.and_then(|value| self.code_ok(vm, value));
                self.finish_code_result(vm, op, result);
                return;
            }
        }
        let imports = match links {
            Some(links) => match self.resolve_code_imports(vm, key, links, code.bytes.as_slice()) {
                Ok(imports) => imports,
                Err(message) => {
                    let value = self.code_error(vm, &message);
                    self.finish_code_result(vm, op, value);
                    return;
                }
            },
            None => Vec::new(),
        };
        match self.install_artifact(key, code.bytes, code.interface, &imports) {
            Ok(instance) => {
                let selected = self.vm_images[key.image as usize]
                    .instances
                    .get(instance as usize);
                let (handle_kind, index) = match kind {
                    PortableCodeKind::VerifiedModule => (CodeHandleKind::Instance, instance),
                    PortableCodeKind::Function | PortableCodeKind::Class => {
                        let handle_kind = if kind == PortableCodeKind::Function {
                            CodeHandleKind::FunctionBinding
                        } else {
                            CodeHandleKind::ClassBinding
                        };
                        let valid = source_slot.and_then(|source_slot| {
                            selected
                                .and_then(|selected| {
                                    installed_binding_target(selected, handle_kind, source_slot)
                                })
                                .map(|_| source_slot)
                        });
                        let Some(source_slot) = valid else {
                            self.finish_code_error(vm, op, "the installed binding is invalid");
                            return;
                        };
                        (handle_kind, source_slot)
                    }
                    _ => {
                        self.finish_code_error(vm, op, "the install input has another code kind");
                        return;
                    }
                };
                let value = self.machines[vm as usize].alloc(Object::NativeCodeHandle {
                    image: key.image,
                    generation: key.generation,
                    instance,
                    kind: handle_kind,
                    index,
                });
                let result = value.and_then(|value| self.code_ok(vm, value));
                self.finish_code_result(vm, op, result);
            }
            Err(message) => {
                let value = self.code_error(vm, &message);
                self.finish_code_result(vm, op, value);
            }
        }
    }

    fn portable_function_value(&self, vm: VmId, value: Value) -> Result<PortableCode, String> {
        let function = self.function_value_target(vm, value)?;
        self.portable_function_origin(vm, function, None)
    }

    fn function_value_target(&self, vm: VmId, value: Value) -> Result<u32, String> {
        let reference = value
            .as_obj()
            .ok_or_else(|| "the install input is not a function".to_string())?;
        let (function, captures, env) = match self.machines[vm as usize].vm.heap.get(reference) {
            Object::Closure {
                func,
                captures,
                env,
            } => (*func, captures.len(), env.env()),
            _ => return Err("the install input is not a function".to_string()),
        };
        if captures != 0 {
            return Err("a function with captures is not portable code".to_string());
        }
        if env != lm_value::TypeEnvId::EMPTY {
            return Err("an applied generic function is not portable code".to_string());
        }
        let body = self
            .module
            .funcs
            .get(function as usize)
            .ok_or_else(|| "the function has no verified body".to_string())?;
        if body.type_params != 0 || body.effect_params != 0 {
            return Err("a generic function needs an explicit portable application".to_string());
        }
        if !body.captures.is_empty() {
            return Err("a function with captures is not portable code".to_string());
        }
        if body.param_muts.iter().any(|marker| *marker) {
            return Err("a function with a mut parameter is not portable code".to_string());
        }
        Ok(function)
    }

    fn portable_function_origin(
        &self,
        vm: VmId,
        function: u32,
        origin: Option<[u8; 32]>,
    ) -> Result<PortableCode, String> {
        if (function as usize) < self.base_loaded.module().funcs.len() {
            return Ok(PortableCode {
                kind: PortableCodeKind::Function,
                bytes: self.base_loaded.artifact_bytes(),
                interface: None,
                index: function,
                origin,
            });
        }
        let preferred = self.machines[vm as usize].image;
        let mut image_order: Vec<usize> = preferred
            .into_iter()
            .map(|key| key.image as usize)
            .collect();
        for (index, image) in self.vm_images.iter().enumerate() {
            if image.live && !image_order.contains(&index) {
                image_order.push(index);
            }
        }
        for image in image_order {
            for instance in self.vm_images[image].instances.iter().rev() {
                if let Some(index) = instance
                    .funcs
                    .iter()
                    .position(|candidate| *candidate == function)
                {
                    return Ok(PortableCode {
                        kind: PortableCodeKind::Function,
                        bytes: instance.artifact.clone(),
                        interface: instance.interface.clone(),
                        index: index as u32,
                        origin,
                    });
                }
            }
        }
        let wanted = self
            .identity()
            .map_err(|_| "the function has no verified identity".to_string())?
            .func_hashes
            .get(function as usize)
            .copied()
            .ok_or_else(|| "the function has no verified identity".to_string())?;
        for artifact in self.installations.iter().rev() {
            let Ok(source) =
                lm_bytecode::decode_with_bundle(artifact.as_slice(), self.loaded.bundle())
            else {
                continue;
            };
            let Ok(identity) =
                lm_bytecode::identity::module_identity_with_bundle(&source, self.loaded.bundle())
            else {
                continue;
            };
            if let Some(index) = identity.func_hashes.iter().position(|hash| *hash == wanted) {
                return Ok(PortableCode {
                    kind: PortableCodeKind::Function,
                    bytes: artifact.clone(),
                    interface: None,
                    index: index as u32,
                    origin,
                });
            }
        }
        Err("the function has no retained verified origin".to_string())
    }

    fn portable_class_origin(
        &self,
        vm: VmId,
        class: u32,
        origin: Option<[u8; 32]>,
    ) -> Result<PortableCode, String> {
        if (class as usize) < self.base_loaded.module().classes.len() {
            return Ok(PortableCode {
                kind: PortableCodeKind::Class,
                bytes: self.base_loaded.artifact_bytes(),
                interface: None,
                index: class,
                origin,
            });
        }
        let preferred = self.machines[vm as usize].image;
        let mut image_order: Vec<usize> = preferred
            .into_iter()
            .map(|key| key.image as usize)
            .collect();
        for (index, image) in self.vm_images.iter().enumerate() {
            if image.live && !image_order.contains(&index) {
                image_order.push(index);
            }
        }
        for image in image_order {
            for instance in self.vm_images[image].instances.iter().rev() {
                if let Some(index) = instance
                    .classes
                    .iter()
                    .position(|candidate| *candidate == class)
                {
                    return Ok(PortableCode {
                        kind: PortableCodeKind::Class,
                        bytes: instance.artifact.clone(),
                        interface: instance.interface.clone(),
                        index: index as u32,
                        origin,
                    });
                }
            }
        }
        let wanted = self
            .identity()
            .map_err(|_| "the class has no verified identity".to_string())?
            .class_hashes
            .get(class as usize)
            .copied()
            .ok_or_else(|| "the class has no verified identity".to_string())?;
        for artifact in self.installations.iter().rev() {
            let Ok(source) =
                lm_bytecode::decode_with_bundle(artifact.as_slice(), self.loaded.bundle())
            else {
                continue;
            };
            let Ok(identity) =
                lm_bytecode::identity::module_identity_with_bundle(&source, self.loaded.bundle())
            else {
                continue;
            };
            if let Some(index) = identity
                .class_hashes
                .iter()
                .position(|hash| *hash == wanted)
            {
                return Ok(PortableCode {
                    kind: PortableCodeKind::Class,
                    bytes: artifact.clone(),
                    interface: None,
                    index: index as u32,
                    origin,
                });
            }
        }
        Err("the class has no retained verified origin".to_string())
    }

    pub(super) fn handle_function_code(
        &mut self,
        vm: VmId,
        function: u32,
        origin: Option<[u8; 32]>,
    ) {
        match self.portable_function_origin(vm, function, origin) {
            Ok(code) => {
                let value = self.machines[vm as usize]
                    .alloc(Object::NativeCode(Box::new(code)))
                    .and_then(|value| self.machines[vm as usize].push(value));
                if let Err(fault) = value {
                    self.machines[vm as usize].set_fault(fault, "", None);
                }
            }
            Err(message) => {
                self.machines[vm as usize].set_fault(FaultCode::InvalidVmState, &message, None);
            }
        }
    }

    pub(super) fn handle_class_code(&mut self, vm: VmId, class: u32, origin: Option<[u8; 32]>) {
        match self.portable_class_origin(vm, class, origin) {
            Ok(code) => {
                let value = self.machines[vm as usize]
                    .alloc(Object::NativeCode(Box::new(code)))
                    .and_then(|value| self.machines[vm as usize].push(value));
                if let Err(fault) = value {
                    self.machines[vm as usize].set_fault(fault, "", None);
                }
            }
            Err(message) => {
                self.machines[vm as usize].set_fault(FaultCode::InvalidVmState, &message, None);
            }
        }
    }

    fn resolve_code_imports(
        &self,
        vm: VmId,
        key: VmImageKey,
        links: Value,
        artifact: &[u8],
    ) -> Result<Vec<lm_bytecode::append::ResolvedImport>, String> {
        let module = lm_bytecode::decode_with_bundle(artifact, self.loaded.bundle())
            .map_err(|error| format!("the artifact did not decode: {error}"))?;
        let reference = links
            .as_obj()
            .ok_or_else(|| "the link environment has another shape".to_string())?;
        let fields = match self.machines[vm as usize].vm.heap.get(reference) {
            Object::Instance { class, fields, .. }
                if Some(*class) == self.core.link_env && fields.len() == 1 =>
            {
                fields
            }
            _ => return Err("the link environment has another shape".to_string()),
        };
        let list = fields[0]
            .as_obj()
            .ok_or_else(|| "the link environment instance list has another shape".to_string())?;
        let values = match self.machines[vm as usize].vm.heap.get(list) {
            Object::List { items, .. } => items.clone(),
            _ => return Err("the link environment instance list has another shape".to_string()),
        };
        let mut providers = Vec::new();
        providers
            .try_reserve_exact(values.len())
            .map_err(|_| "the link environment is too large".to_string())?;
        for value in values {
            let handle = self
                .code_handle(vm, value, CodeHandleKind::Instance)
                .map_err(|_| "the link environment contains another value".to_string())?;
            if handle.image_key() != key {
                return Err("a link provider belongs to another VM image".to_string());
            }
            let instance = self
                .live_instance(handle)
                .ok_or_else(|| "the link environment contains a stale instance".to_string())?;
            let bytes = instance
                .interface
                .as_ref()
                .ok_or_else(|| "a link provider has no compiler interface".to_string())?;
            let interface = lm_bytecode::interface::decode_interface(bytes.as_slice())
                .map_err(|error| format!("a link provider interface did not decode: {error}"))?;
            if providers.iter().any(|provider: &CodeProvider| {
                provider.interface.module_path == interface.module_path
            }) {
                return Err(format!(
                    "the link environment binds `{}` twice",
                    interface.module_path
                ));
            }
            let source =
                lm_bytecode::decode_with_bundle(instance.artifact.as_slice(), self.loaded.bundle())
                    .map_err(|error| format!("a link provider did not decode: {error}"))?;
            providers.push(CodeProvider {
                source,
                interface,
                funcs: instance.funcs.clone(),
                classes: instance.classes.clone(),
            });
        }

        let mut resolved = Vec::new();
        resolved
            .try_reserve_exact(module.imports.len())
            .map_err(|_| "the import table is too large".to_string())?;
        for import in &module.imports {
            let provider = providers
                .iter()
                .find(|provider| provider.interface.module_path == import.module)
                .ok_or_else(|| format!("the link environment has no `{}` module", import.module))?;
            resolved.push(provider.resolve(import)?);
        }
        Ok(resolved)
    }

    fn code_entry(&mut self, vm: VmId, op: u32, value: Value) {
        let handle = match self.code_handle(vm, value, CodeHandleKind::Instance) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not an Instance");
                return;
            }
        };
        let function = match self.live_instance(handle) {
            Some(instance) => instance.entry,
            None => {
                let value = self.code_error(vm, "the module instance is not live");
                self.finish_code_result(vm, op, value);
                return;
            }
        };
        self.finish_function_lookup(vm, op, handle, function);
    }

    fn code_function(&mut self, vm: VmId, op: u32, value: Value, name: Value) {
        let handle = match self.code_handle(vm, value, CodeHandleKind::Instance) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not an Instance");
                return;
            }
        };
        let name = match name
            .as_obj()
            .map(|reference| self.machines[vm as usize].vm.heap.get(reference))
        {
            Some(Object::Str(text)) => text.as_str().to_string(),
            _ => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::TypeMismatch,
                    "the function name is not String",
                );
                return;
            }
        };
        let function = self.live_instance(handle).and_then(|instance| {
            instance
                .exports
                .iter()
                .find_map(|(export, function)| (export == &name).then_some(*function))
        });
        let Some(function) = function else {
            let value = self.code_error(
                vm,
                "the module instance has no exported function with this name",
            );
            self.finish_code_result(vm, op, value);
            return;
        };
        self.finish_function_lookup(vm, op, handle, function);
    }

    fn code_class(&mut self, vm: VmId, op: u32, value: Value, name: Value) {
        let handle = match self.code_handle(vm, value, CodeHandleKind::Instance) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not an Instance");
                return;
            }
        };
        let name = match name
            .as_obj()
            .map(|reference| self.machines[vm as usize].vm.heap.get(reference))
        {
            Some(Object::Str(text)) => text.as_str().to_string(),
            _ => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::TypeMismatch,
                    "the class name is not String",
                );
                return;
            }
        };
        let class = self.live_instance(handle).and_then(|instance| {
            let source =
                lm_bytecode::decode_with_bundle(instance.artifact.as_slice(), self.loaded.bundle())
                    .ok()?;
            let export = source
                .exports
                .iter()
                .find(|export| export.name == name && export.kind.is_class())?;
            instance.classes.get(export.def as usize).copied()
        });
        let Some(class) = class else {
            let value = self.code_error(
                vm,
                "the module instance has no exported class with this name",
            );
            self.finish_code_result(vm, op, value);
            return;
        };
        let value = self.machines[vm as usize].alloc(Object::NativeCodeHandle {
            image: handle.image,
            generation: handle.generation,
            instance: handle.instance,
            kind: CodeHandleKind::Class,
            index: class,
        });
        let result = value.and_then(|value| self.code_ok(vm, value));
        self.finish_code_result(vm, op, result);
    }

    fn code_entry_binding(&mut self, vm: VmId, op: u32, value: Value) {
        let handle = match self.code_handle(vm, value, CodeHandleKind::Instance) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not an Instance");
                return;
            }
        };
        let binding = self.live_instance(handle).and_then(|instance| {
            let source =
                lm_bytecode::decode_with_bundle(instance.artifact.as_slice(), self.loaded.bundle())
                    .ok()?;
            installed_binding(
                self.loaded.bundle(),
                instance,
                PortableCodeKind::Function,
                source.entry,
            )
        });
        let Some((slot, InstalledBindingTarget::Function(function))) = binding else {
            self.finish_code_error(vm, op, "the module entry has no installed binding");
            return;
        };
        self.finish_function_binding_lookup(vm, op, handle, slot, function);
    }

    fn code_function_binding(&mut self, vm: VmId, op: u32, value: Value, name: Value) {
        let handle = match self.code_handle(vm, value, CodeHandleKind::Instance) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not an Instance");
                return;
            }
        };
        let Some(name) = self.code_name(vm, op, name, "function") else {
            return;
        };
        let binding = self.live_instance(handle).and_then(|instance| {
            let source =
                lm_bytecode::decode_with_bundle(instance.artifact.as_slice(), self.loaded.bundle())
                    .ok()?;
            let suffix = format!(".{name}");
            let mut source_slots: Vec<u32> = source
                .bindings
                .iter()
                .filter(|binding| {
                    binding.class == lm_bytecode::NO_CLASS
                        && (binding.key == name || binding.key.ends_with(&suffix))
                })
                .filter_map(|binding| {
                    source_binding_slot(&source, PortableCodeKind::Function, binding.func)
                })
                .collect();
            source_slots.sort_unstable();
            source_slots.dedup();
            let [source_slot] = source_slots.as_slice() else {
                return None;
            };
            let target = cached_binding_target(instance, *source_slot)?;
            Some((*source_slot, target))
        });
        let Some((slot, InstalledBindingTarget::Function(function))) = binding else {
            self.finish_code_error(
                vm,
                op,
                "the module instance has no function binding with this name",
            );
            return;
        };
        self.finish_function_binding_lookup(vm, op, handle, slot, function);
    }

    fn code_class_binding(&mut self, vm: VmId, op: u32, value: Value, name: Value) {
        let handle = match self.code_handle(vm, value, CodeHandleKind::Instance) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not an Instance");
                return;
            }
        };
        let Some(name) = self.code_name(vm, op, name, "class") else {
            return;
        };
        let binding = self.live_instance(handle).and_then(|instance| {
            let source =
                lm_bytecode::decode_with_bundle(instance.artifact.as_slice(), self.loaded.bundle())
                    .ok()?;
            let export = source
                .exports
                .iter()
                .find(|export| export.name == name && export.kind.is_class())?;
            installed_binding(
                self.loaded.bundle(),
                instance,
                PortableCodeKind::Class,
                export.def,
            )
        });
        let Some((slot, InstalledBindingTarget::Class { .. })) = binding else {
            self.finish_code_error(
                vm,
                op,
                "the module instance has no class binding with this name",
            );
            return;
        };
        self.finish_binding_lookup(vm, op, handle, CodeHandleKind::ClassBinding, slot);
    }

    fn finish_function_binding_lookup(
        &mut self,
        vm: VmId,
        op: u32,
        instance: CodeHandle,
        slot: u32,
        function: u32,
    ) {
        let Some(function_class) = self.core.function_binding else {
            self.machines[vm as usize].set_fault(FaultCode::MalformedState, "", Some(op));
            return;
        };
        let matches = self
            .requested_function_contract(vm, function_class)
            .and_then(|(input, output)| self.function_matches_contract(function, input, output));
        match matches {
            Ok(true) => {
                self.finish_binding_lookup(vm, op, instance, CodeHandleKind::FunctionBinding, slot)
            }
            Ok(false) => self.finish_code_error(
                vm,
                op,
                "the binding does not match the requested monomorphic contract",
            ),
            Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
        }
    }

    fn finish_binding_lookup(
        &mut self,
        vm: VmId,
        op: u32,
        instance: CodeHandle,
        kind: CodeHandleKind,
        slot: u32,
    ) {
        let value = self.machines[vm as usize].alloc(Object::NativeCodeHandle {
            image: instance.image,
            generation: instance.generation,
            instance: instance.instance,
            kind,
            index: slot,
        });
        let result = value.and_then(|value| self.code_ok(vm, value));
        self.finish_code_result(vm, op, result);
    }

    fn code_binding_slot(&mut self, vm: VmId, op: u32, value: Value) {
        let handle = match self.binding_handle(vm, value) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not an installed binding");
                return;
            }
        };
        let Some(slot) = self.live_binding_slot(handle) else {
            self.finish_code_error(vm, op, "the installed binding is not live");
            return;
        };
        let value = self.machines[vm as usize].alloc(Object::NativeCodeHandle {
            image: handle.image,
            generation: handle.generation,
            instance: handle.instance,
            kind: CodeHandleKind::Slot,
            index: slot,
        });
        let result = value.and_then(|value| self.code_ok(vm, value));
        self.finish_code_result(vm, op, result);
    }

    fn code_binding_spec(&mut self, vm: VmId, op: u32, value: Value) {
        let handle = match self.binding_handle(vm, value) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not an installed binding");
                return;
            }
        };
        let source = self
            .live_binding_source(handle)
            .map(|(instance, source_slot)| {
                (
                    instance.artifact.clone(),
                    instance.interface.clone(),
                    source_slot,
                )
            });
        let Some((bytes, interface, index)) = source else {
            self.finish_code_error(vm, op, "the installed binding is not live");
            return;
        };
        let value = self.machines[vm as usize].alloc(Object::NativeCode(Box::new(PortableCode {
            kind: PortableCodeKind::SlotSpec,
            bytes,
            interface,
            index,
            origin: None,
        })));
        let result = value.and_then(|value| self.code_ok(vm, value));
        self.finish_code_result(vm, op, result);
    }

    fn code_binding_instance(&mut self, vm: VmId, op: u32, value: Value) {
        let handle = match self.binding_handle(vm, value) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not an installed binding");
                return;
            }
        };
        if self.live_binding_target(handle).is_none() {
            self.finish_code_error(vm, op, "the installed binding is not live");
            return;
        }
        let value = self.machines[vm as usize].alloc(Object::NativeCodeHandle {
            image: handle.image,
            generation: handle.generation,
            instance: handle.instance,
            kind: CodeHandleKind::Instance,
            index: handle.instance,
        });
        let result = value.and_then(|value| self.code_ok(vm, value));
        self.finish_code_result(vm, op, result);
    }

    fn code_binding_function_target(&mut self, vm: VmId, op: u32, value: Value) {
        let handle = match self.code_handle(vm, value, CodeHandleKind::FunctionBinding) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not a FunctionBinding");
                return;
            }
        };
        let Some(InstalledBindingTarget::Function(function)) = self.live_binding_target(handle)
        else {
            self.finish_code_error(vm, op, "the function binding is not live");
            return;
        };
        let value = self.machines[vm as usize].alloc(Object::NativeCodeHandle {
            image: handle.image,
            generation: handle.generation,
            instance: handle.instance,
            kind: CodeHandleKind::Function,
            index: function,
        });
        let result = value.and_then(|value| self.code_ok(vm, value));
        self.finish_code_result(vm, op, result);
    }

    fn code_binding_class_target(&mut self, vm: VmId, op: u32, value: Value) {
        let handle = match self.code_handle(vm, value, CodeHandleKind::ClassBinding) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not a ClassBinding");
                return;
            }
        };
        let Some(InstalledBindingTarget::Class { class, .. }) = self.live_binding_target(handle)
        else {
            self.finish_code_error(vm, op, "the class binding is not live");
            return;
        };
        let value = self.machines[vm as usize].alloc(Object::NativeCodeHandle {
            image: handle.image,
            generation: handle.generation,
            instance: handle.instance,
            kind: CodeHandleKind::Class,
            index: class,
        });
        let result = value.and_then(|value| self.code_ok(vm, value));
        self.finish_code_result(vm, op, result);
    }

    fn finish_function_lookup(&mut self, vm: VmId, op: u32, instance: CodeHandle, function: u32) {
        let Some(function_class) = self.core.function_def else {
            self.machines[vm as usize].set_fault(FaultCode::MalformedState, "", Some(op));
            return;
        };
        let contract = self.requested_function_contract(vm, function_class);
        let matches = contract
            .and_then(|(input, output)| self.function_matches_contract(function, input, output));
        match matches {
            Ok(true) => {
                let value = self.machines[vm as usize].alloc(Object::NativeCodeHandle {
                    image: instance.image,
                    generation: instance.generation,
                    instance: instance.instance,
                    kind: CodeHandleKind::Function,
                    index: function,
                });
                let result = value.and_then(|value| self.code_ok(vm, value));
                self.finish_code_result(vm, op, result);
            }
            Ok(false) => {
                let value = self.code_error(
                    vm,
                    "the function does not match the requested monomorphic contract",
                );
                self.finish_code_result(vm, op, value);
            }
            Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
        }
    }

    fn finish_portable_function_lookup(
        &mut self,
        vm: VmId,
        op: u32,
        code: PortableCode,
        function: u32,
        origin: Option<[u8; 32]>,
    ) {
        let Some(function_class) = self.core.function_code else {
            self.machines[vm as usize].set_fault(FaultCode::MalformedState, "", Some(op));
            return;
        };
        let contract = self.requested_function_contract(vm, function_class);
        let source = lm_bytecode::decode_with_bundle(code.bytes.as_slice(), self.loaded.bundle())
            .map_err(|_| FaultCode::MalformedState);
        let matches = match (contract, source) {
            (Ok((input, output)), Ok(source)) => {
                self.portable_function_matches_contract(&source, function, input, output)
            }
            (Err(code), _) | (_, Err(code)) => Err(code),
        };
        match matches {
            Ok(true) => {
                let value =
                    self.machines[vm as usize].alloc(Object::NativeCode(Box::new(PortableCode {
                        kind: PortableCodeKind::Function,
                        bytes: code.bytes,
                        interface: code.interface,
                        index: function,
                        origin,
                    })));
                let result = value.and_then(|value| self.code_ok(vm, value));
                self.finish_code_result(vm, op, result);
            }
            Ok(false) => self.finish_code_error(
                vm,
                op,
                "the function does not match the requested monomorphic contract",
            ),
            Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
        }
    }

    fn code_slot_spec(&mut self, vm: VmId, op: u32, value: Value, name: Value) {
        let handle = match self.code_handle(vm, value, CodeHandleKind::Instance) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not an Instance");
                return;
            }
        };
        let name = match name
            .as_obj()
            .map(|reference| self.machines[vm as usize].vm.heap.get(reference))
        {
            Some(Object::Str(text)) => text.as_str().to_string(),
            _ => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::TypeMismatch,
                    "the slot name is not String",
                );
                return;
            }
        };
        let Some(instance) = self.live_instance(handle) else {
            self.finish_code_error(vm, op, "the module instance is not live");
            return;
        };
        let interface_bytes = instance.interface.clone();
        let artifact = instance.artifact.clone();
        let module =
            match lm_bytecode::decode_with_bundle(artifact.as_slice(), self.loaded.bundle()) {
                Ok(module) => module,
                Err(error) => {
                    self.finish_code_error(
                        vm,
                        op,
                        &format!("the module artifact did not decode: {error}"),
                    );
                    return;
                }
            };
        let interface_key = match interface_bytes.as_ref() {
            Some(bytes) => match lm_bytecode::interface::decode_interface(bytes.as_slice()) {
                Ok(interface) => {
                    let qualified = lm_bytecode::qualified_key(&interface.module_path, &name);
                    interface
                        .slots
                        .iter()
                        .find(|spec| spec.binding == name)
                        .or_else(|| {
                            interface
                                .slots
                                .iter()
                                .find(|spec| spec.binding == qualified)
                        })
                        .map(|spec| spec.key)
                }
                Err(error) => {
                    self.finish_code_error(
                        vm,
                        op,
                        &format!("the module interface did not decode: {error}"),
                    );
                    return;
                }
            },
            None => None,
        };
        let exported_index = module.exports.iter().find_map(|export| {
            if export.name != name {
                return None;
            }
            let target = if export.kind.is_class() {
                if export.ctor == lm_bytecode::NO_CTOR {
                    return None;
                }
                lm_bytecode::SlotTarget::Class {
                    class: export.def,
                    constructor: export.ctor,
                }
            } else if export.kind == lm_bytecode::ExportKind::Function {
                lm_bytecode::SlotTarget::Function(export.def)
            } else {
                return None;
            };
            module
                .slots
                .iter()
                .position(|slot| slot.initial == Some(target))
        });
        let ad_hoc_key = lm_bytecode::ad_hoc_slot_key(&name);
        let index = interface_key
            .and_then(|key| module.slots.iter().position(|slot| slot.key == key))
            .or(exported_index)
            .or_else(|| module.slots.iter().position(|slot| slot.key == ad_hoc_key));
        let Some(index) = index else {
            self.finish_code_error(vm, op, "the module instance has no slot with this name");
            return;
        };
        let Ok(index) = u32::try_from(index) else {
            self.finish_code_error(vm, op, "the module slot index is too large");
            return;
        };
        let value = self.machines[vm as usize].alloc(Object::NativeCode(Box::new(PortableCode {
            kind: PortableCodeKind::SlotSpec,
            bytes: artifact,
            interface: interface_bytes,
            index,
            origin: None,
        })));
        let result = value.and_then(|value| self.code_ok(vm, value));
        self.finish_code_result(vm, op, result);
    }

    fn code_slot_for(&mut self, vm: VmId, op: u32, value: Value, spec: Value) {
        let handle = match self.code_handle(vm, value, CodeHandleKind::Instance) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not an Instance");
                return;
            }
        };
        let portable = match self.portable_code(vm, spec, PortableCodeKind::SlotSpec) {
            Ok(portable) => portable,
            Err(code) => {
                self.fault_caller(vm, op, code, "the slot specification has another shape");
                return;
            }
        };
        let source = match lm_bytecode::decode_with_bundle(
            portable.bytes.as_slice(),
            self.loaded.bundle(),
        ) {
            Ok(source) => source,
            Err(error) => {
                self.finish_code_error(
                    vm,
                    op,
                    &format!("the slot specification did not decode: {error}"),
                );
                return;
            }
        };
        let Some(wanted) = source.slots.get(portable.index as usize) else {
            self.finish_code_error(vm, op, "the slot specification has an invalid index");
            return;
        };
        let Some(instance) = self.live_instance(handle) else {
            self.finish_code_error(vm, op, "the module instance is not live");
            return;
        };
        let target_artifact = instance.artifact.clone();
        let target_interface = instance.interface.clone();
        let target_slots = instance.slots.clone();
        let target =
            match lm_bytecode::decode_with_bundle(target_artifact.as_slice(), self.loaded.bundle())
            {
                Ok(target) => target,
                Err(error) => {
                    self.finish_code_error(
                        vm,
                        op,
                        &format!("the module artifact did not decode: {error}"),
                    );
                    return;
                }
            };
        let target_index = target.slots.iter().position(|slot| slot.key == wanted.key);
        let source_contract = portable.interface.as_ref().and_then(|bytes| {
            lm_bytecode::interface::decode_interface(bytes.as_slice())
                .ok()?
                .slots
                .into_iter()
                .find(|slot| slot.key == wanted.key)
                .map(|slot| slot.contract_hash)
        });
        let target_contract = target_interface.as_ref().and_then(|bytes| {
            lm_bytecode::interface::decode_interface(bytes.as_slice())
                .ok()?
                .slots
                .into_iter()
                .find(|slot| slot.key == wanted.key)
                .map(|slot| slot.contract_hash)
        });
        let compatible = portable.bytes.as_slice() == target_artifact.as_slice()
            || matches!((source_contract, target_contract), (Some(left), Some(right)) if left == right);
        let target_index = if compatible { target_index } else { None };
        let mapped = target_index.and_then(|index| target_slots.get(index).copied());
        let Some(mapped) = mapped else {
            self.finish_code_error(vm, op, "the module instance has no compatible slot");
            return;
        };
        let value = self.machines[vm as usize].alloc(Object::NativeCodeHandle {
            image: handle.image,
            generation: handle.generation,
            instance: handle.instance,
            kind: CodeHandleKind::Slot,
            index: mapped,
        });
        let result = value.and_then(|value| self.code_ok(vm, value));
        self.finish_code_result(vm, op, result);
    }

    fn code_activate(
        &mut self,
        vm: VmId,
        op: u32,
        image: Value,
        definition: Value,
        arguments: Value,
    ) {
        let Some(key) = self.image_arg(vm, op, image) else {
            return;
        };
        let handle = self
            .code_handle(vm, definition, CodeHandleKind::Function)
            .or_else(|_| self.code_handle(vm, definition, CodeHandleKind::FunctionBinding));
        let handle = match handle {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the program is not an installed function");
                return;
            }
        };
        if handle.image_key() != key {
            self.finish_code_error(vm, op, "the function does not belong to this VM image");
            return;
        }
        let function = if handle.kind == CodeHandleKind::Function {
            if !self.live_function(handle) {
                self.finish_code_error(vm, op, "the function does not belong to this VM image");
                return;
            }
            handle.index
        } else {
            let Some(slot) = self.live_binding_slot(handle) else {
                self.finish_code_error(vm, op, "the function binding is not live");
                return;
            };
            match self.vm_images[key.image as usize].slots.get(slot as usize) {
                Some(ImageSlotTarget::Function(function)) => *function,
                _ => {
                    self.finish_code_error(vm, op, "the function binding has no current target");
                    return;
                }
            }
        };
        let values = match arguments {
            Value::Unit => Vec::new(),
            Value::Obj(reference) => match self.machines[vm as usize].vm.heap.get(reference) {
                Object::Tuple { items } => items.clone(),
                _ => {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument view is not a tuple",
                    );
                    return;
                }
            },
            _ => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::TypeMismatch,
                    "the argument view is not unit or a tuple",
                );
                return;
            }
        };
        let target = match self.prepare_run_target(vm, key) {
            Some(target) => target,
            None => {
                self.finish_code_error(vm, op, "the VM image has no run capacity");
                return;
            }
        };
        let locals = match self.transfer_all(vm, target, &values) {
            Ok(values) => values,
            Err(_) => {
                self.rollback_run_target(vm, target);
                self.finish_code_error(vm, op, "an argument cannot enter the VM image");
                return;
            }
        };
        if self
            .check_frame_args(target, function, lm_value::TypeEnvId::EMPTY, &locals)
            .is_err()
        {
            self.rollback_run_target(vm, target);
            self.finish_code_error(vm, op, "an argument has the wrong type");
            return;
        }
        self.machines[target as usize].load_frame(
            &self.module,
            function,
            locals,
            None,
            lm_value::TypeEnvId::EMPTY,
        );
        let result = self.machines[vm as usize]
            .alloc(Object::NativeRun { vm: target })
            .and_then(|value| self.code_ok(vm, value));
        if result.is_err() {
            self.rollback_run_target(vm, target);
        }
        self.finish_code_result(vm, op, result);
    }

    fn replacement_address(
        &self,
        vm: VmId,
        key: VmImageKey,
        value: Value,
    ) -> Result<u32, &'static str> {
        let slot = self
            .code_handle(vm, value, CodeHandleKind::Slot)
            .or_else(|_| self.binding_handle(vm, value))
            .map_err(|_| "the replacement address is not a binding")?;
        if slot.image_key() != key {
            return Err("the slot does not belong to this VM image");
        }
        if slot.kind == CodeHandleKind::Slot {
            self.live_slot(slot)
                .then_some(slot.index)
                .ok_or("the slot does not belong to this VM image")
        } else {
            self.live_binding_slot(slot)
                .ok_or("the slot does not belong to this VM image")
        }
    }

    fn replacement_function(
        &self,
        vm: VmId,
        key: VmImageKey,
        target: Value,
    ) -> Result<u32, String> {
        match self
            .code_handle(vm, target, CodeHandleKind::Function)
            .or_else(|_| self.code_handle(vm, target, CodeHandleKind::FunctionBinding))
        {
            Ok(handle) => {
                if handle.image_key() != key {
                    return Err("the function does not belong to this VM image".to_string());
                }
                if handle.kind == CodeHandleKind::Function {
                    self.live_function(handle)
                        .then_some(handle.index)
                        .ok_or_else(|| "the function does not belong to this VM image".to_string())
                } else {
                    match self.live_binding_target(handle) {
                        Some(InstalledBindingTarget::Function(function)) => Ok(function),
                        _ => Err("the function binding is not live".to_string()),
                    }
                }
            }
            Err(_) => self.function_value_target(vm, target),
        }
    }

    fn replacement_class(
        &self,
        vm: VmId,
        key: VmImageKey,
        target: Value,
    ) -> Result<(u32, u32), &'static str> {
        let target = self
            .code_handle(vm, target, CodeHandleKind::Class)
            .or_else(|_| self.code_handle(vm, target, CodeHandleKind::ClassBinding))
            .map_err(|_| "the replacement target is not a class")?;
        if target.image_key() != key {
            return Err("the class does not belong to this VM image");
        }
        if target.kind == CodeHandleKind::Class {
            if !self.live_class(target) {
                return Err("the class does not belong to this VM image");
            }
            self.live_class_constructor(target)
                .map(|constructor| (target.index, constructor))
                .ok_or("the class has no live constructor")
        } else {
            match self.live_binding_target(target) {
                Some(InstalledBindingTarget::Class { class, constructor }) => {
                    Ok((class, constructor))
                }
                _ => Err("the class binding is not live"),
            }
        }
    }

    fn code_change(&mut self, vm: VmId, op: u32, image: Value, slot: Value, target: Value) {
        let Some(key) = self.image_arg(vm, op, image) else {
            return;
        };
        let slot = match self.replacement_address(vm, key, slot) {
            Ok(slot) => slot,
            Err(message) => {
                self.finish_code_error(vm, op, message);
                return;
            }
        };
        let kind = match op {
            lm_abi::OP_VM_CHANGE_FUNCTION => {
                let function = match self.replacement_function(vm, key, target) {
                    Ok(function) => function,
                    Err(message) => {
                        self.finish_code_error(vm, op, &message);
                        return;
                    }
                };
                if self.validate_function_slot(key, slot, function).is_err() {
                    self.finish_code_error(
                        vm,
                        op,
                        "the replacement target does not match the slot contract",
                    );
                    return;
                }
                lm_heap::SlotChangeKind::Function
            }
            lm_abi::OP_VM_CHANGE_CLASS => {
                let (class, constructor) = match self.replacement_class(vm, key, target) {
                    Ok(target) => target,
                    Err(message) => {
                        self.finish_code_error(vm, op, message);
                        return;
                    }
                };
                if self
                    .validate_class_slot(key, slot, class, constructor)
                    .is_err()
                {
                    self.finish_code_error(
                        vm,
                        op,
                        "the replacement target does not match the slot contract",
                    );
                    return;
                }
                lm_heap::SlotChangeKind::Class
            }
            lm_abi::OP_VM_CHANGE_VALUE => {
                if self.validate_value_slot(slot, vm, target).is_err() {
                    self.finish_code_error(
                        vm,
                        op,
                        "the replacement target does not match the slot contract",
                    );
                    return;
                }
                lm_heap::SlotChangeKind::Value
            }
            lm_abi::OP_VM_CHANGE_PROCESS => {
                if self.validate_process_slot(slot, vm, target).is_err() {
                    self.finish_code_error(
                        vm,
                        op,
                        "the replacement target does not match the slot contract",
                    );
                    return;
                }
                lm_heap::SlotChangeKind::Process
            }
            _ => {
                self.machines[vm as usize].set_fault(FaultCode::MalformedState, "", Some(op));
                return;
            }
        };
        let version = match self.slot_version(key, slot) {
            Ok(version) if version.checked_add(1).is_some() => version,
            _ => {
                self.finish_code_error(vm, op, "the slot version is exhausted");
                return;
            }
        };
        let value = self.machines[vm as usize]
            .alloc(Object::NativeSlotChange {
                image: key.image,
                generation: key.generation,
                slot,
                version,
                kind,
                target,
            })
            .and_then(|value| self.code_ok(vm, value));
        self.finish_code_result(vm, op, value);
    }

    fn code_replace_all(&mut self, vm: VmId, op: u32, image: Value, values: Value) {
        let Some(key) = self.image_arg(vm, op, image) else {
            return;
        };
        let values = match values
            .as_obj()
            .map(|reference| self.machines[vm as usize].vm.heap.get(reference))
        {
            Some(Object::List { items, .. }) => items.clone(),
            _ => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::TypeMismatch,
                    "the changes are not a List",
                );
                return;
            }
        };
        let mut changes = Vec::new();
        if changes.try_reserve_exact(values.len()).is_err() {
            self.machines[vm as usize].set_fault(FaultCode::HeapLimit, "", Some(op));
            return;
        }
        let mut seen = BTreeSet::new();
        for value in values {
            let change = value
                .as_obj()
                .map(|reference| self.machines[vm as usize].vm.heap.get(reference));
            let Some(Object::NativeSlotChange {
                image,
                generation,
                slot,
                version,
                kind,
                target,
            }) = change
            else {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::TypeMismatch,
                    "the change list contains another value",
                );
                return;
            };
            if *image != key.image || *generation != key.generation {
                self.finish_code_error(vm, op, "a change belongs to another VM image");
                return;
            }
            if !seen.insert(*slot) {
                self.finish_code_error(vm, op, "the batch changes one slot twice");
                return;
            }
            if self.slot_version(key, *slot) != Ok(*version) {
                self.finish_code_error(vm, op, "a slot change is stale");
                return;
            }
            changes.push((*slot, *version, *kind, *target));
        }
        if changes.is_empty() {
            let value = self.code_ok(vm, Value::Unit);
            self.finish_code_result(vm, op, value);
            return;
        }
        if self.check_slot_safepoint(key).is_err() {
            self.finish_code_error(vm, op, "the VM image is not at a safe replacement point");
            return;
        }

        let mut prepared = Vec::new();
        if prepared.try_reserve_exact(changes.len()).is_err() {
            self.machines[vm as usize].set_fault(FaultCode::HeapLimit, "", Some(op));
            return;
        }
        for (slot, version, kind, target) in &changes {
            let result = match kind {
                lm_heap::SlotChangeKind::Function => self
                    .replacement_function(vm, key, *target)
                    .map_err(|_| FaultCode::TypeMismatch)
                    .and_then(|function| {
                        self.validate_function_slot(key, *slot, function)?;
                        Ok(PreparedSlotTarget::Ready(ImageSlotTarget::Function(
                            function,
                        )))
                    }),
                lm_heap::SlotChangeKind::Class => self
                    .replacement_class(vm, key, *target)
                    .map_err(|_| FaultCode::TypeMismatch)
                    .and_then(|(class, constructor)| {
                        self.validate_class_slot(key, *slot, class, constructor)?;
                        Ok(PreparedSlotTarget::Ready(ImageSlotTarget::Class {
                            class,
                            constructor,
                        }))
                    }),
                lm_heap::SlotChangeKind::Value => self
                    .validate_value_slot(*slot, vm, *target)
                    .map(|()| PreparedSlotTarget::Value(*target)),
                lm_heap::SlotChangeKind::Process => self
                    .validate_process_slot(*slot, vm, *target)
                    .map(PreparedSlotTarget::Ready),
            };
            match result {
                Ok(target) => prepared.push((*slot, *version, target)),
                Err(_) => {
                    self.finish_code_error(
                        vm,
                        op,
                        "a replacement target does not match its slot contract",
                    );
                    return;
                }
            }
        }

        let mut staged_roots = Vec::new();
        let mut committed = Vec::new();
        for (slot, version, target) in prepared {
            let target = match target {
                PreparedSlotTarget::Ready(target) => target,
                PreparedSlotTarget::Value(value) => {
                    let moved = match self.stage_value_target(key, vm, value, &staged_roots) {
                        Ok(value) => value,
                        Err(_) => {
                            self.finish_code_error(
                                vm,
                                op,
                                "a replacement value cannot enter the VM image",
                            );
                            return;
                        }
                    };
                    if let Value::Obj(reference) = moved {
                        staged_roots.push(reference);
                    }
                    ImageSlotTarget::Value(moved)
                }
            };
            committed.push((slot, version, target));
        }
        match self.commit_slot_targets(key, &committed) {
            Ok(()) => {
                let value = self.code_ok(vm, Value::Unit);
                self.finish_code_result(vm, op, value);
            }
            Err(_) => self.finish_code_error(vm, op, "a slot change became stale"),
        }
    }

    fn code_replace(&mut self, vm: VmId, op: u32, image: Value, slot: Value, target: Value) {
        let Some(key) = self.image_arg(vm, op, image) else {
            return;
        };
        let slot = match self
            .code_handle(vm, slot, CodeHandleKind::Slot)
            .or_else(|_| self.binding_handle(vm, slot))
        {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the replacement address is not a binding");
                return;
            }
        };
        let slot_index = if slot.kind == CodeHandleKind::Slot {
            self.live_slot(slot).then_some(slot.index)
        } else {
            self.live_binding_slot(slot)
        };
        if slot.image_key() != key {
            self.finish_code_error(vm, op, "the slot does not belong to this VM image");
            return;
        }
        let Some(slot_index) = slot_index else {
            self.finish_code_error(vm, op, "the slot does not belong to this VM image");
            return;
        };
        let replaced = match op {
            lm_abi::OP_VM_REPLACE_FUNCTION => {
                let function = match self
                    .code_handle(vm, target, CodeHandleKind::Function)
                    .or_else(|_| self.code_handle(vm, target, CodeHandleKind::FunctionBinding))
                {
                    Ok(handle) => {
                        let function = if handle.kind == CodeHandleKind::Function {
                            self.live_function(handle).then_some(handle.index)
                        } else {
                            match self.live_binding_target(handle) {
                                Some(InstalledBindingTarget::Function(function)) => Some(function),
                                _ => None,
                            }
                        };
                        if handle.image_key() != key {
                            self.finish_code_error(
                                vm,
                                op,
                                "the function does not belong to this VM image",
                            );
                            return;
                        }
                        let Some(function) = function else {
                            self.finish_code_error(
                                vm,
                                op,
                                "the function does not belong to this VM image",
                            );
                            return;
                        };
                        function
                    }
                    Err(_) => match self.function_value_target(vm, target) {
                        Ok(function) => function,
                        Err(message) => {
                            self.finish_code_error(vm, op, &message);
                            return;
                        }
                    },
                };
                self.replace_function_slot(key, slot_index, function)
            }
            lm_abi::OP_VM_REPLACE_CLASS => {
                let target = match self
                    .code_handle(vm, target, CodeHandleKind::Class)
                    .or_else(|_| self.code_handle(vm, target, CodeHandleKind::ClassBinding))
                {
                    Ok(handle) => handle,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the replacement target is not a class");
                        return;
                    }
                };
                let replacement = if target.kind == CodeHandleKind::Class {
                    self.live_class(target)
                        .then(|| {
                            self.live_class_constructor(target)
                                .map(|ctor| (target.index, ctor))
                        })
                        .flatten()
                } else {
                    match self.live_binding_target(target) {
                        Some(InstalledBindingTarget::Class { class, constructor }) => {
                            Some((class, constructor))
                        }
                        _ => None,
                    }
                };
                if target.image_key() != key {
                    self.finish_code_error(vm, op, "the class does not belong to this VM image");
                    return;
                }
                let Some((class, constructor)) = replacement else {
                    self.finish_code_error(vm, op, "the class does not belong to this VM image");
                    return;
                };
                self.replace_class_slot(key, slot_index, class, constructor)
            }
            lm_abi::OP_VM_REPLACE_VALUE => self.replace_value_slot(key, slot_index, vm, target),
            lm_abi::OP_VM_REPLACE_PROCESS => self.replace_process_slot(key, slot_index, vm, target),
            _ => Err(FaultCode::MalformedState),
        };
        match replaced {
            Ok(()) => {
                let value = self.code_ok(vm, Value::Unit);
                self.finish_code_result(vm, op, value);
            }
            Err(FaultCode::InvalidVmState) => {
                self.finish_code_error(vm, op, "the VM image is not at a safe replacement point")
            }
            Err(_) => self.finish_code_error(
                vm,
                op,
                "the replacement target does not match the slot contract",
            ),
        }
    }

    fn portable_code(
        &self,
        vm: VmId,
        value: Value,
        expected: PortableCodeKind,
    ) -> Result<PortableCode, FaultCode> {
        let reference = value.as_obj().ok_or(FaultCode::TypeMismatch)?;
        match self.machines[vm as usize].vm.heap.get(reference) {
            Object::NativeCode(code) if code.kind == expected => Ok((**code).clone()),
            _ => Err(FaultCode::TypeMismatch),
        }
    }

    fn code_handle(
        &self,
        vm: VmId,
        value: Value,
        expected: CodeHandleKind,
    ) -> Result<CodeHandle, FaultCode> {
        let reference = value.as_obj().ok_or(FaultCode::TypeMismatch)?;
        match self.machines[vm as usize].vm.heap.get(reference) {
            Object::NativeCodeHandle {
                image,
                generation,
                instance,
                kind,
                index,
            } if *kind == expected => Ok(CodeHandle {
                image: *image,
                generation: *generation,
                instance: *instance,
                kind: *kind,
                index: *index,
            }),
            _ => Err(FaultCode::TypeMismatch),
        }
    }

    fn binding_handle(&self, vm: VmId, value: Value) -> Result<CodeHandle, FaultCode> {
        self.code_handle(vm, value, CodeHandleKind::FunctionBinding)
            .or_else(|_| self.code_handle(vm, value, CodeHandleKind::ClassBinding))
    }

    fn live_binding_source(&self, handle: CodeHandle) -> Option<(&InstalledInstance, u32)> {
        if !matches!(
            handle.kind,
            CodeHandleKind::FunctionBinding | CodeHandleKind::ClassBinding
        ) {
            return None;
        }
        let instance = self
            .vm_images
            .get(handle.image as usize)
            .filter(|image| image.live && image.generation == handle.generation)?
            .instances
            .get(handle.instance as usize)?;
        instance.slots.get(handle.index as usize)?;
        let source_slot = handle.index;
        let target = installed_binding_target(instance, handle.kind, source_slot)?;
        match (handle.kind, target) {
            (CodeHandleKind::FunctionBinding, InstalledBindingTarget::Function(_))
            | (CodeHandleKind::ClassBinding, InstalledBindingTarget::Class { .. }) => {
                Some((instance, source_slot))
            }
            _ => None,
        }
    }

    fn live_binding_target(&self, handle: CodeHandle) -> Option<InstalledBindingTarget> {
        let (instance, _) = self.live_binding_source(handle)?;
        installed_binding_target(instance, handle.kind, handle.index)
    }

    fn live_binding_slot(&self, handle: CodeHandle) -> Option<u32> {
        let (instance, source_slot) = self.live_binding_source(handle)?;
        instance.slots.get(source_slot as usize).copied()
    }

    fn live_instance(&self, handle: CodeHandle) -> Option<&InstalledInstance> {
        if handle.kind != CodeHandleKind::Instance || handle.index != handle.instance {
            return None;
        }
        self.vm_images
            .get(handle.image as usize)
            .filter(|image| image.live && image.generation == handle.generation)
            .and_then(|image| image.instances.get(handle.instance as usize))
    }

    fn live_function(&self, handle: CodeHandle) -> bool {
        if handle.kind != CodeHandleKind::Function {
            return false;
        }
        self.vm_images
            .get(handle.image as usize)
            .filter(|image| image.live && image.generation == handle.generation)
            .and_then(|image| image.instances.get(handle.instance as usize))
            .is_some_and(|instance| instance.funcs.contains(&handle.index))
    }

    fn live_class(&self, handle: CodeHandle) -> bool {
        if handle.kind != CodeHandleKind::Class {
            return false;
        }
        self.vm_images
            .get(handle.image as usize)
            .filter(|image| image.live && image.generation == handle.generation)
            .and_then(|image| image.instances.get(handle.instance as usize))
            .is_some_and(|instance| instance.classes.contains(&handle.index))
    }

    fn live_class_constructor(&self, handle: CodeHandle) -> Option<u32> {
        if handle.kind != CodeHandleKind::Class {
            return None;
        }
        let instance = self
            .vm_images
            .get(handle.image as usize)
            .filter(|image| image.live && image.generation == handle.generation)?
            .instances
            .get(handle.instance as usize)?;
        let source =
            lm_bytecode::decode_with_bundle(instance.artifact.as_slice(), self.loaded.bundle())
                .ok()?;
        let source_class = instance
            .classes
            .iter()
            .position(|class| *class == handle.index)?;
        let constructor = source.slots.iter().find_map(|slot| match slot.initial {
            Some(lm_bytecode::SlotTarget::Class { class, constructor })
                if class as usize == source_class =>
            {
                Some(constructor)
            }
            _ => None,
        });
        let constructor = constructor.or_else(|| {
            source.exports.iter().find_map(|export| {
                (export.kind.is_class()
                    && export.def as usize == source_class
                    && export.ctor != lm_bytecode::NO_CTOR)
                    .then_some(export.ctor)
            })
        })?;
        instance.funcs.get(constructor as usize).copied()
    }

    fn live_slot(&self, handle: CodeHandle) -> bool {
        if handle.kind != CodeHandleKind::Slot {
            return false;
        }
        self.vm_images
            .get(handle.image as usize)
            .filter(|image| image.live && image.generation == handle.generation)
            .and_then(|image| image.instances.get(handle.instance as usize))
            .is_some_and(|instance| instance.slots.contains(&handle.index))
    }

    fn requested_function_contract(
        &mut self,
        vm: VmId,
        function_class: u32,
    ) -> Result<(ClosedTypeId, ClosedTypeId), FaultCode> {
        let result_class = self.core.result.ok_or(FaultCode::MalformedState)?;
        let (reply, env) = self.reply_type(vm)?;
        let closed = self
            .envs
            .close(&self.module, reply, env)
            .map_err(|_| FaultCode::BoundaryLimit)?;
        let function = match self.envs.ty(closed).cloned() {
            Some(ClosedType::Inst(class, args)) if class == result_class && args.len() == 2 => {
                args[0]
            }
            _ => return Err(FaultCode::MalformedState),
        };
        match self.envs.ty(function).cloned() {
            Some(ClosedType::Inst(class, args)) if class == function_class && args.len() == 2 => {
                Ok((args[0], args[1]))
            }
            _ => Err(FaultCode::MalformedState),
        }
    }

    fn function_matches_contract(
        &mut self,
        function: u32,
        input: ClosedTypeId,
        output: ClosedTypeId,
    ) -> Result<bool, FaultCode> {
        let code = self
            .module
            .funcs
            .get(function as usize)
            .cloned()
            .ok_or(FaultCode::MalformedState)?;
        if code.type_params != 0
            || code.effect_params != 0
            || !code.captures.is_empty()
            || code.param_muts.iter().any(|marker| *marker)
        {
            return Ok(false);
        }
        let mut params = Vec::with_capacity(code.params.len());
        for parameter in code.params {
            params.push(
                self.envs
                    .close(&self.module, parameter, lm_value::TypeEnvId::EMPTY)
                    .map_err(|_| FaultCode::BoundaryLimit)?,
            );
        }
        let actual_input = if params.is_empty() {
            self.envs
                .intern(ClosedType::Unit)
                .map_err(|_| FaultCode::BoundaryLimit)?
        } else {
            self.envs
                .intern(ClosedType::Tuple(params))
                .map_err(|_| FaultCode::BoundaryLimit)?
        };
        let actual_output = self
            .envs
            .close(&self.module, code.ret, lm_value::TypeEnvId::EMPTY)
            .map_err(|_| FaultCode::BoundaryLimit)?;
        let identity = self.identity()?.clone();
        let space = ClosedTypeSpace {
            module: &self.module,
            types: &self.envs,
            identity: &identity,
        };
        Ok(
            closed_types_match(self.loaded.bundle(), space, actual_input, space, input)
                && closed_types_match(self.loaded.bundle(), space, actual_output, space, output),
        )
    }

    fn portable_function_matches_contract(
        &mut self,
        module: &lm_bytecode::Module,
        function: u32,
        input: ClosedTypeId,
        output: ClosedTypeId,
    ) -> Result<bool, FaultCode> {
        let code = module
            .funcs
            .get(function as usize)
            .ok_or(FaultCode::MalformedState)?;
        if code.type_params != 0
            || code.effect_params != 0
            || !code.captures.is_empty()
            || code.param_muts.iter().any(|marker| *marker)
        {
            return Ok(false);
        }
        let mut source_types = lm_bytecode::closed::TypeEnvs::default();
        let mut parameters = Vec::with_capacity(code.params.len());
        for parameter in &code.params {
            parameters.push(
                source_types
                    .close(module, *parameter, TypeEnvId::EMPTY)
                    .map_err(|_| FaultCode::BoundaryLimit)?,
            );
        }
        let source_input = if parameters.is_empty() {
            source_types
                .intern(ClosedType::Unit)
                .map_err(|_| FaultCode::BoundaryLimit)?
        } else {
            source_types
                .intern(ClosedType::Tuple(parameters))
                .map_err(|_| FaultCode::BoundaryLimit)?
        };
        let source_output = source_types
            .close(module, code.ret, TypeEnvId::EMPTY)
            .map_err(|_| FaultCode::BoundaryLimit)?;
        let source_identity =
            lm_bytecode::identity::module_identity_with_bundle(module, self.loaded.bundle())
                .map_err(|_| FaultCode::MalformedState)?;
        let target_identity = self.identity()?.clone();
        let source_space = ClosedTypeSpace {
            module,
            types: &source_types,
            identity: &source_identity,
        };
        let target_space = ClosedTypeSpace {
            module: &self.module,
            types: &self.envs,
            identity: &target_identity,
        };
        Ok(closed_types_match(
            self.loaded.bundle(),
            source_space,
            source_input,
            target_space,
            input,
        ) && closed_types_match(
            self.loaded.bundle(),
            source_space,
            source_output,
            target_space,
            output,
        ))
    }

    pub(super) fn code_ok(&mut self, vm: VmId, value: Value) -> Result<Value, FaultCode> {
        self.make_instance(vm, self.core.result_ok, vec![value])
    }

    pub(super) fn code_error(&mut self, vm: VmId, message: &str) -> Result<Value, FaultCode> {
        let message = self.machines[vm as usize].alloc(Object::Str(message.into()))?;
        let error = self.make_instance(vm, self.core.code_error, vec![message])?;
        self.make_instance(vm, self.core.result_err, vec![error])
    }

    pub(super) fn finish_code_error(&mut self, vm: VmId, op: u32, message: &str) {
        let value = self.code_error(vm, message);
        self.finish_code_result(vm, op, value);
    }

    pub(super) fn finish_code_result(
        &mut self,
        vm: VmId,
        op: u32,
        result: Result<Value, FaultCode>,
    ) {
        match result {
            Ok(value) => self.install_value_reply(vm, value),
            Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
        }
    }
}
