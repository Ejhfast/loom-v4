//! Syntax, code, and source-location operations.

use super::*;

impl Machine {
    /// Read and validate one syntax tree value.
    pub(super) fn syntax_tree_parts(
        &self,
        reference: ObjRef,
        tree_class: u32,
    ) -> Result<(Value, Value, SharedText, SharedBytes), FaultCode> {
        let Object::Instance { class, fields, .. } = self.vm.heap.get(reference) else {
            return Err(BAD_TYPE);
        };
        if *class != tree_class {
            return Err(BAD_TYPE);
        }
        let [source, records] = fields.as_slice() else {
            return Err(BAD_TYPE);
        };
        let source_ref = source.as_obj().ok_or(BAD_TYPE)?;
        let records_ref = records.as_obj().ok_or(BAD_TYPE)?;
        let Object::Str(text) = self.vm.heap.get(source_ref) else {
            return Err(BAD_TYPE);
        };
        let Object::Bytes(bytes) = self.vm.heap.get(records_ref) else {
            return Err(BAD_TYPE);
        };
        Ok((*source, *records, text.clone(), bytes.clone()))
    }

    /// Read and validate one syntax element value.
    pub(super) fn syntax_element_parts(
        &self,
        reference: ObjRef,
        node: u32,
        token: u32,
        trivia: u32,
    ) -> Result<(Value, Value, SharedText, SharedBytes, u32), FaultCode> {
        let Object::Instance { class, fields, .. } = self.vm.heap.get(reference) else {
            return Err(BAD_TYPE);
        };
        if *class != node && *class != token && *class != trivia {
            return Err(BAD_TYPE);
        }
        let [source, records, Value::Int(index)] = fields.as_slice() else {
            return Err(BAD_TYPE);
        };
        let index = u32::try_from(*index).map_err(|_| BAD_TYPE)?;
        let source_ref = source.as_obj().ok_or(BAD_TYPE)?;
        let records_ref = records.as_obj().ok_or(BAD_TYPE)?;
        let Object::Str(text) = self.vm.heap.get(source_ref) else {
            return Err(BAD_TYPE);
        };
        let Object::Bytes(bytes) = self.vm.heap.get(records_ref) else {
            return Err(BAD_TYPE);
        };
        Ok((*source, *records, text.clone(), bytes.clone(), index))
    }

    /// Read one portable definition source attachment.
    #[inline(never)]
    pub(super) fn exec_code_source(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        ty: u32,
    ) -> Result<(), FaultCode> {
        let reference = self.pop_obj()?;
        let code = match self.vm.heap.get(reference) {
            Object::NativeCode(code)
                if matches!(
                    code.kind,
                    lm_heap::PortableCodeKind::Function | lm_heap::PortableCodeKind::Class
                ) =>
            {
                (**code).clone()
            }
            _ => return Err(BAD_TYPE),
        };
        let artifact = code.artifact().ok_or(BAD_STATE)?;
        let decoded = artifact.root().module();
        let selected = portable_definition_index(&code, decoded)?;
        let debug = lm_bytecode::debug::decode(&decoded.debug).map_err(|_| BAD_STATE)?;
        lm_bytecode::debug::validate(&debug, decoded).map_err(|_| BAD_STATE)?;
        let kind = match code.kind {
            lm_heap::PortableCodeKind::Function => lm_bytecode::debug::DefinitionKind::Function,
            lm_heap::PortableCodeKind::Class => lm_bytecode::debug::DefinitionKind::Class,
            _ => return Err(BAD_TYPE),
        };
        let definition = match code.origin {
            Some(origin) => debug.definitions.iter().find(|definition| {
                definition.origin == origin
                    && definition.kind == kind
                    && definition.target == selected
            }),
            None => debug
                .definitions
                .iter()
                .rev()
                .find(|definition| definition.kind == kind && definition.target == selected),
        };
        let Some(definition) = definition else {
            let family = self.close_option_family(module, envs, ty)?;
            self.push(Value::EmptyCase { ty: family, arm: 1 })?;
            return Ok(());
        };
        let source = debug
            .sources
            .get(definition.source as usize)
            .ok_or(BAD_STATE)?;
        let identity = artifact.root().identity();
        let syntax_class = module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_NODE];
        let source_class = module.core_roles[lm_bytecode::corepin::ROLE_DEFINITION_SOURCE];
        if syntax_class == lm_bytecode::NO_ROLE || source_class == lm_bytecode::NO_ROLE {
            return Err(BAD_STATE);
        }

        let root = self.vm.operands.len();
        let path =
            SharedText::try_from_string(source.path.clone()).map_err(|_| FaultCode::HeapLimit)?;
        let path = self.alloc(Object::Str(path))?;
        self.push(path)?;
        let text =
            SharedText::try_from_string(source.text.clone()).map_err(|_| FaultCode::HeapLimit)?;
        let text = self.alloc(Object::Str(text))?;
        self.push(text)?;
        let records =
            SharedBytes::try_from_slice(&source.syntax).map_err(|_| FaultCode::HeapLimit)?;
        let records = self.alloc(Object::Bytes(records))?;
        self.push(records)?;
        let syntax = self.alloc_syntax_view(syntax_class, text, records, definition.syntax)?;
        self.push(syntax)?;
        let spec = self.alloc_definition_spec(module, &code, decoded, identity)?;
        self.push(spec)?;
        let value = self.alloc(Object::Instance {
            class: source_class,
            fields: vec![path, syntax, spec].into(),
            env: Witness::EMPTY,
        })?;
        let reference = value.as_obj().ok_or(BAD_STATE)?;
        self.vm.heap.set_frozen(reference);
        self.vm.operands.truncate(root);
        self.push(value)?;
        Ok(())
    }

    /// Read stable binding data from one portable definition.
    #[inline(never)]
    pub(super) fn exec_code_definition(
        &mut self,
        module: &NamespaceRuntime,
    ) -> Result<(), FaultCode> {
        let reference = self.pop_obj()?;
        let code = match self.vm.heap.get(reference) {
            Object::NativeCode(code)
                if matches!(
                    code.kind,
                    lm_heap::PortableCodeKind::Function | lm_heap::PortableCodeKind::Class
                ) =>
            {
                (**code).clone()
            }
            _ => return Err(BAD_TYPE),
        };
        let artifact = code.artifact().ok_or(BAD_STATE)?;
        let decoded = artifact.root().module();
        let identity = artifact.root().identity();
        let value = self.alloc_definition_spec(module, &code, decoded, identity)?;
        self.push(value)
    }

    /// Resolve one fault trace only when a program inspects it.
    #[inline(never)]
    pub(super) fn exec_fault_locations(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        ty: u32,
        primary: bool,
    ) -> Result<(), FaultCode> {
        let reference = self.pop_obj()?;
        let trace = match self.vm.heap.get(reference) {
            Object::NativeFault { trace, .. } => trace.to_vec(),
            _ => return Err(BAD_TYPE),
        };
        let debug = lm_bytecode::debug::decode(&module.debug).map_err(|_| BAD_STATE)?;
        lm_bytecode::debug::validate(&debug, module).map_err(|_| BAD_STATE)?;
        let identity = module.identity().map_err(|_| BAD_STATE)?;
        if primary {
            if let Some(site) = trace.into_iter().next() {
                let location = self.alloc_fault_location(module, envs, &debug, identity, site)?;
                self.push(location)?;
                return Ok(());
            }
            let family = self.close_option_family(module, envs, ty)?;
            self.push(Value::EmptyCase { ty: family, arm: 1 })?;
            return Ok(());
        }

        let root = self.vm.operands.len();
        let mut locations = Vec::new();
        locations
            .try_reserve_exact(trace.len())
            .map_err(|_| FaultCode::HeapLimit)?;
        for site in trace {
            let location = self.alloc_fault_location(module, envs, &debug, identity, site)?;
            self.push(location)?;
            locations.push(location);
        }
        let list = self.alloc(Object::List {
            items: locations.into(),
            epoch: StructuralEpoch::default(),
        })?;
        self.vm.operands.truncate(root);
        self.push(list)?;
        Ok(())
    }

    /// Read the debug origin for the current code reification instruction.
    pub(super) fn current_code_origin(&self, module: &NamespaceRuntime) -> Option<[u8; 32]> {
        let frame = self.vm.frames.last()?;
        let instruction = frame.ip.checked_sub(1)?;
        let debug = lm_bytecode::debug::decode(&module.debug).ok()?;
        debug
            .code_origins
            .iter()
            .rev()
            .find(|origin| {
                origin.function == frame.func
                    && origin.block == frame.block
                    && origin.instruction == instruction
            })
            .map(|origin| origin.origin)
    }

    pub(super) fn syntax_view_class(
        class: lm_abi::syntax::SyntaxClass,
        node: u32,
        token: u32,
        trivia: u32,
    ) -> u32 {
        match class {
            lm_abi::syntax::SyntaxClass::Node | lm_abi::syntax::SyntaxClass::Invalid => node,
            lm_abi::syntax::SyntaxClass::Token => token,
            lm_abi::syntax::SyntaxClass::Trivia => trivia,
        }
    }

    /// Execute one public syntax instruction.
    #[inline(never)]
    pub(super) fn exec_syntax(
        &mut self,
        instr: ExtendedInstr,
        tree: u32,
        node: u32,
        token: u32,
        trivia: u32,
        builder: u32,
    ) -> Result<(), FaultCode> {
        match instr {
            ExtendedInstr::SyntaxTreeRoot => {
                let tree_ref = self.pop_obj()?;
                let (source, records, text, data) = self.syntax_tree_parts(tree_ref, tree)?;
                let view = lm_abi::syntax::SyntaxView::new(data.as_slice(), text.len())
                    .map_err(|_| FaultCode::BadCast)?;
                let root = view.record(view.root()).map_err(|_| FaultCode::BadCast)?;
                if !matches!(
                    root.class,
                    lm_abi::syntax::SyntaxClass::Node | lm_abi::syntax::SyntaxClass::Invalid
                ) {
                    return Err(FaultCode::BadCast);
                }
                if root.lo != 0 || root.hi as usize != text.len() {
                    return Err(FaultCode::BadCast);
                }
                let value = self.alloc_syntax_view(node, source, records, view.root())?;
                self.push(value)?;
            }
            ExtendedInstr::SyntaxKind
            | ExtendedInstr::SyntaxCategory
            | ExtendedInstr::SyntaxRangeStart
            | ExtendedInstr::SyntaxRangeEnd
            | ExtendedInstr::SyntaxText => {
                let element = self.pop_obj()?;
                let (_, _, text, data, index) =
                    self.syntax_element_parts(element, node, token, trivia)?;
                let view = lm_abi::syntax::SyntaxView::new(data.as_slice(), text.len())
                    .map_err(|_| FaultCode::BadCast)?;
                let record = view.record(index).map_err(|_| FaultCode::BadCast)?;
                match instr {
                    ExtendedInstr::SyntaxKind => self.push(Value::Int(i64::from(record.kind)))?,
                    ExtendedInstr::SyntaxCategory => {
                        self.push(Value::Int(i64::from(record.class as u8)))?
                    }
                    ExtendedInstr::SyntaxRangeStart => {
                        self.push(Value::Int(i64::from(record.lo)))?
                    }
                    ExtendedInstr::SyntaxRangeEnd => self.push(Value::Int(i64::from(record.hi)))?,
                    ExtendedInstr::SyntaxText => {
                        let slice = text
                            .slice(record.lo as usize, record.hi as usize)
                            .ok_or(FaultCode::BadCast)?;
                        let value = self.alloc(Object::Substring(slice))?;
                        self.push(value)?;
                    }
                    _ => unreachable!("the syntax scalar dispatcher receives a scalar operation"),
                }
            }
            ExtendedInstr::SyntaxChildren => {
                let element = self.pop_obj()?;
                let (source, records, text, data, index) =
                    self.syntax_element_parts(element, node, token, trivia)?;
                let view = lm_abi::syntax::SyntaxView::new(data.as_slice(), text.len())
                    .map_err(|_| FaultCode::BadCast)?;
                let record = view.record(index).map_err(|_| FaultCode::BadCast)?;
                let mut descriptors = Vec::new();
                descriptors
                    .try_reserve_exact(record.child_len as usize)
                    .map_err(|_| FaultCode::HeapLimit)?;
                for offset in 0..record.child_len {
                    let index = view.child(record, offset).map_err(|_| FaultCode::BadCast)?;
                    let child = view.record(index).map_err(|_| FaultCode::BadCast)?;
                    descriptors.push((
                        Self::syntax_view_class(child.class, node, token, trivia),
                        index,
                    ));
                }
                let base = self.vm.operands.len();
                for (class, index) in descriptors {
                    let child = self.alloc_syntax_view(class, source, records, index)?;
                    self.push(child)?;
                }
                let items = self.vm.operands.split_off(base);
                let list = self.alloc(Object::List {
                    items: items.into(),
                    epoch: StructuralEpoch::default(),
                })?;
                self.push(list)?;
            }
            ExtendedInstr::SyntaxDetach => {
                let element = self.pop_obj()?;
                let (_, _, text, data, index) =
                    self.syntax_element_parts(element, node, token, trivia)?;
                let detached = lm_abi::syntax::detach_syntax(data.as_slice(), text.len(), index)
                    .map_err(|_| FaultCode::BadCast)?;
                let source = text
                    .slice(detached.source_start as usize, detached.source_end as usize)
                    .ok_or(FaultCode::BadCast)?
                    .try_compact()
                    .map_err(|_| FaultCode::HeapLimit)?;
                let records = SharedBytes::try_from_slice(&detached.records)
                    .map_err(|_| FaultCode::HeapLimit)?;
                let view = lm_abi::syntax::SyntaxView::new(records.as_slice(), source.len())
                    .map_err(|_| FaultCode::BadCast)?;
                let record = view.record(detached.root).map_err(|_| FaultCode::BadCast)?;
                let class = Self::syntax_view_class(record.class, node, token, trivia);
                let source = self.alloc(Object::Str(source))?;
                self.push(source)?;
                let records = self.alloc(Object::Bytes(records))?;
                self.push(records)?;
                let value = self.alloc_syntax_view(class, source, records, detached.root)?;
                self.vm.operands.truncate(self.vm.operands.len() - 2);
                self.push(value)?;
            }
            ExtendedInstr::SyntaxBuildToken | ExtendedInstr::SyntaxBuildTrivia => {
                let text_ref = self.pop_obj()?;
                let kind = u16::try_from(self.pop_int()?).map_err(|_| FaultCode::BadCast)?;
                let builder_ref = self.pop_obj()?;
                match self.vm.heap.get(builder_ref) {
                    Object::Instance { class, fields, .. }
                        if *class == builder && fields.is_empty() => {}
                    _ => return Err(BAD_TYPE),
                }
                let text = match self.vm.heap.get(text_ref) {
                    Object::Str(text) => text.clone(),
                    _ => return Err(BAD_TYPE),
                };
                let (class, syntax_class) = if matches!(instr, ExtendedInstr::SyntaxBuildToken) {
                    (token, lm_abi::syntax::SyntaxClass::Token)
                } else {
                    (trivia, lm_abi::syntax::SyntaxClass::Trivia)
                };
                let encoded = lm_abi::syntax::build_syntax_leaf(syntax_class, kind, text.as_str())
                    .map_err(|_| FaultCode::BadCast)?;
                let records =
                    SharedBytes::try_from_slice(&encoded).map_err(|_| FaultCode::HeapLimit)?;
                let source = Value::Obj(text_ref);
                self.push(source)?;
                let records = self.alloc(Object::Bytes(records))?;
                self.push(records)?;
                let value = self.alloc_syntax_view(class, source, records, 0)?;
                self.vm.operands.truncate(self.vm.operands.len() - 2);
                self.push(value)?;
            }
            ExtendedInstr::SyntaxBuildNode => {
                let children_ref = self.pop_obj()?;
                let kind = u16::try_from(self.pop_int()?).map_err(|_| FaultCode::BadCast)?;
                let builder_ref = self.pop_obj()?;
                match self.vm.heap.get(builder_ref) {
                    Object::Instance { class, fields, .. }
                        if *class == builder && fields.is_empty() => {}
                    _ => return Err(BAD_TYPE),
                }
                let child_values = match self.vm.heap.get(children_ref) {
                    Object::List { items, .. } => {
                        let mut copy = Vec::new();
                        copy.try_reserve_exact(items.len())
                            .map_err(|_| FaultCode::HeapLimit)?;
                        copy.extend_from_slice(items);
                        copy
                    }
                    _ => return Err(BAD_TYPE),
                };
                let mut owned = Vec::new();
                owned
                    .try_reserve_exact(child_values.len())
                    .map_err(|_| FaultCode::HeapLimit)?;
                for child in child_values {
                    let child = child.as_obj().ok_or(BAD_TYPE)?;
                    let (_, _, source, records, index) =
                        self.syntax_element_parts(child, node, token, trivia)?;
                    owned.push((source, records, index));
                }
                let mut parts = Vec::new();
                parts
                    .try_reserve_exact(owned.len())
                    .map_err(|_| FaultCode::HeapLimit)?;
                for (source, records, index) in &owned {
                    parts.push(lm_abi::syntax::SyntaxPart {
                        source: source.as_str(),
                        records: records.as_slice(),
                        index: *index,
                    });
                }
                let built = lm_abi::syntax::build_syntax_node(kind, &parts)
                    .map_err(|_| FaultCode::BadCast)?;
                let source =
                    SharedText::try_from_string(built.source).map_err(|_| FaultCode::HeapLimit)?;
                let records = SharedBytes::try_from_slice(&built.records)
                    .map_err(|_| FaultCode::HeapLimit)?;
                let view = lm_abi::syntax::SyntaxView::new(records.as_slice(), source.len())
                    .map_err(|_| FaultCode::BadCast)?;
                let root = view.root();
                let source = self.alloc(Object::Str(source))?;
                self.push(source)?;
                let records = self.alloc(Object::Bytes(records))?;
                self.push(records)?;
                let value = self.alloc_syntax_view(node, source, records, root)?;
                self.vm.operands.truncate(self.vm.operands.len() - 2);
                self.push(value)?;
            }
            ExtendedInstr::SyntaxToTree => {
                let element = self.pop_obj()?;
                let (source, records, text, data, index) =
                    self.syntax_element_parts(element, node, token, trivia)?;
                let view = lm_abi::syntax::SyntaxView::new(data.as_slice(), text.len())
                    .map_err(|_| FaultCode::BadCast)?;
                let record = view.record(index).map_err(|_| FaultCode::BadCast)?;
                if !matches!(
                    record.class,
                    lm_abi::syntax::SyntaxClass::Node | lm_abi::syntax::SyntaxClass::Invalid
                ) {
                    return Err(FaultCode::BadCast);
                }
                if index == view.root() && record.lo == 0 && record.hi as usize == text.len() {
                    let value = self.alloc_syntax_tree(tree, source, records)?;
                    self.push(value)?;
                    return Ok(());
                }
                let detached = lm_abi::syntax::detach_syntax(data.as_slice(), text.len(), index)
                    .map_err(|_| FaultCode::BadCast)?;
                let source = text
                    .slice(detached.source_start as usize, detached.source_end as usize)
                    .ok_or(FaultCode::BadCast)?
                    .try_compact()
                    .map_err(|_| FaultCode::HeapLimit)?;
                let records = SharedBytes::try_from_slice(&detached.records)
                    .map_err(|_| FaultCode::HeapLimit)?;
                let source = self.alloc(Object::Str(source))?;
                self.push(source)?;
                let records = self.alloc(Object::Bytes(records))?;
                self.push(records)?;
                let value = self.alloc_syntax_tree(tree, source, records)?;
                self.vm.operands.truncate(self.vm.operands.len() - 2);
                self.push(value)?;
            }
            _ => unreachable!("the syntax dispatcher receives one syntax instruction"),
        }
        Ok(())
    }
}
