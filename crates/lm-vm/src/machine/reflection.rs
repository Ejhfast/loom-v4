//! Runtime operations for exact reflection descriptors.

use super::*;
use lm_bytecode::ExportKind;
use std::collections::HashSet;

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
