//! Runtime operations for exact reflection descriptors.

use super::*;
use lm_bytecode::closed::{ClosedType, ClosedTypeId, TypeEnv};
use lm_bytecode::{
    BcRow, BcType, ConstValue, Constant, ExportKind, ReflectionKind, NO_REFLECTION_DEF,
};
use std::collections::HashSet;

enum ReflectedValue {
    Callable(u32),
    Constant(Constant),
    ClassDescriptor { descriptor: Value, class: u32 },
}

impl Machine {
    /// Push one descriptor for an exact relocated module surface.
    pub(super) fn exec_module_code(
        &mut self,
        module: &NamespaceRuntime,
        reflection: u32,
    ) -> Result<(), FaultCode> {
        module
            .reflections
            .get(reflection as usize)
            .ok_or(BAD_STATE)?;
        let value = self.alloc_reflection_descriptor(
            module,
            lm_bytecode::corepin::ROLE_MODULE_CODE,
            vec![Value::Int(i64::from(reflection))],
        )?;
        self.push(value)
    }

    /// Replace one module descriptor with its source declarations.
    pub(super) fn exec_reflection_declarations(
        &mut self,
        module: &NamespaceRuntime,
    ) -> Result<(), FaultCode> {
        let descriptor = self.pop()?;
        let fields = self.reflection_fields(
            module,
            descriptor,
            lm_bytecode::corepin::ROLE_MODULE_CODE,
            1,
        )?;
        let reflection = u32::try_from(fields[0]).map_err(|_| BAD_TYPE)?;
        let count = module
            .reflections
            .get(reflection as usize)
            .ok_or(BAD_TYPE)?
            .declarations
            .len();
        let base = self.vm.operands.len();
        for declaration in 0..count {
            let declaration = u32::try_from(declaration).map_err(|_| BAD_STATE)?;
            let value = self.alloc_reflection_descriptor(
                module,
                lm_bytecode::corepin::ROLE_DECLARATION_CODE,
                vec![
                    Value::Int(i64::from(reflection)),
                    Value::Int(i64::from(declaration)),
                ],
            )?;
            if let Err(error) = self.push(value) {
                self.vm.operands.truncate(base);
                return Err(error);
            }
        }
        self.finish_reflection_list(base)
    }

    /// Replace one declaration descriptor with its effective methods.
    pub(super) fn exec_reflection_members(
        &mut self,
        module: &NamespaceRuntime,
    ) -> Result<(), FaultCode> {
        let descriptor = self.pop()?;
        let fields = self.reflection_fields(
            module,
            descriptor,
            lm_bytecode::corepin::ROLE_DECLARATION_CODE,
            2,
        )?;
        let reflection = u32::try_from(fields[0]).map_err(|_| BAD_TYPE)?;
        let declaration_index = usize::try_from(fields[1]).map_err(|_| BAD_TYPE)?;
        let declaration = module
            .reflections
            .get(reflection as usize)
            .and_then(|surface| surface.declarations.get(declaration_index))
            .ok_or(BAD_TYPE)?;
        let base = self.vm.operands.len();
        if !matches!(declaration.kind, ExportKind::Class | ExportKind::Enum) {
            return self.finish_reflection_list(base);
        }
        let mut class = declaration.def;
        let mut selectors = HashSet::new();
        let mut methods = Vec::new();
        loop {
            let definition = module.classes.get(class as usize).ok_or(BAD_TYPE)?;
            for (selector, function) in &definition.methods {
                if selectors.insert(*selector) {
                    methods.push((*selector, *function));
                }
            }
            let Some(parent) = definition.parent() else {
                break;
            };
            class = parent;
        }
        for (selector, function) in methods {
            let value = self.alloc_reflection_descriptor(
                module,
                lm_bytecode::corepin::ROLE_MEMBER_CODE,
                vec![
                    Value::Int(i64::from(declaration.def)),
                    Value::Int(i64::from(selector)),
                    Value::Int(i64::from(function)),
                ],
            )?;
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
        let descriptor = self.pop()?;
        let reference = descriptor.as_obj().ok_or(BAD_TYPE)?;
        let (class, fields) = match self.vm.heap.get(reference) {
            Object::Instance { class, fields, .. } => (*class, fields.to_vec()),
            _ => return Err(BAD_TYPE),
        };
        let name = if Some(class) == module.core.module_code {
            let [Value::Int(reflection)] = fields.as_slice() else {
                return Err(BAD_TYPE);
            };
            let reflection = usize::try_from(*reflection).map_err(|_| BAD_TYPE)?;
            module
                .reflections
                .get(reflection)
                .map(|surface| surface.name.clone())
                .ok_or(BAD_TYPE)?
        } else if Some(class) == module.core.declaration_code {
            let [Value::Int(reflection), Value::Int(declaration)] = fields.as_slice() else {
                return Err(BAD_TYPE);
            };
            let reflection = usize::try_from(*reflection).map_err(|_| BAD_TYPE)?;
            let declaration = usize::try_from(*declaration).map_err(|_| BAD_TYPE)?;
            module
                .reflections
                .get(reflection)
                .and_then(|surface| surface.declarations.get(declaration))
                .map(|declaration| declaration.name.clone())
                .ok_or(BAD_TYPE)?
        } else if Some(class) == module.core.member_code {
            let [Value::Int(_), Value::Int(selector), Value::Int(_)] = fields.as_slice() else {
                return Err(BAD_TYPE);
            };
            let selector = usize::try_from(*selector).map_err(|_| BAD_TYPE)?;
            module.selectors.get(selector).cloned().ok_or(BAD_TYPE)?
        } else {
            return Err(BAD_TYPE);
        };
        let value = self.alloc(Object::Str(name.into()))?;
        self.push(value)
    }

    /// Replace one declaration descriptor with its stable kind name.
    pub(super) fn exec_reflection_declaration_kind(
        &mut self,
        module: &NamespaceRuntime,
    ) -> Result<(), FaultCode> {
        let descriptor = self.pop()?;
        let fields = self.reflection_fields(
            module,
            descriptor,
            lm_bytecode::corepin::ROLE_DECLARATION_CODE,
            2,
        )?;
        let reflection = usize::try_from(fields[0]).map_err(|_| BAD_TYPE)?;
        let declaration = usize::try_from(fields[1]).map_err(|_| BAD_TYPE)?;
        let kind = module
            .reflections
            .get(reflection)
            .and_then(|surface| surface.declarations.get(declaration))
            .map(|declaration| declaration_kind_name(declaration.kind))
            .ok_or(BAD_TYPE)?;
        let value = self.alloc(Object::Str(kind.into()))?;
        self.push(value)
    }

    /// Replace one member descriptor with its stable kind name.
    pub(super) fn exec_reflection_member_kind(
        &mut self,
        module: &NamespaceRuntime,
    ) -> Result<(), FaultCode> {
        let descriptor = self.pop()?;
        self.reflection_fields(
            module,
            descriptor,
            lm_bytecode::corepin::ROLE_MEMBER_CODE,
            3,
        )?;
        let value = self.alloc(Object::Str("method".into()))?;
        self.push(value)
    }

    /// Refine one descriptor to a callable value in a scoped environment.
    pub(super) fn exec_reflection_refine(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        kind: ReflectionKind,
        pattern: u32,
        fail: u32,
    ) -> Result<(), FaultCode> {
        let descriptor = self.pop()?;
        let Some(candidate) = self.reflection_value(module, descriptor, kind)? else {
            return self.finish_reflection_miss(fail);
        };
        let Some(environment) = self.reflection_environment(module, envs, pattern, &candidate)?
        else {
            return self.finish_reflection_miss(fail);
        };
        let value = self.alloc_reflected_value(candidate)?;
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
        descriptor: Value,
        kind: ReflectionKind,
    ) -> Result<Option<ReflectedValue>, FaultCode> {
        match kind {
            ReflectionKind::ClassDescriptor => {
                let fields = self.reflection_fields(
                    module,
                    descriptor,
                    lm_bytecode::corepin::ROLE_DECLARATION_CODE,
                    2,
                )?;
                let reflection = usize::try_from(fields[0]).map_err(|_| BAD_TYPE)?;
                let declaration = usize::try_from(fields[1]).map_err(|_| BAD_TYPE)?;
                let Some(declaration) = module
                    .reflections
                    .get(reflection)
                    .and_then(|surface| surface.declarations.get(declaration))
                else {
                    return Ok(None);
                };
                if declaration.kind != ExportKind::Class {
                    return Ok(None);
                }
                Ok(module.classes.get(declaration.def as usize).map(|_| {
                    ReflectedValue::ClassDescriptor {
                        descriptor,
                        class: declaration.def,
                    }
                }))
            }
            ReflectionKind::Class | ReflectionKind::Function => {
                let fields = self.reflection_fields(
                    module,
                    descriptor,
                    lm_bytecode::corepin::ROLE_DECLARATION_CODE,
                    2,
                )?;
                let reflection = usize::try_from(fields[0]).map_err(|_| BAD_TYPE)?;
                let declaration = usize::try_from(fields[1]).map_err(|_| BAD_TYPE)?;
                let Some(declaration) = module
                    .reflections
                    .get(reflection)
                    .and_then(|surface| surface.declarations.get(declaration))
                else {
                    return Ok(None);
                };
                let accepted = matches!(
                    (kind, declaration.kind),
                    (ReflectionKind::Class, ExportKind::Class)
                        | (ReflectionKind::Function, ExportKind::Function)
                );
                if !accepted || declaration.callable == NO_REFLECTION_DEF {
                    return Ok(None);
                }
                Ok(module
                    .funcs
                    .get(declaration.callable as usize)
                    .map(|_| ReflectedValue::Callable(declaration.callable)))
            }
            ReflectionKind::Method => {
                let fields = self.reflection_fields(
                    module,
                    descriptor,
                    lm_bytecode::corepin::ROLE_MEMBER_CODE,
                    3,
                )?;
                let owner = u32::try_from(fields[0]).map_err(|_| BAD_TYPE)?;
                let selector = u32::try_from(fields[1]).map_err(|_| BAD_TYPE)?;
                let candidate = u32::try_from(fields[2]).map_err(|_| BAD_TYPE)?;
                if method_of(&module.dispatch, owner, selector).ok() != Some(candidate) {
                    return Ok(None);
                }
                Ok(module
                    .funcs
                    .get(candidate as usize)
                    .map(|_| ReflectedValue::Callable(candidate)))
            }
            ReflectionKind::Constant => {
                let fields = self.reflection_fields(
                    module,
                    descriptor,
                    lm_bytecode::corepin::ROLE_DECLARATION_CODE,
                    2,
                )?;
                let reflection = usize::try_from(fields[0]).map_err(|_| BAD_TYPE)?;
                let declaration = usize::try_from(fields[1]).map_err(|_| BAD_TYPE)?;
                Ok(module
                    .reflections
                    .get(reflection)
                    .and_then(|surface| surface.declarations.get(declaration))
                    .filter(|declaration| declaration.kind == ExportKind::Constant)
                    .and_then(|declaration| declaration.constant.clone())
                    .map(ReflectedValue::Constant))
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
            ReflectedValue::Callable(candidate) => {
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

    fn alloc_reflected_value(&mut self, candidate: ReflectedValue) -> Result<Value, FaultCode> {
        match candidate {
            ReflectedValue::Callable(candidate) => self.alloc(Object::Closure {
                func: candidate,
                captures: Vec::new().into(),
                env: Witness::EMPTY,
            }),
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

    fn reflection_fields(
        &self,
        module: &NamespaceRuntime,
        value: Value,
        role: usize,
        count: usize,
    ) -> Result<Vec<i64>, FaultCode> {
        let class = *module.core_roles.get(role).ok_or(BAD_STATE)?;
        if class == lm_bytecode::NO_ROLE {
            return Err(BAD_STATE);
        }
        let reference = value.as_obj().ok_or(BAD_TYPE)?;
        let Object::Instance {
            class: found,
            fields,
            ..
        } = self.vm.heap.get(reference)
        else {
            return Err(BAD_TYPE);
        };
        if *found != class || fields.len() != count {
            return Err(BAD_TYPE);
        }
        fields
            .iter()
            .map(|value| match value {
                Value::Int(value) => Ok(*value),
                _ => Err(BAD_TYPE),
            })
            .collect()
    }

    fn alloc_reflection_descriptor(
        &mut self,
        module: &NamespaceRuntime,
        role: usize,
        fields: Vec<Value>,
    ) -> Result<Value, FaultCode> {
        let class = *module.core_roles.get(role).ok_or(BAD_STATE)?;
        if class == lm_bytecode::NO_ROLE {
            return Err(BAD_STATE);
        }
        let value = self.alloc(Object::Instance {
            class,
            fields: fields.into(),
            env: Witness::EMPTY,
        })?;
        self.vm.heap.set_frozen(value.as_obj().ok_or(BAD_STATE)?);
        Ok(value)
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
}

fn declaration_kind_name(kind: ExportKind) -> &'static str {
    match kind {
        ExportKind::Function => "function",
        ExportKind::Class => "class",
        ExportKind::Enum => "enum",
        ExportKind::EnumCase => "enum_case",
        ExportKind::Interface => "interface",
        ExportKind::Constant => "constant",
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
