//! Portable code values and installed code handles.
//!
//! This module implements the VM kernel operations for verification,
//! installation, typed lookup, activation, and slot replacement.

use super::*;
use lm_heap::{CodeHandleKind, PortableCode, PortableCodeKind};

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
            lm_abi::OP_VM_VERIFY => self.code_verify(vm, op, args[0]),
            lm_abi::OP_VM_INSTALL => self.code_install(vm, op, args[0], args[1], None),
            lm_abi::OP_VM_INSTALL_WITH => {
                self.code_install(vm, op, args[0], args[1], Some(args[2]))
            }
            lm_abi::OP_VM_INSTANCE_ENTRY => self.code_entry(vm, op, args[0]),
            lm_abi::OP_VM_INSTANCE_FUNCTION => self.code_function(vm, op, args[0], args[1]),
            lm_abi::OP_VM_INSTANCE_SLOT | lm_abi::OP_VM_INSTANCE_SLOT_SPEC => {
                self.code_slot(vm, op, args[0], args[1])
            }
            lm_abi::OP_VM_ACTIVATE_DEF => self.code_activate(vm, op, args[0], args[1], args[2]),
            lm_abi::OP_VM_REPLACE_FUNCTION => self.code_replace(vm, op, args[0], args[1], args[2]),
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
        let verified = lm_bytecode::decode(code.bytes.as_slice())
            .map_err(|error| format!("the artifact did not decode: {error}"))
            .and_then(|module| {
                lm_verify::verify_module(&module)
                    .map_err(|error| format!("the artifact did not verify: {error}"))?;
                let identity = lm_bytecode::identity::module_identity(&module)
                    .map_err(|error| format!("the artifact has no identity: {error}"))?;
                if let Some(bytes) = &code.interface {
                    let interface = lm_bytecode::interface::decode_interface(bytes.as_slice())
                        .map_err(|error| format!("the interface did not decode: {error}"))?;
                    if lm_bytecode::interface::encode_interface(&interface) != bytes.as_slice() {
                        return Err("the interface bytes are not canonical".to_string());
                    }
                    lm_bytecode::interface::validate_interface(&module, &identity, &interface)
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

    fn code_install(
        &mut self,
        vm: VmId,
        op: u32,
        image: Value,
        module: Value,
        links: Option<Value>,
    ) {
        let Some(key) = self.image_arg(vm, op, image) else {
            return;
        };
        let code = match self.portable_code(vm, module, PortableCodeKind::VerifiedModule) {
            Ok(code) => code,
            Err(code) => {
                self.fault_caller(vm, op, code, "the install input is not a VerifiedModule");
                return;
            }
        };
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
                let value = self.machines[vm as usize].alloc(Object::NativeCodeHandle {
                    image: key.image,
                    generation: key.generation,
                    instance,
                    kind: CodeHandleKind::Instance,
                    index: instance,
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

    fn resolve_code_imports(
        &self,
        vm: VmId,
        key: VmImageKey,
        links: Value,
        artifact: &[u8],
    ) -> Result<Vec<lm_bytecode::append::ResolvedImport>, String> {
        let module = lm_bytecode::decode(artifact)
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
            let source = lm_bytecode::decode(instance.artifact.as_slice())
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

    fn finish_function_lookup(&mut self, vm: VmId, op: u32, instance: CodeHandle, function: u32) {
        let contract = self.requested_function_contract(vm);
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

    fn code_slot(&mut self, vm: VmId, op: u32, value: Value, index: Value) {
        let handle = match self.code_handle(vm, value, CodeHandleKind::Instance) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the receiver is not an Instance");
                return;
            }
        };
        let Value::Int(index) = index else {
            self.fault_caller(vm, op, FaultCode::TypeMismatch, "the slot index is not Int");
            return;
        };
        let source = usize::try_from(index).ok();
        let mapped = source.and_then(|source| {
            self.live_instance(handle)
                .and_then(|instance| instance.slots.get(source).copied())
        });
        let Some(mapped) = mapped else {
            let value = self.code_error(vm, "the module instance has no slot at this index");
            self.finish_code_result(vm, op, value);
            return;
        };
        let value = if op == lm_abi::OP_VM_INSTANCE_SLOT {
            self.machines[vm as usize].alloc(Object::NativeCodeHandle {
                image: handle.image,
                generation: handle.generation,
                instance: handle.instance,
                kind: CodeHandleKind::Slot,
                index: mapped,
            })
        } else {
            let artifact = self
                .live_instance(handle)
                .map(|instance| instance.artifact.clone())
                .ok_or(FaultCode::InvalidVmState);
            artifact.and_then(|bytes| {
                self.machines[vm as usize].alloc(Object::NativeCode(Box::new(PortableCode {
                    kind: PortableCodeKind::SlotSpec,
                    bytes,
                    interface: None,
                    index: index as u32,
                })))
            })
        };
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
        let handle = match self.code_handle(vm, definition, CodeHandleKind::Function) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the program is not a FunctionDef");
                return;
            }
        };
        if handle.image_key() != key || !self.live_function(handle) {
            self.fault_caller(
                vm,
                op,
                FaultCode::InvalidVmState,
                "the function does not belong to this VM image",
            );
            return;
        }
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
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::InvalidVmState,
                    "the VM image has no run budget left",
                );
                return;
            }
        };
        let locals = match self.transfer_all(vm, target, &values) {
            Ok(values) => values,
            Err(code) => {
                self.rollback_run_target(vm, target);
                self.fault_caller(vm, op, code, "an argument is not sendable");
                return;
            }
        };
        if let Err(code) =
            self.check_frame_args(target, handle.index, lm_value::TypeEnvId::EMPTY, &locals)
        {
            self.rollback_run_target(vm, target);
            self.fault_caller(vm, op, code, "an argument has the wrong type");
            return;
        }
        self.machines[target as usize].load_frame(
            &self.module,
            handle.index,
            locals,
            None,
            lm_value::TypeEnvId::EMPTY,
        );
        match self.machines[vm as usize].alloc(Object::NativeRun { vm: target }) {
            Ok(value) => self.install_value_reply(vm, value),
            Err(code) => {
                self.rollback_run_target(vm, target);
                self.machines[vm as usize].set_fault(code, "", Some(op));
            }
        }
    }

    fn code_replace(&mut self, vm: VmId, op: u32, image: Value, slot: Value, target: Value) {
        let Some(key) = self.image_arg(vm, op, image) else {
            return;
        };
        let slot = match self.code_handle(vm, slot, CodeHandleKind::Slot) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the replacement slot is not a Slot");
                return;
            }
        };
        let target = match self.code_handle(vm, target, CodeHandleKind::Function) {
            Ok(handle) => handle,
            Err(code) => {
                self.fault_caller(vm, op, code, "the replacement target is not a FunctionDef");
                return;
            }
        };
        let valid = slot.image_key() == key
            && target.image_key() == key
            && self.live_slot(slot)
            && self.live_function(target);
        if !valid {
            let value =
                self.code_error(vm, "the replacement handles do not belong to this VM image");
            self.finish_code_result(vm, op, value);
            return;
        }
        match self.replace_function_slot(key, slot.index, target.index) {
            Ok(()) => {
                let value = self.code_ok(vm, Value::Unit);
                self.finish_code_result(vm, op, value);
            }
            Err(_) => {
                let value = self.code_error(
                    vm,
                    "the replacement target does not match the slot contract",
                );
                self.finish_code_result(vm, op, value);
            }
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
    ) -> Result<(ClosedTypeId, ClosedTypeId), FaultCode> {
        let result_class = self.core.result.ok_or(FaultCode::MalformedState)?;
        let function_class = self.core.function_def.ok_or(FaultCode::MalformedState)?;
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
        Ok(actual_input == input && actual_output == output)
    }

    fn code_ok(&mut self, vm: VmId, value: Value) -> Result<Value, FaultCode> {
        self.make_instance(vm, self.core.result_ok, vec![value])
    }

    fn code_error(&mut self, vm: VmId, message: &str) -> Result<Value, FaultCode> {
        let message = self.machines[vm as usize].alloc(Object::Str(message.into()))?;
        let error = self.make_instance(vm, self.core.code_error, vec![message])?;
        self.make_instance(vm, self.core.result_err, vec![error])
    }

    fn finish_code_result(&mut self, vm: VmId, op: u32, result: Result<Value, FaultCode>) {
        match result {
            Ok(value) => self.install_value_reply(vm, value),
            Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
        }
    }
}
