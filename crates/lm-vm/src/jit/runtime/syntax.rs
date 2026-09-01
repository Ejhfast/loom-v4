//! Fault, dynamic-value, and syntax runtime paths.

use super::*;

impl MachineRuntime<'_> {
    pub(super) fn syntax_roles(&self) -> Result<[u32; 5], HeapOperationResult> {
        let roles = [
            self.module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_TREE],
            self.module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_NODE],
            self.module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_TOKEN],
            self.module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_TRIVIA],
            self.module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_BUILDER],
        ];
        if roles.contains(&lm_bytecode::NO_ROLE) {
            return Err(HeapOperationResult::Fault(crate::FaultCode::MalformedState));
        }
        Ok(roles)
    }

    pub(super) fn syntax_tree_parts(
        &self,
        reference: u64,
        tree_class: u32,
    ) -> Result<SyntaxTreeParts, HeapOperationResult> {
        let reference = object_reference(reference);
        let Some(crate::Object::Instance { class, fields, .. }) =
            self.machine.vm.heap.try_get(reference)
        else {
            return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch));
        };
        if *class != tree_class {
            return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch));
        }
        let [source, records] = fields.as_slice() else {
            return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch));
        };
        let Some(source_ref) = source.as_obj() else {
            return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch));
        };
        let Some(records_ref) = records.as_obj() else {
            return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch));
        };
        let text = match self.machine.vm.heap.try_get(source_ref) {
            Some(crate::Object::Str(text)) => text.clone(),
            _ => return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        };
        let data = match self.machine.vm.heap.try_get(records_ref) {
            Some(crate::Object::Bytes(data)) => data.clone(),
            _ => return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        };
        Ok(SyntaxTreeParts {
            source: *source,
            records: *records,
            text,
            data,
        })
    }

    pub(super) fn syntax_element_parts(
        &self,
        reference: u64,
        node: u32,
        token: u32,
        trivia: u32,
    ) -> Result<SyntaxElementParts, HeapOperationResult> {
        let reference = object_reference(reference);
        let Some(crate::Object::Instance { class, fields, .. }) =
            self.machine.vm.heap.try_get(reference)
        else {
            return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch));
        };
        if *class != node && *class != token && *class != trivia {
            return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch));
        }
        let [source, records, Value::Int(index)] = fields.as_slice() else {
            return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch));
        };
        let Ok(index) = u32::try_from(*index) else {
            return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch));
        };
        let Some(source_ref) = source.as_obj() else {
            return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch));
        };
        let Some(records_ref) = records.as_obj() else {
            return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch));
        };
        let text = match self.machine.vm.heap.try_get(source_ref) {
            Some(crate::Object::Str(text)) => text.clone(),
            _ => return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        };
        let data = match self.machine.vm.heap.try_get(records_ref) {
            Some(crate::Object::Bytes(data)) => data.clone(),
            _ => return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        };
        Ok(SyntaxElementParts {
            source: *source,
            records: *records,
            text,
            data,
            index,
        })
    }

    pub(super) fn syntax_record(
        &self,
        reference: u64,
    ) -> Result<lm_abi::syntax::SyntaxRecord, HeapOperationResult> {
        let [_, node, token, trivia, _] = self.syntax_roles()?;
        let parts = self.syntax_element_parts(reference, node, token, trivia)?;
        let view = lm_abi::syntax::SyntaxView::new(parts.data.as_slice(), parts.text.len())
            .map_err(|_| HeapOperationResult::Fault(crate::FaultCode::BadCast))?;
        view.record(parts.index)
            .map_err(|_| HeapOperationResult::Fault(crate::FaultCode::BadCast))
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

    pub(super) fn syntax_result(&mut self, reference: ObjRef) -> HeapOperationResult {
        HeapOperationResult::Value {
            bits: object_bits(reference),
            heap: Some(self.machine.vm.heap.jit_view()),
        }
    }

    pub(super) fn syntax_leaf(
        &mut self,
        request: HeapOperationRequest<'_>,
        syntax_class: lm_abi::syntax::SyntaxClass,
    ) -> HeapOperationResult {
        let [_, _, token, trivia, builder] = match self.syntax_roles() {
            Ok(roles) => roles,
            Err(result) => return result,
        };
        let builder_ref = object_reference(request.first);
        match self.machine.vm.heap.try_get(builder_ref) {
            Some(crate::Object::Instance { class, fields, .. })
                if *class == builder && fields.is_empty() => {}
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        }
        let kind = match u16::try_from(request.second as i64) {
            Ok(kind) => kind,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let text_ref = object_reference(request.third);
        let text = match self.machine.vm.heap.try_get(text_ref) {
            Some(crate::Object::Str(text)) => text.clone(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        let encoded = match lm_abi::syntax::build_syntax_leaf(syntax_class, kind, text.as_str()) {
            Ok(encoded) => encoded,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let records = match SharedBytes::try_from_slice(&encoded) {
            Ok(records) => records,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        let records_ref =
            match self.allocate_heap_reference(crate::Object::Bytes(records), &request, &[]) {
                Ok(reference) => reference,
                Err(result) => return result,
            };
        let class = if matches!(syntax_class, lm_abi::syntax::SyntaxClass::Token) {
            token
        } else {
            trivia
        };
        let value = self.allocate_frozen_instance(
            class,
            vec![Value::Obj(text_ref), Value::Obj(records_ref), Value::Int(0)],
            &request,
            &[records_ref],
        );
        match value {
            Ok(reference) => self.syntax_result(reference),
            Err(result) => result,
        }
    }

    pub(super) fn runtime_fault_code(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let reference = object_reference(request.first);
        let code = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::NativeFault { code, .. }) => *code,
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        self.allocate_heap_object(crate::Object::Str(code.to_string().into()), &request)
    }

    pub(super) fn runtime_fault_denied(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let reference = object_reference(request.first);
        let reason = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Str(text)) => text.to_string(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        self.allocate_heap_object(
            crate::Object::NativeFault {
                code: crate::FaultCode::PolicyDenied,
                message: reason,
                op: None,
                trace: Box::default(),
            },
            &request,
        )
    }

    pub(super) fn runtime_dyn_pack(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let Some(value) = tagged_value(request.second, request.first) else {
            return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch);
        };
        let ty = request.third as u32;
        let environment = TypeEnvId((request.third >> 32) as u32);
        let closed = match self.envs.close(self.module, ty, environment) {
            Ok(closed) => closed,
            Err(_) => return HeapOperationResult::Interpreter,
        };
        self.allocate_heap_object(crate::Object::DynValue { value, ty: closed }, &request)
    }

    pub(super) fn runtime_syntax_tree_root(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let [tree, node, _, _, _] = match self.syntax_roles() {
            Ok(roles) => roles,
            Err(result) => return result,
        };
        let parts = match self.syntax_tree_parts(request.first, tree) {
            Ok(parts) => parts,
            Err(result) => return result,
        };
        let view = match lm_abi::syntax::SyntaxView::new(parts.data.as_slice(), parts.text.len()) {
            Ok(view) => view,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let root = match view.record(view.root()) {
            Ok(root) => root,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        if !matches!(
            root.class,
            lm_abi::syntax::SyntaxClass::Node | lm_abi::syntax::SyntaxClass::Invalid
        ) || root.lo != 0
            || root.hi as usize != parts.text.len()
        {
            return HeapOperationResult::Fault(crate::FaultCode::BadCast);
        }
        match self.allocate_frozen_instance(
            node,
            vec![
                parts.source,
                parts.records,
                Value::Int(i64::from(view.root())),
            ],
            &request,
            &[],
        ) {
            Ok(reference) => self.syntax_result(reference),
            Err(result) => result,
        }
    }

    pub(super) fn runtime_syntax_kind(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        match self.syntax_record(request.first) {
            Ok(record) => heap_int(i64::from(record.kind)),
            Err(result) => result,
        }
    }

    pub(super) fn runtime_syntax_category(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        match self.syntax_record(request.first) {
            Ok(record) => heap_int(i64::from(record.class as u8)),
            Err(result) => result,
        }
    }

    pub(super) fn runtime_syntax_range_start(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        match self.syntax_record(request.first) {
            Ok(record) => heap_int(i64::from(record.lo)),
            Err(result) => result,
        }
    }

    pub(super) fn runtime_syntax_range_end(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        match self.syntax_record(request.first) {
            Ok(record) => heap_int(i64::from(record.hi)),
            Err(result) => result,
        }
    }

    pub(super) fn runtime_syntax_text(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let [_, node, token, trivia, _] = match self.syntax_roles() {
            Ok(roles) => roles,
            Err(result) => return result,
        };
        let parts = match self.syntax_element_parts(request.first, node, token, trivia) {
            Ok(parts) => parts,
            Err(result) => return result,
        };
        let view = match lm_abi::syntax::SyntaxView::new(parts.data.as_slice(), parts.text.len()) {
            Ok(view) => view,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let record = match view.record(parts.index) {
            Ok(record) => record,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let Some(slice) = parts.text.slice(record.lo as usize, record.hi as usize) else {
            return HeapOperationResult::Fault(crate::FaultCode::BadCast);
        };
        self.allocate_heap_object(crate::Object::Substring(slice), &request)
    }

    pub(super) fn runtime_syntax_children(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let [_, node, token, trivia, _] = match self.syntax_roles() {
            Ok(roles) => roles,
            Err(result) => return result,
        };
        let parts = match self.syntax_element_parts(request.first, node, token, trivia) {
            Ok(parts) => parts,
            Err(result) => return result,
        };
        let view = match lm_abi::syntax::SyntaxView::new(parts.data.as_slice(), parts.text.len()) {
            Ok(view) => view,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let record = match view.record(parts.index) {
            Ok(record) => record,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let mut descriptors = Vec::new();
        if descriptors
            .try_reserve_exact(record.child_len as usize)
            .is_err()
        {
            return HeapOperationResult::HeapLimit;
        }
        for offset in 0..record.child_len {
            let index = match view.child(record, offset) {
                Ok(index) => index,
                Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
            };
            let child = match view.record(index) {
                Ok(child) => child,
                Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
            };
            descriptors.push((
                Self::syntax_view_class(child.class, node, token, trivia),
                index,
            ));
        }
        let mut items = Vec::new();
        let mut roots = Vec::new();
        if items.try_reserve_exact(descriptors.len()).is_err()
            || roots.try_reserve_exact(descriptors.len()).is_err()
        {
            return HeapOperationResult::HeapLimit;
        }
        for (class, index) in descriptors {
            let child = match self.allocate_frozen_instance(
                class,
                vec![parts.source, parts.records, Value::Int(i64::from(index))],
                &request,
                &roots,
            ) {
                Ok(reference) => reference,
                Err(result) => return result,
            };
            roots.push(child);
            items.push(Value::Obj(child));
        }
        let list = match self.allocate_heap_reference(
            crate::Object::List {
                items: items.into(),
                epoch: StructuralEpoch::default(),
            },
            &request,
            &roots,
        ) {
            Ok(reference) => reference,
            Err(result) => return result,
        };
        self.syntax_result(list)
    }

    pub(super) fn runtime_syntax_detach(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let [_, node, token, trivia, _] = match self.syntax_roles() {
            Ok(roles) => roles,
            Err(result) => return result,
        };
        let parts = match self.syntax_element_parts(request.first, node, token, trivia) {
            Ok(parts) => parts,
            Err(result) => return result,
        };
        let detached = match lm_abi::syntax::detach_syntax(
            parts.data.as_slice(),
            parts.text.len(),
            parts.index,
        ) {
            Ok(detached) => detached,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let source = match parts
            .text
            .slice(detached.source_start as usize, detached.source_end as usize)
            .ok_or(())
            .and_then(|text| text.try_compact().map_err(|_| ()))
        {
            Ok(source) => source,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        let records = match SharedBytes::try_from_slice(&detached.records) {
            Ok(records) => records,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        let view = match lm_abi::syntax::SyntaxView::new(records.as_slice(), source.len()) {
            Ok(view) => view,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let record = match view.record(detached.root) {
            Ok(record) => record,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let class = Self::syntax_view_class(record.class, node, token, trivia);
        let source_ref =
            match self.allocate_heap_reference(crate::Object::Str(source), &request, &[]) {
                Ok(reference) => reference,
                Err(result) => return result,
            };
        let records_ref = match self.allocate_heap_reference(
            crate::Object::Bytes(records),
            &request,
            &[source_ref],
        ) {
            Ok(reference) => reference,
            Err(result) => return result,
        };
        match self.allocate_frozen_instance(
            class,
            vec![
                Value::Obj(source_ref),
                Value::Obj(records_ref),
                Value::Int(i64::from(detached.root)),
            ],
            &request,
            &[source_ref, records_ref],
        ) {
            Ok(reference) => self.syntax_result(reference),
            Err(result) => result,
        }
    }

    pub(super) fn runtime_syntax_build_token(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.syntax_leaf(request, lm_abi::syntax::SyntaxClass::Token)
    }

    pub(super) fn runtime_syntax_build_trivia(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.syntax_leaf(request, lm_abi::syntax::SyntaxClass::Trivia)
    }

    pub(super) fn runtime_syntax_build_node(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let [_, node, token, trivia, builder] = match self.syntax_roles() {
            Ok(roles) => roles,
            Err(result) => return result,
        };
        let builder_ref = object_reference(request.first);
        match self.machine.vm.heap.try_get(builder_ref) {
            Some(crate::Object::Instance { class, fields, .. })
                if *class == builder && fields.is_empty() => {}
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        }
        let kind = match u16::try_from(request.second as i64) {
            Ok(kind) => kind,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let children_ref = object_reference(request.third);
        let child_values = match self.machine.vm.heap.try_get(children_ref) {
            Some(crate::Object::List { items, .. }) => items.to_vec(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        let mut owned = Vec::new();
        if owned.try_reserve_exact(child_values.len()).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        for child in child_values {
            let Some(reference) = child.as_obj() else {
                return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch);
            };
            let parts = match self.syntax_element_parts(object_bits(reference), node, token, trivia)
            {
                Ok(parts) => parts,
                Err(result) => return result,
            };
            owned.push((parts.text, parts.data, parts.index));
        }
        let mut parts = Vec::new();
        if parts.try_reserve_exact(owned.len()).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        for (source, records, index) in &owned {
            parts.push(lm_abi::syntax::SyntaxPart {
                source: source.as_str(),
                records: records.as_slice(),
                index: *index,
            });
        }
        let built = match lm_abi::syntax::build_syntax_node(kind, &parts) {
            Ok(built) => built,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let source = match SharedText::try_from_string(built.source) {
            Ok(source) => source,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        let records = match SharedBytes::try_from_slice(&built.records) {
            Ok(records) => records,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        let view = match lm_abi::syntax::SyntaxView::new(records.as_slice(), source.len()) {
            Ok(view) => view,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let root = view.root();
        let source_ref =
            match self.allocate_heap_reference(crate::Object::Str(source), &request, &[]) {
                Ok(reference) => reference,
                Err(result) => return result,
            };
        let records_ref = match self.allocate_heap_reference(
            crate::Object::Bytes(records),
            &request,
            &[source_ref],
        ) {
            Ok(reference) => reference,
            Err(result) => return result,
        };
        match self.allocate_frozen_instance(
            node,
            vec![
                Value::Obj(source_ref),
                Value::Obj(records_ref),
                Value::Int(i64::from(root)),
            ],
            &request,
            &[source_ref, records_ref],
        ) {
            Ok(reference) => self.syntax_result(reference),
            Err(result) => result,
        }
    }

    pub(super) fn runtime_syntax_to_tree(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let [tree, node, token, trivia, _] = match self.syntax_roles() {
            Ok(roles) => roles,
            Err(result) => return result,
        };
        let parts = match self.syntax_element_parts(request.first, node, token, trivia) {
            Ok(parts) => parts,
            Err(result) => return result,
        };
        let view = match lm_abi::syntax::SyntaxView::new(parts.data.as_slice(), parts.text.len()) {
            Ok(view) => view,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let record = match view.record(parts.index) {
            Ok(record) => record,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        if !matches!(
            record.class,
            lm_abi::syntax::SyntaxClass::Node | lm_abi::syntax::SyntaxClass::Invalid
        ) {
            return HeapOperationResult::Fault(crate::FaultCode::BadCast);
        }
        if parts.index == view.root() && record.lo == 0 && record.hi as usize == parts.text.len() {
            return match self.allocate_frozen_instance(
                tree,
                vec![parts.source, parts.records],
                &request,
                &[],
            ) {
                Ok(reference) => self.syntax_result(reference),
                Err(result) => result,
            };
        }
        let detached = match lm_abi::syntax::detach_syntax(
            parts.data.as_slice(),
            parts.text.len(),
            parts.index,
        ) {
            Ok(detached) => detached,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
        };
        let source = match parts
            .text
            .slice(detached.source_start as usize, detached.source_end as usize)
            .ok_or(())
            .and_then(|text| text.try_compact().map_err(|_| ()))
        {
            Ok(source) => source,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        let records = match SharedBytes::try_from_slice(&detached.records) {
            Ok(records) => records,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        let source_ref =
            match self.allocate_heap_reference(crate::Object::Str(source), &request, &[]) {
                Ok(reference) => reference,
                Err(result) => return result,
            };
        let records_ref = match self.allocate_heap_reference(
            crate::Object::Bytes(records),
            &request,
            &[source_ref],
        ) {
            Ok(reference) => reference,
            Err(result) => return result,
        };
        match self.allocate_frozen_instance(
            tree,
            vec![Value::Obj(source_ref), Value::Obj(records_ref)],
            &request,
            &[source_ref, records_ref],
        ) {
            Ok(reference) => self.syntax_result(reference),
            Err(result) => result,
        }
    }
}
