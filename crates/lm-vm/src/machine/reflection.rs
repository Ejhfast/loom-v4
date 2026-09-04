//! Runtime operations for exact reflection descriptors.

use super::*;
use lm_bytecode::artifact::{Artifact, ArtifactId};
use lm_bytecode::closed::{ClosedType, ClosedTypeId, TypeEnv};
use lm_bytecode::interface::{ExportEntry, IfaceItem};
use lm_bytecode::{BcRow, BcType, ConstValue, Constant, Export, ExportKind, ReflectionKind};
use lm_heap::{
    CodeArtifact, CodeDescriptor, CodeDescriptorKind, LinkedCode, LinkedCodeKind, PortableCode,
    PortableCodeKind, PortableCodeStorage,
};
use std::collections::HashSet;

#[derive(Clone, Copy)]
struct ReflectionSource<'a> {
    unit: &'a lm_bytecode::artifact::LinkUnit,
    relocation: &'a lm_link::UnitRelocation,
}

#[derive(Clone, Copy)]
struct DeclarationSource<'a> {
    entry: &'a ExportEntry,
    export: &'a Export,
}

enum ReflectedValue {
    Callable(u32),
    ExactCallable(u32),
    Constant(Constant),
    ClassDescriptor { descriptor: Value, class: u32 },
}

#[derive(Clone)]
struct OpenDescriptor {
    linked: LinkedCode,
    descriptor: CodeDescriptor,
}

impl Machine {
    /// Push one descriptor for an exact relocated module surface.
    pub(super) fn exec_module_code(
        &mut self,
        module: &NamespaceRuntime,
        reflection: u32,
    ) -> Result<(), FaultCode> {
        let source = reflection_source(module, reflection)?;
        let unit = source.unit.id();
        let value = self.alloc(Object::NativeLinkedCode(Box::new(LinkedCode {
            kind: LinkedCodeKind::Module,
            unit: unit.into_bytes(),
            descriptor: None,
        })))?;
        self.push(value)
    }

    /// Replace one module descriptor with its source declarations.
    pub(super) fn exec_reflection_declarations(
        &mut self,
        module: &NamespaceRuntime,
    ) -> Result<(), FaultCode> {
        let module_value = self.pop()?;
        let artifact = self.description_artifact(module, module_value)?;
        let declarations: Vec<u32> = artifact
            .artifact()
            .root()
            .interface()
            .exports
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.source && entry.kind != ExportKind::EnumCase)
            .map(|(index, _)| u32::try_from(index).map_err(|_| BAD_STATE))
            .collect::<Result<_, _>>()?;
        let base = self.vm.operands.len();
        for declaration in declarations {
            let value = self.alloc(Object::NativeCodeDescriptor(Box::new(CodeDescriptor {
                kind: CodeDescriptorKind::Declaration,
                artifact: artifact.clone(),
                declaration,
                member: None,
            })))?;
            if let Err(error) = self.push(value) {
                self.vm.operands.truncate(base);
                return Err(error);
            }
        }
        self.finish_reflection_list(base)
    }

    /// Open one inert descriptor against one linked module.
    pub(super) fn exec_reflection_open(&mut self) -> Result<(), FaultCode> {
        let descriptor = self.pop()?.as_obj().ok_or(BAD_TYPE)?;
        if !matches!(
            self.vm.heap.get(descriptor),
            Object::NativeCodeDescriptor(_)
        ) {
            return Err(BAD_TYPE);
        }
        let module = self.pop()?.as_obj().ok_or(BAD_TYPE)?;
        let linked = match self.vm.heap.get(module) {
            Object::NativeLinkedCode(linked)
                if linked.kind == LinkedCodeKind::Module && linked.descriptor.is_none() =>
            {
                (**linked).clone()
            }
            _ => return Err(BAD_TYPE),
        };
        let value = self.alloc(Object::NativeLinkedCode(Box::new(LinkedCode {
            kind: LinkedCodeKind::Open,
            unit: linked.unit,
            descriptor: Some(descriptor),
        })))?;
        self.push(value)
    }

    /// Replace one declaration descriptor with its effective methods.
    pub(super) fn exec_reflection_members(
        &mut self,
        _module: &NamespaceRuntime,
    ) -> Result<(), FaultCode> {
        let descriptor = self.pop()?.as_obj().ok_or(BAD_TYPE)?;
        let descriptor = self.code_descriptor(descriptor, CodeDescriptorKind::Declaration)?;
        let artifact = descriptor.artifact.artifact();
        let declaration = portable_declaration_source(artifact, descriptor.declaration as usize)?;
        let base = self.vm.operands.len();
        if !matches!(declaration.entry.kind, ExportKind::Class | ExportKind::Enum) {
            return self.finish_reflection_list(base);
        }
        let provider = artifact.root().module();
        let mut class = declaration.export.def;
        let mut selectors = HashSet::new();
        let mut methods = Vec::new();
        loop {
            let definition = provider.classes.get(class as usize).ok_or(BAD_TYPE)?;
            for (selector, _) in &definition.methods {
                if selectors.insert(*selector) {
                    let name = provider
                        .selectors
                        .get(*selector as usize)
                        .cloned()
                        .ok_or(BAD_STATE)?;
                    methods.push(name);
                }
            }
            let Some(parent) = definition.parent() else {
                break;
            };
            class = parent;
        }
        for member in methods {
            let value = self.alloc(Object::NativeCodeDescriptor(Box::new(CodeDescriptor {
                kind: CodeDescriptorKind::Member,
                artifact: descriptor.artifact.clone(),
                declaration: descriptor.declaration,
                member: Some(member),
            })))?;
            if let Err(error) = self.push(value) {
                self.vm.operands.truncate(base);
                return Err(error);
            }
        }
        self.finish_reflection_list(base)
    }

    /// Replace one reflection descriptor with its source name.
    pub(super) fn exec_reflection_name(
        &mut self,
        module: &NamespaceRuntime,
    ) -> Result<(), FaultCode> {
        let value = self.pop()?;
        let reference = value.as_obj().ok_or(BAD_TYPE)?;
        let name = match self.vm.heap.get(reference) {
            Object::NativeLinkedCode(linked)
                if linked.kind == LinkedCodeKind::Module && linked.descriptor.is_none() =>
            {
                let unit = ArtifactId::from_bytes(linked.unit);
                linked_source(module, unit)
                    .ok_or(BAD_STATE)?
                    .unit
                    .module_path()
                    .to_string()
            }
            Object::NativeCode(code) if code.kind == PortableCodeKind::VerifiedModule => {
                let artifact = code.artifact().ok_or(BAD_STATE)?;
                artifact.root().module_path().to_string()
            }
            Object::NativeCodeDescriptor(descriptor) => match descriptor.kind {
                CodeDescriptorKind::Declaration => portable_declaration_source(
                    descriptor.artifact.artifact(),
                    descriptor.declaration as usize,
                )?
                .entry
                .name
                .clone(),
                CodeDescriptorKind::Member => descriptor.member.clone().ok_or(BAD_STATE)?,
            },
            _ => return Err(BAD_TYPE),
        };
        let value = self.alloc(Object::Str(name.into()))?;
        self.push(value)
    }

    /// Replace one declaration descriptor with its kind value.
    pub(super) fn exec_reflection_declaration_kind(
        &mut self,
        module: &NamespaceRuntime,
    ) -> Result<(), FaultCode> {
        let descriptor = self.pop()?.as_obj().ok_or(BAD_TYPE)?;
        let descriptor = self.code_descriptor(descriptor, CodeDescriptorKind::Declaration)?;
        let role = declaration_kind_role(
            portable_declaration_source(
                descriptor.artifact.artifact(),
                descriptor.declaration as usize,
            )?
            .entry
            .kind,
        );
        self.push_reflection_kind(module, role)
    }

    /// Replace one member descriptor with its kind value.
    pub(super) fn exec_reflection_member_kind(
        &mut self,
        module: &NamespaceRuntime,
    ) -> Result<(), FaultCode> {
        let descriptor = self.pop()?.as_obj().ok_or(BAD_TYPE)?;
        self.code_descriptor(descriptor, CodeDescriptorKind::Member)?;
        self.push_reflection_kind(module, lm_bytecode::corepin::ROLE_CODE_KIND_METHOD)
    }

    /// Replace one declaration descriptor with its generic arity.
    pub(super) fn exec_reflection_type_parameter_count(
        &mut self,
        _module: &NamespaceRuntime,
    ) -> Result<(), FaultCode> {
        let descriptor = self.pop()?.as_obj().ok_or(BAD_TYPE)?;
        let descriptor = self.code_descriptor(descriptor, CodeDescriptorKind::Declaration)?;
        let declaration = portable_declaration_source(
            descriptor.artifact.artifact(),
            descriptor.declaration as usize,
        )?;
        let count = match &declaration.entry.item {
            IfaceItem::Class(class) => class.type_params,
            IfaceItem::Func(_) | IfaceItem::Interface(_) | IfaceItem::Const(_) => 0,
        };
        self.push(Value::Int(i64::from(count)))
    }

    /// Replace one declaration descriptor with direct interface names.
    pub(super) fn exec_reflection_interface_names(
        &mut self,
        _module: &NamespaceRuntime,
    ) -> Result<(), FaultCode> {
        let descriptor = self.pop()?.as_obj().ok_or(BAD_TYPE)?;
        let descriptor = self.code_descriptor(descriptor, CodeDescriptorKind::Declaration)?;
        let declaration = portable_declaration_source(
            descriptor.artifact.artifact(),
            descriptor.declaration as usize,
        )?;
        let base = self.vm.operands.len();
        let IfaceItem::Class(class) = &declaration.entry.item else {
            return self.finish_reflection_list(base);
        };
        let interfaces: Vec<String> = class
            .conformances
            .iter()
            .map(|conformance| reflection_name(&conformance.application.interface))
            .collect();
        for interface in interfaces {
            let value = self.alloc(Object::Str(interface.into()))?;
            if let Err(error) = self.push(value) {
                self.vm.operands.truncate(base);
                return Err(error);
            }
        }
        self.finish_reflection_list(base)
    }

    /// Refine one descriptor to a callable value in a scoped environment.
    pub(super) fn exec_reflection_refine(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        slots: Option<&[ImageSlotTarget]>,
        kind: ReflectionKind,
        pattern: u32,
        fail: u32,
    ) -> Result<(), FaultCode> {
        let descriptor = self.pop()?;
        let Some(candidate) = self.reflection_value(module, slots, descriptor, kind)? else {
            return self.finish_reflection_miss(fail);
        };
        let Some(environment) = self.reflection_environment(module, envs, pattern, &candidate)?
        else {
            return self.finish_reflection_miss(fail);
        };
        let value = self.alloc_reflected_value(module, candidate)?;
        self.push(value)?;
        self.vm.frames.last_mut().ok_or(BAD_STATE)?.env = environment;
        Ok(())
    }

    /// Restore the environment prefix outside one refinement arm.
    pub(super) fn exec_reflection_end(
        &mut self,
        envs: &mut TypeEnvs,
        type_base: u32,
        effect_base: u32,
    ) -> Result<(), FaultCode> {
        let current = self.frame_env();
        let environment = envs
            .prefix_env(current, type_base as usize, effect_base as usize)
            .map_err(env_fault)?;
        self.vm.frames.last_mut().ok_or(BAD_STATE)?.env = environment;
        Ok(())
    }

    fn finish_reflection_miss(&mut self, fail: u32) -> Result<(), FaultCode> {
        let frame = self.vm.frames.last_mut().ok_or(BAD_STATE)?;
        frame.block = fail;
        frame.ip = 0;
        Ok(())
    }

    fn reflection_value(
        &self,
        module: &NamespaceRuntime,
        slots: Option<&[ImageSlotTarget]>,
        descriptor: Value,
        kind: ReflectionKind,
    ) -> Result<Option<ReflectedValue>, FaultCode> {
        let open = self.open_descriptor(module, descriptor)?;
        let unit_id = ArtifactId::from_bytes(open.linked.unit);
        let Some(source) = linked_source(module, unit_id) else {
            return Ok(None);
        };
        let artifact = open.descriptor.artifact.artifact();
        if artifact.id() != unit_id {
            return Ok(None);
        }
        let declaration =
            portable_declaration_source(artifact, open.descriptor.declaration as usize)?;
        if !matches!(kind, ReflectionKind::Method | ReflectionKind::Code)
            && open.descriptor.kind != CodeDescriptorKind::Declaration
        {
            return Ok(None);
        }
        match kind {
            ReflectionKind::Code => {
                let function = match open.descriptor.kind {
                    CodeDescriptorKind::Declaration => match declaration.entry.kind {
                        ExportKind::Function => source
                            .relocation
                            .functions()
                            .get(declaration.export.def as usize)
                            .copied(),
                        ExportKind::Class if declaration.export.ctor != lm_bytecode::NO_CTOR => {
                            source
                                .relocation
                                .functions()
                                .get(declaration.export.ctor as usize)
                                .copied()
                        }
                        _ => None,
                    },
                    CodeDescriptorKind::Member => {
                        let member = open.descriptor.member.as_deref().ok_or(BAD_STATE)?;
                        if !matches!(declaration.entry.kind, ExportKind::Class | ExportKind::Enum) {
                            return Ok(None);
                        }
                        exact_method_target(source, declaration.export.def, member)
                    }
                };
                Ok(function
                    .filter(|function| module.funcs.get(*function as usize).is_some())
                    .map(ReflectedValue::ExactCallable))
            }
            ReflectionKind::ClassDescriptor => {
                if declaration.entry.kind != ExportKind::Class {
                    return Ok(None);
                }
                let Some(class) = current_class_target(
                    module,
                    slots,
                    source,
                    class_binding(source, declaration.export.def)?,
                ) else {
                    return Ok(None);
                };
                let descriptor = open.linked.descriptor.ok_or(BAD_STATE)?;
                Ok(module
                    .classes
                    .get(class as usize)
                    .map(|_| ReflectedValue::ClassDescriptor {
                        descriptor: Value::Obj(descriptor),
                        class,
                    }))
            }
            ReflectionKind::Class | ReflectionKind::Function => {
                let accepted = matches!(
                    (kind, declaration.entry.kind),
                    (ReflectionKind::Class, ExportKind::Class)
                        | (ReflectionKind::Function, ExportKind::Function)
                );
                if !accepted {
                    return Ok(None);
                }
                let local = if kind == ReflectionKind::Class {
                    declaration.export.ctor
                } else {
                    declaration.export.def
                };
                if local == lm_bytecode::NO_CTOR {
                    return Ok(None);
                }
                let callable = match kind {
                    ReflectionKind::Class => current_constructor_target(
                        module,
                        slots,
                        source,
                        class_binding(source, declaration.export.def)?,
                    ),
                    ReflectionKind::Function => current_function_target(
                        module,
                        slots,
                        source,
                        &lm_bytecode::qualified_key(
                            source.unit.module_path(),
                            &declaration.entry.name,
                        ),
                    ),
                    _ => unreachable!("the reflection kind was selected above"),
                };
                let Some(callable) = callable else {
                    return Ok(None);
                };
                Ok(module
                    .funcs
                    .get(callable as usize)
                    .map(|_| ReflectedValue::Callable(callable)))
            }
            ReflectionKind::Method => {
                if open.descriptor.kind != CodeDescriptorKind::Member {
                    return Ok(None);
                }
                let member = open.descriptor.member.as_deref().ok_or(BAD_STATE)?;
                if !matches!(declaration.entry.kind, ExportKind::Class | ExportKind::Enum) {
                    return Ok(None);
                }
                let Some(owner) = current_class_target(
                    module,
                    slots,
                    source,
                    class_binding(source, declaration.export.def)?,
                ) else {
                    return Ok(None);
                };
                let Some(selector) = module
                    .selectors
                    .iter()
                    .position(|selector| selector == member)
                    .and_then(|selector| u32::try_from(selector).ok())
                else {
                    return Ok(None);
                };
                let Ok(candidate) = method_of(&module.dispatch, owner, selector) else {
                    return Ok(None);
                };
                let Some(candidate) =
                    current_method_target(module, slots, owner, selector, candidate)
                else {
                    return Ok(None);
                };
                Ok(module
                    .funcs
                    .get(candidate as usize)
                    .map(|_| ReflectedValue::Callable(candidate)))
            }
            ReflectionKind::Constant => {
                if declaration.entry.kind != ExportKind::Constant {
                    return Ok(None);
                }
                let Some(mut constant) = declaration.export.constant.clone() else {
                    return Ok(None);
                };
                let Some(ty) = source.relocation.types().get(constant.ty as usize).copied() else {
                    return Ok(None);
                };
                constant.ty = ty;
                Ok(Some(ReflectedValue::Constant(constant)))
            }
        }
    }

    fn reflection_environment(
        &self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        pattern: u32,
        candidate: &ReflectedValue,
    ) -> Result<Option<TypeEnvId>, FaultCode> {
        let target = module.funcs.get(pattern as usize).ok_or(BAD_STATE)?;
        if target.params.len() != 1 {
            return Ok(None);
        }
        let parent = envs.env(self.frame_env()).cloned().ok_or(BAD_STATE)?;
        if parent.types.len() > target.type_params as usize
            || parent.rows.len() > target.effect_params as usize
        {
            return Err(BAD_STATE);
        }
        let mut types = vec![None; target.type_params as usize];
        let mut rows = vec![None; target.effect_params as usize];
        for (slot, value) in types.iter_mut().zip(parent.types) {
            *slot = Some(value);
        }
        for (slot, value) in rows.iter_mut().zip(parent.rows) {
            *slot = Some(value);
        }
        let matches = match candidate {
            ReflectedValue::Callable(candidate) | ReflectedValue::ExactCallable(candidate) => {
                let candidate = module.funcs.get(*candidate as usize).ok_or(BAD_STATE)?;
                if candidate.type_params != 0
                    || candidate.effect_params != 0
                    || !candidate.captures.is_empty()
                {
                    false
                } else {
                    match_callable(
                        module,
                        envs,
                        target.params[0],
                        candidate,
                        &mut types,
                        &mut rows,
                    )?
                }
            }
            ReflectedValue::Constant(constant) => match_type(
                module,
                envs,
                target.params[0],
                constant.ty,
                &mut types,
                &mut rows,
                0,
            )?,
            ReflectedValue::ClassDescriptor { class, .. } => {
                let Some(definition) = module.classes.get(*class as usize) else {
                    return Ok(None);
                };
                let Some(BcType::Var(variable)) = module.types.get(target.params[0] as usize)
                else {
                    return Ok(None);
                };
                let Some(binding) = types.get_mut(*variable as usize) else {
                    return Ok(None);
                };
                if definition.type_params != 0 || binding.is_some() {
                    return Ok(None);
                }
                *binding = Some(envs.intern(ClosedType::Class(*class)).map_err(env_fault)?);
                true
            }
        };
        if !matches {
            return Ok(None);
        }
        let Some(types) = types.into_iter().collect::<Option<Vec<_>>>() else {
            return Ok(None);
        };
        let Some(rows) = rows.into_iter().collect::<Option<Vec<_>>>() else {
            return Ok(None);
        };
        let environment = envs
            .intern_env(TypeEnv {
                types: types.clone(),
                rows,
            })
            .map_err(env_fault)?;
        let bounds = module.func_bounds.get(pattern as usize).ok_or(BAD_STATE)?;
        for (subject, required) in types.into_iter().zip(bounds) {
            for bound in required {
                if !envs
                    .satisfies_interface(module, subject, bound, environment)
                    .map_err(env_fault)?
                {
                    return Ok(None);
                }
            }
        }
        Ok(Some(environment))
    }

    fn alloc_reflected_value(
        &mut self,
        module: &NamespaceRuntime,
        candidate: ReflectedValue,
    ) -> Result<Value, FaultCode> {
        match candidate {
            ReflectedValue::Callable(candidate) => self.alloc(Object::Closure {
                func: candidate,
                captures: Vec::new().into(),
                env: Witness::EMPTY,
            }),
            ReflectedValue::ExactCallable(candidate) => {
                let artifact = module
                    .code_namespace()
                    .function_artifact(candidate)
                    .map_err(|_| BAD_STATE)?;
                let bytes = lm_bytecode::artifact::encode_with_bundle(&artifact, module.bundle())
                    .map_err(|_| BAD_STATE)?;
                self.alloc(Object::NativeCode(Box::new(PortableCode {
                    kind: PortableCodeKind::Function,
                    storage: PortableCodeStorage::Verified(CodeArtifact::with_bytes(
                        std::sync::Arc::new(artifact),
                        bytes.into(),
                    )),
                    slot: None,
                    origin: None,
                })))
            }
            ReflectedValue::Constant(constant) => {
                self.alloc_reflection_constant(&constant.value, &mut Vec::new(), 0)
            }
            ReflectedValue::ClassDescriptor { descriptor, .. } => Ok(descriptor),
        }
    }

    fn alloc_reflection_constant(
        &mut self,
        constant: &ConstValue,
        roots: &mut Vec<Value>,
        depth: usize,
    ) -> Result<Value, FaultCode> {
        if depth >= 32 {
            return Err(BAD_STATE);
        }
        match constant {
            ConstValue::Unit => Ok(Value::Unit),
            ConstValue::Bool(value) => Ok(Value::Bool(*value)),
            ConstValue::Int(value) => Ok(Value::Int(*value)),
            ConstValue::Float(bits) => Ok(Value::Float(canonical_float_bits(*bits))),
            ConstValue::Char(value) => Ok(Value::Char(*value)),
            ConstValue::String(value) => {
                self.alloc_with_roots(Object::Str(value.clone().into()), roots)
            }
            ConstValue::Bytes(value) => {
                let bytes = SharedBytes::try_from_slice(value).map_err(|_| FaultCode::HeapLimit)?;
                self.alloc_with_roots(Object::Bytes(bytes), roots)
            }
            ConstValue::Tuple(values) => {
                let base = roots.len();
                let mut items = Vec::with_capacity(values.len());
                for value in values {
                    let value = self.alloc_reflection_constant(value, roots, depth + 1)?;
                    items.push(value);
                    roots.push(value);
                }
                let result = self.alloc_with_roots(
                    Object::Tuple {
                        items: items.into(),
                    },
                    roots,
                );
                roots.truncate(base);
                result
            }
        }
    }

    fn description_artifact(
        &self,
        module: &NamespaceRuntime,
        value: Value,
    ) -> Result<CodeArtifact, FaultCode> {
        let reference = value.as_obj().ok_or(BAD_TYPE)?;
        match self.vm.heap.get(reference) {
            Object::NativeCode(code) if code.kind == PortableCodeKind::VerifiedModule => {
                code.artifact_store().ok_or(BAD_STATE)
            }
            Object::NativeLinkedCode(linked)
                if linked.kind == LinkedCodeKind::Module && linked.descriptor.is_none() =>
            {
                let unit = ArtifactId::from_bytes(linked.unit);
                let artifact = module.description_artifact(unit)?;
                Ok(CodeArtifact::new(artifact, 0))
            }
            _ => Err(BAD_TYPE),
        }
    }

    fn code_descriptor(
        &self,
        reference: ObjRef,
        kind: CodeDescriptorKind,
    ) -> Result<CodeDescriptor, FaultCode> {
        match self.vm.heap.get(reference) {
            Object::NativeCodeDescriptor(descriptor) if descriptor.kind == kind => {
                Ok((**descriptor).clone())
            }
            _ => Err(BAD_TYPE),
        }
    }

    fn open_descriptor(
        &self,
        module: &NamespaceRuntime,
        value: Value,
    ) -> Result<OpenDescriptor, FaultCode> {
        let reference = value.as_obj().ok_or(BAD_TYPE)?;
        let linked = match self.vm.heap.get(reference) {
            Object::NativeLinkedCode(linked)
                if linked.kind == LinkedCodeKind::Open && linked.descriptor.is_some() =>
            {
                (**linked).clone()
            }
            _ => return Err(BAD_TYPE),
        };
        let descriptor = linked.descriptor.ok_or(BAD_STATE)?;
        let descriptor = match self.vm.heap.get(descriptor) {
            Object::NativeCodeDescriptor(descriptor) => (**descriptor).clone(),
            _ => return Err(BAD_TYPE),
        };
        if descriptor.artifact.artifact().id().into_bytes() != linked.unit {
            return Err(BAD_STATE);
        }
        if linked_source(module, ArtifactId::from_bytes(linked.unit)).is_none() {
            return Err(BAD_STATE);
        }
        Ok(OpenDescriptor { linked, descriptor })
    }

    fn finish_reflection_list(&mut self, base: usize) -> Result<(), FaultCode> {
        let items = self.vm.operands.split_off(base);
        let value = self.alloc(Object::List {
            items: items.into(),
            epoch: StructuralEpoch::default(),
        })?;
        self.vm.heap.set_frozen(value.as_obj().ok_or(BAD_STATE)?);
        self.push(value)
    }

    fn push_reflection_kind(
        &mut self,
        module: &NamespaceRuntime,
        arm_role: usize,
    ) -> Result<(), FaultCode> {
        let arm = module.core_roles[arm_role];
        if arm == lm_bytecode::NO_ROLE {
            return Err(BAD_STATE);
        }
        let value = self.alloc(Object::Instance {
            class: arm,
            fields: Vec::new().into(),
            env: Witness::EMPTY,
        })?;
        self.vm.heap.set_frozen(value.as_obj().ok_or(BAD_STATE)?);
        self.push(value)
    }
}

fn reflection_source(
    module: &NamespaceRuntime,
    reflection: u32,
) -> Result<ReflectionSource<'_>, FaultCode> {
    let surface = module
        .reflections
        .get(reflection as usize)
        .ok_or(BAD_STATE)?;
    let unit_id = module
        .code_namespace()
        .reflection_unit(reflection)
        .ok_or(BAD_STATE)?;
    let unit = module.code_namespace().unit(unit_id).ok_or(BAD_STATE)?;
    if unit.module_path() != surface.name || unit.identity().semantic_hash != surface.semantic_hash
    {
        return Err(BAD_STATE);
    }
    let relocation = module
        .code_namespace()
        .relocation(unit_id)
        .ok_or(BAD_STATE)?;
    Ok(ReflectionSource { unit, relocation })
}

fn linked_source(module: &NamespaceRuntime, unit: ArtifactId) -> Option<ReflectionSource<'_>> {
    Some(ReflectionSource {
        unit: module.code_namespace().unit(unit)?,
        relocation: module.code_namespace().relocation(unit)?,
    })
}

fn portable_declaration_source(
    artifact: &Artifact,
    declaration: usize,
) -> Result<DeclarationSource<'_>, FaultCode> {
    let unit = artifact.root();
    let entry = unit
        .interface()
        .exports
        .get(declaration)
        .filter(|entry| entry.source && entry.kind != ExportKind::EnumCase)
        .ok_or(BAD_TYPE)?;
    let export = unit
        .module()
        .exports
        .get(declaration)
        .filter(|export| export.name == entry.name && export.kind == entry.kind)
        .ok_or(BAD_STATE)?;
    Ok(DeclarationSource { entry, export })
}

fn current_slot_target(
    module: &NamespaceRuntime,
    slots: Option<&[ImageSlotTarget]>,
    slot: u32,
) -> Option<ImageSlotTarget> {
    if let Some(slots) = slots {
        return slots.get(slot as usize).copied();
    }
    match module.code_namespace().slot_initials().get(slot as usize)? {
        Some(lm_bytecode::SlotTarget::Function(function)) => {
            Some(ImageSlotTarget::Function(*function))
        }
        Some(lm_bytecode::SlotTarget::Class { class, constructor }) => {
            Some(ImageSlotTarget::Class {
                class: *class,
                constructor: *constructor,
            })
        }
        None => Some(ImageSlotTarget::Empty),
    }
}

fn source_slot(source: ReflectionSource<'_>, binding: &str) -> Option<u32> {
    let local = source
        .unit
        .module()
        .slots
        .iter()
        .position(|slot| slot.binding == binding)?;
    source.relocation.slots().get(local).copied()
}

fn class_binding(source: ReflectionSource<'_>, class: u32) -> Result<&str, FaultCode> {
    source
        .unit
        .module()
        .classes
        .get(class as usize)
        .map(|definition| definition.key.as_str())
        .ok_or(BAD_STATE)
}

fn exact_method_target(source: ReflectionSource<'_>, mut class: u32, member: &str) -> Option<u32> {
    let provider = source.unit.module();
    let selector = provider
        .selectors
        .iter()
        .position(|selector| selector == member)
        .and_then(|selector| u32::try_from(selector).ok())?;
    loop {
        let definition = provider.classes.get(class as usize)?;
        if let Some((_, function)) = definition
            .methods
            .iter()
            .find(|(candidate, _)| *candidate == selector)
        {
            return source
                .relocation
                .functions()
                .get(*function as usize)
                .copied();
        }
        class = definition.parent()?;
    }
}

fn current_function_target(
    module: &NamespaceRuntime,
    slots: Option<&[ImageSlotTarget]>,
    source: ReflectionSource<'_>,
    binding: &str,
) -> Option<u32> {
    let slot = source_slot(source, binding)?;
    match current_slot_target(module, slots, slot)? {
        ImageSlotTarget::Function(function) => Some(function),
        _ => None,
    }
}

fn current_class_target(
    module: &NamespaceRuntime,
    slots: Option<&[ImageSlotTarget]>,
    source: ReflectionSource<'_>,
    binding: &str,
) -> Option<u32> {
    let slot = source_slot(source, binding)?;
    match current_slot_target(module, slots, slot)? {
        ImageSlotTarget::Class { class, .. } => Some(class),
        _ => None,
    }
}

fn current_constructor_target(
    module: &NamespaceRuntime,
    slots: Option<&[ImageSlotTarget]>,
    source: ReflectionSource<'_>,
    binding: &str,
) -> Option<u32> {
    let slot = source_slot(source, binding)?;
    match current_slot_target(module, slots, slot)? {
        ImageSlotTarget::Class { constructor, .. } => Some(constructor),
        _ => None,
    }
}

fn current_method_target(
    module: &NamespaceRuntime,
    slots: Option<&[ImageSlotTarget]>,
    owner: u32,
    selector: u32,
    initial: u32,
) -> Option<u32> {
    let selector_name = module.selectors.get(selector as usize)?;
    let mut class = owner;
    let binding = loop {
        let definition = module.classes.get(class as usize)?;
        if definition
            .methods
            .iter()
            .any(|(candidate, _)| *candidate == selector)
        {
            break format!("{}.{}", definition.key, selector_name);
        }
        class = definition.parent()?;
    };
    let Some(slot) = module.slots.iter().position(|slot| {
        matches!(slot.contract, lm_bytecode::SlotContract::Method(_)) && slot.binding == binding
    }) else {
        return Some(initial);
    };
    let slot = u32::try_from(slot).ok()?;
    match current_slot_target(module, slots, slot)? {
        ImageSlotTarget::Function(function) => Some(function),
        _ => None,
    }
}

fn declaration_kind_role(kind: ExportKind) -> usize {
    match kind {
        ExportKind::Function => lm_bytecode::corepin::ROLE_CODE_KIND_FUNCTION,
        ExportKind::Class => lm_bytecode::corepin::ROLE_CODE_KIND_CLASS,
        ExportKind::Enum => lm_bytecode::corepin::ROLE_CODE_KIND_ENUM,
        ExportKind::EnumCase => lm_bytecode::corepin::ROLE_CODE_KIND_ENUM,
        ExportKind::Interface => lm_bytecode::corepin::ROLE_CODE_KIND_INTERFACE,
        ExportKind::Constant => lm_bytecode::corepin::ROLE_CODE_KIND_CONSTANT,
    }
}

fn reflection_name(name: &lm_bytecode::interface::QualName) -> String {
    if name.is_core() {
        format!("core.{}", name.name)
    } else {
        name.text()
    }
}

fn match_callable(
    module: &NamespaceRuntime,
    envs: &mut TypeEnvs,
    pattern: u32,
    candidate: &lm_bytecode::Func,
    types: &mut [Option<ClosedTypeId>],
    rows: &mut [Option<Vec<u32>>],
) -> Result<bool, FaultCode> {
    let Some(BcType::Fn(params, muts, result, row)) = module.types.get(pattern as usize).cloned()
    else {
        return Ok(false);
    };
    if params.len() != candidate.params.len()
        || muts.len() != candidate.param_muts.len()
        || candidate
            .param_muts
            .iter()
            .zip(&muts)
            .any(|(actual, accepted)| *actual && !*accepted)
    {
        return Ok(false);
    }
    for (pattern, candidate) in params.into_iter().zip(&candidate.params) {
        if !match_type(module, envs, pattern, *candidate, types, rows, 0)? {
            return Ok(false);
        }
    }
    if !match_type(module, envs, result, candidate.ret, types, rows, 0)? {
        return Ok(false);
    }
    match_rows(module, &row, &candidate.row, rows)
}

fn match_type(
    module: &NamespaceRuntime,
    envs: &mut TypeEnvs,
    pattern: u32,
    candidate: u32,
    types: &mut [Option<ClosedTypeId>],
    rows: &mut [Option<Vec<u32>>],
    depth: usize,
) -> Result<bool, FaultCode> {
    if depth >= lm_bytecode::closed::MAX_CLOSED_DEPTH as usize {
        return Ok(false);
    }
    let pattern_node = module
        .types
        .get(pattern as usize)
        .cloned()
        .ok_or(BAD_STATE)?;
    let candidate_node = module
        .types
        .get(candidate as usize)
        .cloned()
        .ok_or(BAD_STATE)?;
    if let BcType::Var(variable) = pattern_node {
        let Some(binding) = types.get_mut(variable as usize) else {
            return Ok(false);
        };
        if matches!(
            candidate_node,
            BcType::Never | BcType::Var(_) | BcType::Projection { .. }
        ) {
            return Ok(false);
        }
        let candidate = envs
            .close(module, candidate, TypeEnvId::EMPTY)
            .map_err(env_fault)?;
        return Ok(match binding {
            Some(current) => *current == candidate,
            slot @ None => {
                *slot = Some(candidate);
                true
            }
        });
    }
    let pairs: Vec<(u32, u32)> = match (pattern_node, candidate_node) {
        (BcType::Unit, BcType::Unit)
        | (BcType::Never, BcType::Never)
        | (BcType::Bool, BcType::Bool)
        | (BcType::Int, BcType::Int)
        | (BcType::Float, BcType::Float)
        | (BcType::Str, BcType::Str)
        | (BcType::Fault, BcType::Fault)
        | (BcType::Request, BcType::Request)
        | (BcType::PolicyTable, BcType::PolicyTable)
        | (BcType::Vm, BcType::Vm)
        | (BcType::Digest, BcType::Digest)
        | (BcType::VmSnapshot, BcType::VmSnapshot)
        | (BcType::Bytes, BcType::Bytes)
        | (BcType::FileHandle, BcType::FileHandle)
        | (BcType::ResourceHandle, BcType::ResourceHandle)
        | (BcType::HostResource, BcType::HostResource) => Vec::new(),
        (BcType::Class(left), BcType::Class(right)) if left == right => Vec::new(),
        (BcType::Inst(left, xs), BcType::Inst(right, ys))
            if left == right && xs.len() == ys.len() =>
        {
            xs.into_iter().zip(ys).collect()
        }
        (BcType::List(left), BcType::List(right))
        | (BcType::Run(left), BcType::Run(right))
        | (BcType::Wait(left), BcType::Wait(right))
        | (BcType::RunSnapshot(left), BcType::RunSnapshot(right)) => vec![(left, right)],
        (BcType::Map(a, b), BcType::Map(c, d))
        | (BcType::PendingCall(a, b), BcType::PendingCall(c, d))
        | (BcType::Handle(a, b), BcType::Handle(c, d)) => vec![(a, c), (b, d)],
        (BcType::Tuple(xs), BcType::Tuple(ys)) if xs.len() == ys.len() => {
            xs.into_iter().zip(ys).collect()
        }
        (BcType::Op(left, a), BcType::Op(right, b)) if left == right => vec![(a, b)],
        (BcType::Fn(xs, xm, xr, xrow), BcType::Fn(ys, ym, yr, yrow))
        | (BcType::Callback(xs, xm, xr, xrow), BcType::Callback(ys, ym, yr, yrow))
            if xs.len() == ys.len()
                && xm.len() == ym.len()
                && ym
                    .iter()
                    .zip(&xm)
                    .all(|(actual, accepted)| !*actual || *accepted) =>
        {
            if !match_rows(module, &xrow, &yrow, rows)? {
                return Ok(false);
            }
            xs.into_iter().zip(ys).chain([(xr, yr)]).collect()
        }
        _ => return Ok(false),
    };
    for (left, right) in pairs {
        if !match_type(module, envs, left, right, types, rows, depth + 1)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn match_rows(
    module: &NamespaceRuntime,
    pattern: &[BcRow],
    candidate: &[BcRow],
    bindings: &mut [Option<Vec<u32>>],
) -> Result<bool, FaultCode> {
    let mut allowed = Vec::new();
    let mut fresh = None;
    for element in pattern {
        match element {
            BcRow::Op(operation) => allowed.push(*operation),
            BcRow::Group(group) => {
                module.bundle.extend_group_operations(*group, &mut allowed);
            }
            BcRow::Var(variable) => {
                let Some(binding) = bindings.get(*variable as usize) else {
                    return Ok(false);
                };
                match binding {
                    Some(row) => allowed.extend_from_slice(row),
                    None if fresh.is_none() || fresh == Some(*variable) => fresh = Some(*variable),
                    None => return Ok(false),
                }
            }
        }
    }
    allowed.sort_unstable();
    allowed.dedup();
    let mut actual = Vec::new();
    for element in candidate {
        match element {
            BcRow::Op(operation) => actual.push(*operation),
            BcRow::Group(group) => {
                module.bundle.extend_group_operations(*group, &mut actual);
            }
            BcRow::Var(_) => return Ok(false),
        }
    }
    actual.sort_unstable();
    actual.dedup();
    let remainder: Vec<u32> = actual
        .into_iter()
        .filter(|operation| allowed.binary_search(operation).is_err())
        .collect();
    if let Some(variable) = fresh {
        bindings[variable as usize] = Some(remainder);
        Ok(true)
    } else {
        Ok(remainder.is_empty())
    }
}
