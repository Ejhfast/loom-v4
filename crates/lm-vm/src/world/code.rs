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

impl World {
    /// Execute one portable-code kernel operation.
    pub(super) fn code_exec(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        match op {
            lm_abi::OP_VM_ARTIFACT => self.code_artifact(vm, op, args[0]),
            lm_abi::OP_VM_VERIFY => self.code_verify(vm, op, args[0]),
            lm_abi::OP_VM_INSTALL => self.code_install(vm, op, args[0], args[1]),
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
            index: u32::MAX,
        }));
        match self.machines[vm as usize].alloc(object) {
            Ok(value) => self.install_value_reply(vm, value),
            Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
        }
    }

    fn code_verify(&mut self, vm: VmId, op: u32, value: Value) {
        let bytes = match self.portable_code(vm, value, PortableCodeKind::Artifact) {
            Ok((bytes, _)) => bytes,
            Err(code) => {
                self.fault_caller(vm, op, code, "the verify receiver is not an Artifact");
                return;
            }
        };
        let verified = lm_bytecode::decode(bytes.as_slice())
            .map_err(|error| format!("the artifact did not decode: {error}"))
            .and_then(|module| {
                crate::load(module)
                    .map(|_| ())
                    .map_err(|error| format!("the artifact did not verify: {error}"))
            });
        match verified {
            Ok(()) => {
                let value =
                    self.machines[vm as usize].alloc(Object::NativeCode(Box::new(PortableCode {
                        kind: PortableCodeKind::VerifiedModule,
                        bytes,
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

    fn code_install(&mut self, vm: VmId, op: u32, image: Value, module: Value) {
        let Some(key) = self.image_arg(vm, op, image) else {
            return;
        };
        let bytes = match self.portable_code(vm, module, PortableCodeKind::VerifiedModule) {
            Ok((bytes, _)) => bytes,
            Err(code) => {
                self.fault_caller(vm, op, code, "the install input is not a VerifiedModule");
                return;
            }
        };
        match self.install_artifact(key, bytes) {
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
    ) -> Result<(SharedBytes, u32), FaultCode> {
        let reference = value.as_obj().ok_or(FaultCode::TypeMismatch)?;
        match self.machines[vm as usize].vm.heap.get(reference) {
            Object::NativeCode(code) if code.kind == expected => {
                Ok((code.bytes.clone(), code.index))
            }
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
