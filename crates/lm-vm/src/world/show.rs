//! Stable rendering of outcomes, values, and trace events.
//!
//! Every helper here is read-only. A dump never changes a world.

use super::*;

impl World {
    /// Build the frame locations of one stopped run for its holder.
    ///
    /// The origins come from the code of `target`. The holder
    /// allocates each `CodeLocation` with its own core roles. The top
    /// frame comes first.
    pub(super) fn inspect_stack(&mut self, holder: VmId, target: VmId) -> Result<Value, FaultCode> {
        let origins = self.execution_origins(target)?;
        let code = self.code_of(holder).clone();
        let root = self.machines[holder as usize].vm.operands.len();
        let mut locations = Vec::new();
        locations
            .try_reserve_exact(origins.len())
            .map_err(|_| FaultCode::HeapLimit)?;
        let result = (|| {
            for origin in origins {
                let location = self.machines[holder as usize].alloc_code_location(
                    code.as_ref(),
                    &mut self.envs,
                    origin,
                )?;
                self.machines[holder as usize].push(location)?;
                locations.push(location);
            }
            self.machines[holder as usize].alloc(Object::List {
                items: locations.into(),
                epoch: lm_heap::StructuralEpoch::default(),
            })
        })();
        self.machines[holder as usize].vm.operands.truncate(root);
        result
    }

    /// Resolve every frame of `target` to a code origin, top frame first.
    fn execution_origins(
        &mut self,
        target: VmId,
    ) -> Result<Vec<crate::machine::CodeOrigin>, FaultCode> {
        self.materialize_native_machine(target)?;
        let code = self.code_of(target);
        let debug =
            lm_bytecode::debug::decode(&code.debug).map_err(|_| FaultCode::MalformedState)?;
        lm_bytecode::debug::validate(&debug, code.as_ref())
            .map_err(|_| FaultCode::MalformedState)?;
        let identity = code.identity()?;
        let frames = &self.machines[target as usize].vm.frames;
        let mut origins = Vec::new();
        origins
            .try_reserve_exact(frames.len())
            .map_err(|_| FaultCode::HeapLimit)?;
        for frame in frames.iter().rev() {
            let site = lm_heap::FaultSite {
                function: frame.func,
                block: frame.block,
                instruction: frame.ip.saturating_sub(1),
            };
            origins.push(crate::machine::code_origin(
                code.as_ref(),
                &debug,
                identity,
                site,
            )?);
        }
        Ok(origins)
    }

    /// Render a terminal outcome as stable text.
    pub fn show_outcome(&self, outcome: &Outcome) -> String {
        match outcome {
            Outcome::Done(value) => {
                let code = self.root_code().as_ref();
                let entry = &code.funcs[code.entry as usize];
                let expected = ShowExpected::Module {
                    ty: entry.ret,
                    env: TypeEnvId::EMPTY,
                };
                let mut visited = Vec::new();
                let shown = self.show_value_inner(
                    code,
                    &self.machines[0].vm.heap,
                    *value,
                    Some(expected),
                    0,
                    &mut visited,
                );
                format!("Done({shown})")
            }
            Outcome::Fault(code) => format!("Fault({code})"),
        }
    }

    /// Render the retained guest locations of one machine fault.
    pub fn fault_context(&self, fault: &FaultRec) -> Vec<String> {
        let debug = lm_bytecode::debug::decode(&self.root_code().debug).ok();
        let identity = self.identity().ok();
        let mut lines = Vec::new();
        for site in &fault.trace {
            let Some(function) = self.root_code().funcs.get(site.function as usize) else {
                continue;
            };
            let mut offset = 0usize;
            let mut valid = true;
            for block in function.blocks.iter().take(site.block as usize) {
                let Some(next) = offset.checked_add(block.len()) else {
                    valid = false;
                    break;
                };
                offset = next;
            }
            if !valid
                || function
                    .blocks
                    .get(site.block as usize)
                    .is_none_or(|block| site.instruction as usize >= block.len())
            {
                continue;
            }
            let Some(offset) = offset.checked_add(site.instruction as usize) else {
                continue;
            };
            let hash = identity
                .and_then(|identity| identity.func_hashes.get(site.function as usize))
                .map(|hash| {
                    hash[..4]
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>()
                })
                .unwrap_or_else(|| "unknown".to_string());
            let mapping = debug.as_ref().and_then(|debug| {
                debug
                    .functions
                    .iter()
                    .rev()
                    .find(|mapping| mapping.function == site.function)
                    .and_then(|mapping| {
                        debug
                            .sources
                            .get(mapping.source as usize)
                            .map(|source| (mapping, source))
                    })
            });
            match mapping {
                Some((mapping, source)) => {
                    let start = mapping.lo as usize;
                    let prefix = source.text.as_bytes().get(..start).unwrap_or_default();
                    let line = prefix.iter().filter(|byte| **byte == b'\n').count() + 1;
                    let column = prefix
                        .iter()
                        .rposition(|byte| *byte == b'\n')
                        .map_or(prefix.len() + 1, |at| prefix.len() - at);
                    lines.push(format!(
                        "  at {} ({}:{line}:{column}, bytecode {offset}, {hash})",
                        function.name, source.path
                    ));
                }
                None => lines.push(format!(
                    "  at {} (bytecode {offset}, {hash})",
                    function.name
                )),
            }
        }
        lines
    }

    /// Render one root-machine value in a stable readable form.
    pub fn show_value(&self, value: Value) -> String {
        self.show_value_of(0, value)
    }

    /// Render one value of one machine.
    pub fn show_value_of(&self, vm: VmId, value: Value) -> String {
        let code = self.code_of(vm).as_ref();
        let mut visited = Vec::new();
        self.show_value_inner(
            code,
            &self.machines[vm as usize].vm.heap,
            value,
            None,
            0,
            &mut visited,
        )
    }

    /// Render one terminal result with the machine result type.
    pub fn show_result_of(&self, vm: VmId, value: Value) -> String {
        let machine = &self.machines[vm as usize];
        let code = self.code_of(vm).as_ref();
        let expected = machine.body_func.map(|func| ShowExpected::Module {
            ty: code.funcs[func as usize].ret,
            env: machine.witness,
        });
        let mut visited = Vec::new();
        self.show_value_inner(code, &machine.vm.heap, value, expected, 0, &mut visited)
    }

    /// Render one dynamic package payload and resume its machine.
    pub(super) fn handle_dynamic_render(&mut self, vm: VmId, value: Value, ty: u32) {
        let text = {
            let code = self.code_of(vm).clone();
            let code = code.as_ref();
            let mut visited = Vec::new();
            self.show_value_inner(
                code,
                &self.machines[vm as usize].vm.heap,
                value,
                Some(ShowExpected::Closed(ty)),
                0,
                &mut visited,
            )
        };
        let result = self.machines[vm as usize]
            .alloc(Object::Str(text.into()))
            .and_then(|value| self.machines[vm as usize].push(value));
        if let Err(code) = result {
            self.machines[vm as usize].set_fault(code, "dynamic rendering failed", None);
        }
    }

    /// Render the dynamic result of `target` for `vm` and resume `vm`.
    pub(super) fn handle_dynamic_render_ref(&mut self, vm: VmId, target: VmId, generation: u32) {
        let text = match self.dynamic_result_text(target, generation) {
            Ok(text) => text,
            Err(code) => {
                self.machines[vm as usize].set_fault(
                    code,
                    "the dynamic result is not available",
                    None,
                );
                return;
            }
        };
        let result = self.machines[vm as usize]
            .alloc(Object::Str(text.into()))
            .and_then(|value| self.machines[vm as usize].push(value));
        if let Err(code) = result {
            self.machines[vm as usize].set_fault(code, "dynamic rendering failed", None);
        }
    }

    /// Render the terminal value of one machine through its own code.
    ///
    /// A packed value renders through its stored type. A dynamic run
    /// renders through its body result type.
    fn dynamic_result_text(&self, target: VmId, generation: u32) -> Result<String, FaultCode> {
        self.dynamic_result_text_at(target, generation, 0)
    }

    fn dynamic_result_text_at(
        &self,
        target: VmId,
        generation: u32,
        depth: u32,
    ) -> Result<String, FaultCode> {
        if depth >= 32 {
            return Ok("...".to_string());
        }
        let machine = self
            .machines
            .get(target as usize)
            .filter(|machine| machine.generation == generation)
            .ok_or(FaultCode::InvalidVmState)?;
        let value = match &machine.vm.terminal {
            Some(Terminal::Done(value)) => *value,
            _ => return Err(FaultCode::InvalidVmState),
        };
        let packed = value
            .as_obj()
            .and_then(|reference| match machine.vm.heap.get(reference) {
                Object::DynValue { value, ty } => Some((*value, *ty)),
                _ => None,
            });
        let code = self.code_of(target).as_ref();
        let mut visited = Vec::new();
        if let Some((value, ty)) = packed {
            return Ok(self.show_value_inner(
                code,
                &machine.vm.heap,
                value,
                Some(ShowExpected::Closed(ty)),
                depth + 1,
                &mut visited,
            ));
        }
        if !machine.dynamic_result {
            return Err(FaultCode::InvalidVmState);
        }
        let expected = machine.body_func.map(|func| ShowExpected::Module {
            ty: code.funcs[func as usize].ret,
            env: machine.witness,
        });
        Ok(self.show_value_inner(
            code,
            &machine.vm.heap,
            value,
            expected,
            depth + 1,
            &mut visited,
        ))
    }

    pub(super) fn resolve_show_expected(
        &self,
        code: &NamespaceRuntime,
        expected: ShowExpected,
    ) -> Option<ShowExpected> {
        let mut current = expected;
        for _ in 0..=code.types.len() {
            let ShowExpected::Module { ty, env } = current else {
                return Some(current);
            };
            match code.types.get(ty as usize)? {
                BcType::Var(index) => {
                    let closed = *self.envs.env(env)?.types.get(*index as usize)?;
                    current = ShowExpected::Closed(closed);
                }
                _ => return Some(current),
            }
        }
        None
    }

    pub(super) fn show_option_shape(
        &self,
        code: &NamespaceRuntime,
        expected: ShowExpected,
    ) -> Option<(ShowOption, ShowExpected)> {
        let expected = self.resolve_show_expected(code, expected)?;
        let option = code.core_layout().option?;
        let some = code.core_layout().option_some?;
        let none = code.core_layout().option_none?;
        let (class, payload) = match expected {
            ShowExpected::Module { ty, env } => match code.types.get(ty as usize)? {
                BcType::Inst(class, args) if args.len() == 1 => {
                    (*class, ShowExpected::Module { ty: args[0], env })
                }
                _ => return None,
            },
            ShowExpected::Closed(ty) => match self.envs.ty(ty)? {
                ClosedType::Inst(class, args) if args.len() == 1 => {
                    (*class, ShowExpected::Closed(args[0]))
                }
                _ => return None,
            },
        };
        let case = if class == option {
            ShowOption::Family
        } else if class == some {
            ShowOption::Some
        } else if class == none {
            ShowOption::None
        } else {
            return None;
        };
        Some((case, payload))
    }

    pub(super) fn empty_matches_option(
        &self,
        code: &NamespaceRuntime,
        expected: ShowExpected,
        stored: u32,
    ) -> bool {
        let Some((_, expected_payload)) = self.show_option_shape(code, expected) else {
            return false;
        };
        let Some(ClosedType::Inst(class, args)) = self.envs.ty(stored) else {
            return false;
        };
        code.core_layout().option == Some(*class)
            && args.len() == 1
            && self.show_expected_equals_closed(code, expected_payload, args[0], 0)
    }

    pub(super) fn show_expected_equals_closed(
        &self,
        code: &NamespaceRuntime,
        expected: ShowExpected,
        closed: u32,
        depth: u32,
    ) -> bool {
        if depth > 64 {
            return false;
        }
        let Some(expected) = self.resolve_show_expected(code, expected) else {
            return false;
        };
        if let ShowExpected::Closed(found) = expected {
            return found == closed;
        }
        let ShowExpected::Module { ty, env } = expected else {
            return false;
        };
        let Some(source) = code.types.get(ty as usize) else {
            return false;
        };
        let Some(target) = self.envs.ty(closed) else {
            return false;
        };
        let child = |this: &Self, source: u32, target: u32| {
            this.show_expected_equals_closed(
                code,
                ShowExpected::Module { ty: source, env },
                target,
                depth + 1,
            )
        };
        match (source, target) {
            (BcType::Unit, ClosedType::Unit)
            | (BcType::Bool, ClosedType::Bool)
            | (BcType::Int, ClosedType::Int)
            | (BcType::Float, ClosedType::Float)
            | (BcType::Str, ClosedType::Str)
            | (BcType::Fault, ClosedType::Fault)
            | (BcType::Request, ClosedType::Request)
            | (BcType::PolicyTable, ClosedType::PolicyTable)
            | (BcType::Vm, ClosedType::Vm)
            | (BcType::Digest, ClosedType::Digest)
            | (BcType::VmSnapshot, ClosedType::VmSnapshot)
            | (BcType::Bytes, ClosedType::Bytes)
            | (BcType::FileHandle, ClosedType::FileHandle)
            | (BcType::ResourceHandle, ClosedType::ResourceHandle) => true,
            (BcType::HostResource, ClosedType::HostResource) => true,
            (BcType::Class(a), ClosedType::Class(b)) => a == b,
            (BcType::Inst(a, source), ClosedType::Inst(b, target)) => {
                a == b
                    && source.len() == target.len()
                    && source
                        .iter()
                        .zip(target)
                        .all(|(source, target)| child(self, *source, *target))
            }
            (BcType::List(source), ClosedType::List(target))
            | (BcType::Run(source), ClosedType::Run(target))
            | (BcType::Wait(source), ClosedType::Wait(target))
            | (BcType::RunSnapshot(source), ClosedType::RunSnapshot(target)) => {
                child(self, *source, *target)
            }
            (BcType::Map(a, b), ClosedType::Map(x, y))
            | (BcType::PendingCall(a, b), ClosedType::PendingCall(x, y))
            | (BcType::Handle(a, b), ClosedType::Handle(x, y)) => {
                child(self, *a, *x) && child(self, *b, *y)
            }
            (BcType::Tuple(source), ClosedType::Tuple(target)) => {
                source.len() == target.len()
                    && source
                        .iter()
                        .zip(target)
                        .all(|(source, target)| child(self, *source, *target))
            }
            (
                BcType::Fn(params, muts, ret, row),
                ClosedType::Fn(other, flags, result, closed_row),
            ) => {
                muts == flags
                    && params.len() == other.len()
                    && params
                        .iter()
                        .zip(other)
                        .all(|(source, target)| child(self, *source, *target))
                    && child(self, *ret, *result)
                    && self.envs.close_row(code, row, env) == *closed_row
            }
            (BcType::Op(op, source), ClosedType::Op(other, target)) => {
                op == other && child(self, *source, *target)
            }
            _ => false,
        }
    }

    pub(super) fn show_list_element(
        &self,
        code: &NamespaceRuntime,
        expected: ShowExpected,
    ) -> Option<ShowExpected> {
        match self.resolve_show_expected(code, expected)? {
            ShowExpected::Module { ty, env } => match code.types.get(ty as usize)? {
                BcType::List(element) => Some(ShowExpected::Module { ty: *element, env }),
                _ => None,
            },
            ShowExpected::Closed(ty) => match self.envs.ty(ty)? {
                ClosedType::List(element) => Some(ShowExpected::Closed(*element)),
                _ => None,
            },
        }
    }

    pub(super) fn show_map_elements(
        &self,
        code: &NamespaceRuntime,
        expected: ShowExpected,
    ) -> Option<(ShowExpected, ShowExpected)> {
        match self.resolve_show_expected(code, expected)? {
            ShowExpected::Module { ty, env } => match code.types.get(ty as usize)? {
                BcType::Map(key, value) => Some((
                    ShowExpected::Module { ty: *key, env },
                    ShowExpected::Module { ty: *value, env },
                )),
                _ => None,
            },
            ShowExpected::Closed(ty) => match self.envs.ty(ty)? {
                ClosedType::Map(key, value) => {
                    Some((ShowExpected::Closed(*key), ShowExpected::Closed(*value)))
                }
                _ => None,
            },
        }
    }

    pub(super) fn show_tuple_elements(
        &self,
        code: &NamespaceRuntime,
        expected: ShowExpected,
    ) -> Option<Vec<ShowExpected>> {
        match self.resolve_show_expected(code, expected)? {
            ShowExpected::Module { ty, env } => match code.types.get(ty as usize)? {
                BcType::Tuple(elements) => Some(
                    elements
                        .iter()
                        .map(|ty| ShowExpected::Module { ty: *ty, env })
                        .collect(),
                ),
                _ => None,
            },
            ShowExpected::Closed(ty) => match self.envs.ty(ty)? {
                ClosedType::Tuple(elements) => Some(
                    elements
                        .iter()
                        .map(|ty| ShowExpected::Closed(*ty))
                        .collect(),
                ),
                _ => None,
            },
        }
    }

    pub(super) fn show_value_inner(
        &self,
        code: &NamespaceRuntime,
        heap: &Heap,
        value: Value,
        expected: Option<ShowExpected>,
        depth: u32,
        visited: &mut Vec<ObjRef>,
    ) -> String {
        const MAX_SHOW_DEPTH: u32 = 32;
        if let Some(expected) = expected {
            if let Some((case, payload)) = self.show_option_shape(code, expected) {
                let none = case == ShowOption::None
                    || (case == ShowOption::Family
                        && matches!(value, Value::EmptyCase { ty, arm: 1 } if self.empty_matches_option(code, expected, ty)));
                if none {
                    return "None".to_string();
                }
                let inner =
                    self.show_value_inner(code, heap, value, Some(payload), depth + 1, visited);
                return format!("Some({inner})");
            }
        }
        match value {
            Value::Unit => "()".to_string(),
            Value::Bool(v) => v.to_string(),
            Value::Int(v) => v.to_string(),
            Value::Float(bits) => f64::from_bits(bits).to_string(),
            Value::Char(value) => format!("{value:?}"),
            Value::Op(op) => format!(
                "<op {}>",
                code.bundle().op_name(op).unwrap_or("<invalid operation>")
            ),
            Value::Callback(reference) => format!("<callback {}>", reference.slot),
            Value::EmptyCase { arm: 1, .. } => "None".to_string(),
            Value::EmptyCase { ty, arm } => format!("<empty type {ty} arm {arm}>"),
            Value::Uninit => "<uninit>".to_string(),
            Value::Obj(r) => {
                if depth >= MAX_SHOW_DEPTH {
                    return "...".to_string();
                }
                if visited.contains(&r) {
                    return "<cycle>".to_string();
                }
                if let Some(text) = heap.text(r) {
                    return render_string(text.as_str());
                }
                match heap.get(r) {
                    Object::Str(text) | Object::Substring(text) => render_string(text),
                    Object::List { items, .. } => {
                        visited.push(r);
                        let element = expected.and_then(|ty| self.show_list_element(code, ty));
                        let parts: Vec<String> = items
                            .iter()
                            .map(|v| {
                                self.show_value_inner(code, heap, *v, element, depth + 1, visited)
                            })
                            .collect();
                        visited.pop();
                        format!("[{}]", parts.join(", "))
                    }
                    Object::Map { entries, .. } => {
                        visited.push(r);
                        let elements = expected.and_then(|ty| self.show_map_elements(code, ty));
                        let parts: Vec<String> = entries
                            .iter()
                            .filter(|entry| entry.is_live())
                            .map(|entry| {
                                format!(
                                    "{}: {}",
                                    self.show_value_inner(
                                        code,
                                        heap,
                                        entry.key,
                                        elements.map(|pair| pair.0),
                                        depth + 1,
                                        visited,
                                    ),
                                    self.show_value_inner(
                                        code,
                                        heap,
                                        entry.value,
                                        elements.map(|pair| pair.1),
                                        depth + 1,
                                        visited,
                                    )
                                )
                            })
                            .collect();
                        visited.pop();
                        format!("{{{}}}", parts.join(", "))
                    }
                    Object::Tuple { items } => {
                        visited.push(r);
                        let elements = expected.and_then(|ty| self.show_tuple_elements(code, ty));
                        let parts: Vec<String> = items
                            .iter()
                            .enumerate()
                            .map(|(index, v)| {
                                self.show_value_inner(
                                    code,
                                    heap,
                                    *v,
                                    elements
                                        .as_ref()
                                        .and_then(|types| types.get(index))
                                        .copied(),
                                    depth + 1,
                                    visited,
                                )
                            })
                            .collect();
                        visited.pop();
                        if parts.len() == 1 {
                            format!("({},)", parts[0])
                        } else {
                            format!("({})", parts.join(", "))
                        }
                    }
                    Object::Instance { class, fields, env } => {
                        visited.push(r);
                        let bc = &code.classes[*class as usize];
                        let text = if bc.kind == BcClassKind::Case {
                            // A case instance prints in constructor
                            // form with its short arm name.
                            let short = bc.name.rsplit('.').next().unwrap_or(&bc.name);
                            if fields.is_empty() {
                                short.to_string()
                            } else {
                                let parts: Vec<String> = fields
                                    .iter()
                                    .zip(bc.fields.iter())
                                    .map(|(v, (_, ty))| {
                                        self.show_value_inner(
                                            code,
                                            heap,
                                            *v,
                                            Some(ShowExpected::Module {
                                                ty: *ty,
                                                env: env.env(),
                                            }),
                                            depth + 1,
                                            visited,
                                        )
                                    })
                                    .collect();
                                format!("{}({})", short, parts.join(", "))
                            }
                        } else {
                            let parts: Vec<String> = bc
                                .fields
                                .iter()
                                .zip(fields.iter())
                                .map(|((name, ty), v)| {
                                    format!(
                                        "{}: {}",
                                        name,
                                        self.show_value_inner(
                                            code,
                                            heap,
                                            *v,
                                            Some(ShowExpected::Module {
                                                ty: *ty,
                                                env: env.env(),
                                            }),
                                            depth + 1,
                                            visited,
                                        )
                                    )
                                })
                                .collect();
                            format!("{}{{{}}}", bc.name, parts.join(", "))
                        };
                        visited.pop();
                        text
                    }
                    Object::Closure { func, .. } => {
                        format!("<closure {}>", code.funcs[*func as usize].name)
                    }
                    Object::StrBuilder(buf) => match buf.byte_len() {
                        Some(len) => format!("<StringBuilder length {len}>"),
                        None => "<finished StringBuilder>".to_string(),
                    },
                    Object::ByteBuf(bytes) => match bytes.len() {
                        Some(len) => format!("<ByteBuffer length {len}>"),
                        None => "<finished ByteBuffer>".to_string(),
                    },
                    Object::Bytes(bytes) => format!("<Bytes len {}>", bytes.len()),
                    Object::NativeFileHandle { resource } => {
                        if *resource == 0 {
                            "<file closed>".to_string()
                        } else {
                            format!("<file {resource}>")
                        }
                    }
                    Object::NativeResourceHandle { surface, resource } => {
                        format!("<resource {resource} of machine {surface}>")
                    }
                    Object::NativeVm { image, generation } => {
                        format!("<vm {image}:{generation}>")
                    }
                    Object::NativeRun { vm } => format!("<run {vm}>"),
                    Object::NativeDynRef { vm, generation } => self
                        .dynamic_result_text_at(*vm, *generation, depth + 1)
                        .unwrap_or_else(|_| format!("<dynamic result {vm}:{generation}>")),
                    Object::NativeCode(code) => {
                        format!(
                            "<{:?} slot {:?} bytes {}>",
                            code.kind,
                            code.slot,
                            code.encoded().map_or(0, lm_heap::SharedBytes::len)
                        )
                    }
                    Object::NativeCodeHandle {
                        image,
                        generation,
                        instance,
                        kind,
                        index,
                    } => format!(
                        "<{kind:?} {index} in instance {instance} of VM {image}:{generation}>"
                    ),
                    Object::NativeSlotChange { slot, kind, .. } => {
                        format!("<{kind:?} change for slot {slot}>")
                    }
                    Object::NativeTable { vm } => format!("<table {vm}>"),
                    Object::NativeRequest { .. } => "<request>".to_string(),
                    Object::NativeCall { op, .. } => {
                        format!(
                            "<call {}>",
                            code.bundle().op_name(*op).unwrap_or("<invalid operation>")
                        )
                    }
                    Object::NativeFault { code, .. } => code.to_string(),
                    Object::NativeDigest(bytes) => render_digest(bytes),
                    Object::NativeHandle { proc, generation } => {
                        format!("<proc {proc}.{generation}>")
                    }
                    Object::NativeSnapshot(image) => {
                        format!("<snapshot {} bytes>", image.len())
                    }
                    Object::NativeSnapshotRef { image } => {
                        format!("<snapshot {image}>")
                    }
                    Object::NativeWait { owner, token } => {
                        format!("<wait {token} of machine {owner}>")
                    }
                    Object::NativeTcpStream { resource } => {
                        if *resource == 0 {
                            "<TCP stream closed>".to_string()
                        } else {
                            format!("<TCP stream {resource}>")
                        }
                    }
                    Object::NativeTcpListener { resource } => {
                        if *resource == 0 {
                            "<TCP listener closed>".to_string()
                        } else {
                            format!("<TCP listener {resource}>")
                        }
                    }
                    Object::NativeTlsStream { resource } => {
                        if *resource == 0 {
                            "<TLS stream closed>".to_string()
                        } else {
                            format!("<TLS stream {resource}>")
                        }
                    }
                    Object::NativeRawMode { resource } => {
                        if *resource == 0 {
                            "<raw terminal mode closed>".to_string()
                        } else {
                            format!("<raw terminal mode {resource}>")
                        }
                    }
                    Object::NativeSignalStream { resource } => {
                        if *resource == 0 {
                            "<signal stream closed>".to_string()
                        } else {
                            format!("<signal stream {resource}>")
                        }
                    }
                    Object::NativePipeReader { resource } => {
                        if *resource == 0 {
                            "<pipe reader closed>".to_string()
                        } else {
                            format!("<pipe reader {resource}>")
                        }
                    }
                    Object::NativePipeWriter { resource } => {
                        if *resource == 0 {
                            "<pipe writer closed>".to_string()
                        } else {
                            format!("<pipe writer {resource}>")
                        }
                    }
                    Object::NativeChild { resource } => {
                        if *resource == 0 {
                            "<child closed>".to_string()
                        } else {
                            format!("<child {resource}>")
                        }
                    }
                    Object::NativeUdpSocket { resource } => {
                        if *resource == 0 {
                            "<UDP socket closed>".to_string()
                        } else {
                            format!("<UDP socket {resource}>")
                        }
                    }
                    Object::NativeHostResource { kind, resource } => {
                        let name = self
                            .root_code()
                            .bundle()
                            .resource_by_identity(*kind)
                            .and_then(|slot| code.bundle().resource(slot))
                            .map(|resource| resource.name.as_str())
                            .unwrap_or("extension resource");
                        if *resource == 0 {
                            format!("<{name} closed>")
                        } else {
                            format!("<{name} {resource}>")
                        }
                    }
                    Object::DynValue { value, ty } => {
                        visited.push(r);
                        let text = self.show_value_inner(
                            code,
                            heap,
                            *value,
                            Some(ShowExpected::Closed(*ty)),
                            depth + 1,
                            visited,
                        );
                        visited.pop();
                        format!("DynValue({text})")
                    }
                    Object::NativeRegex(regex) => format!("re{:?}", regex.source()),
                    Object::NativeRegexMatch(matched) => {
                        format!("<match {}..{}>", matched.start, matched.end)
                    }
                    Object::NativeCodeDescriptor(descriptor) => match &descriptor.member {
                        Some(member) => format!("<member {member}>"),
                        None => format!("<declaration {}>", descriptor.declaration),
                    },
                    Object::NativeLinkedCode(linked) => match linked.kind {
                        lm_heap::LinkedCodeKind::Module => "<linked module>".to_string(),
                        lm_heap::LinkedCodeKind::Open => "<opened code>".to_string(),
                    },
                }
            }
        }
    }

    /// The report label of one function value: every name that binds
    /// it, or the code label when no name binds it.
    ///
    /// Two modules with equal bodies share one function value, so a
    /// single label would hide one of the two names. A closure body
    /// and the entry take no binding and keep their code label.
    pub(super) fn func_label(&self, code: &NamespaceRuntime, func: u32) -> String {
        let keys: Vec<&str> = self
            .root_code()
            .bindings
            .iter()
            .filter(|b| b.func == func)
            .map(|b| b.key.as_str())
            .collect();
        if keys.is_empty() {
            code.funcs[func as usize].name.clone()
        } else {
            keys.join(", ")
        }
    }

    /// Render the live root-machine state: outcome, heap statistics,
    /// frame count, and every live object in slot order.
    pub fn dump_live(&mut self, outcome: &Outcome) -> String {
        use std::fmt::Write as _;
        let materialization = self.materialize_native_machine(0);
        let m = &self.machines[0];
        let mut out = String::new();
        let _ = writeln!(out, "outcome: {}", self.show_outcome(outcome));
        if let Err(code) = materialization {
            let _ = writeln!(out, "native state: {code}");
        }
        let s = m.vm.heap.stats();
        let _ = writeln!(
            out,
            "heap: live={} slots={} pages={} free={} used_bytes={} cap_bytes={} collections={}",
            s.live, s.slots, s.pages, s.free, s.used_bytes, s.cap_bytes, s.collections
        );
        let _ = writeln!(out, "frames: {} active", m.vm.frames.len());
        for frame in &m.vm.frames {
            let _ = writeln!(
                out,
                "  frame {} block {} ip {}",
                self.func_label(self.code_for_namespace(m.namespace).as_ref(), frame.func),
                frame.block,
                frame.ip
            );
        }
        let _ = writeln!(out, "objects:");
        m.vm.heap.for_each_live(|r, frozen, object| {
            let state = if frozen { "frozen" } else { "mutable" };
            let mut visited = Vec::new();
            let _ = writeln!(
                out,
                "  obj {} gen {} {} {} {}",
                r.slot,
                r.generation,
                object.shape().name,
                state,
                self.show_value_inner(
                    self.code_for_namespace(m.namespace).as_ref(),
                    &m.vm.heap,
                    Value::Obj(r),
                    None,
                    0,
                    &mut visited,
                )
            );
        });
        out
    }
}

/// Render one proc trace event as one stable line.
pub(crate) fn show_trace_event(event: &TraceEvent) -> String {
    match event {
        TraceEvent::Spawn {
            parent,
            proc,
            generation,
        } => format!("spawn parent {parent} proc {proc} gen {generation}"),
        TraceEvent::Send { from, to, accepted } => {
            format!("send from {from} to {to} accepted {accepted}")
        }
        TraceEvent::Receive { proc, closed } => format!("receive proc {proc} closed {closed}"),
        TraceEvent::Close { proc, first } => format!("close proc {proc} first {first}"),
        TraceEvent::Block { vm, kind, target } => {
            let what = match kind {
                TraceBlock::Receive => "receive".to_string(),
                TraceBlock::Send => format!("send target {target}"),
                TraceBlock::Done => format!("done target {target}"),
                TraceBlock::Wait => "wait".to_string(),
                TraceBlock::Snapshot => format!("snapshot target {target}"),
            };
            format!("block vm {vm} on {what}")
        }
        TraceEvent::Unblock { vm } => format!("unblock vm {vm}"),
        TraceEvent::Pause { proc } => format!("pause proc {proc}"),
        TraceEvent::Resume { proc } => format!("resume proc {proc}"),
        TraceEvent::Terminal { proc, faulted } => {
            format!("terminal proc {proc} faulted {faulted}")
        }
    }
}

/// Render one canonical graph digest as lower-case hexadecimal.
pub(crate) fn render_digest(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Render a string value with quotation marks and escapes.
pub(crate) fn render_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
