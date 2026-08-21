//! Stable rendering of outcomes, values, and trace events.
//!
//! Every helper here is read-only. A dump never changes a world.

use super::*;

impl World {
    /// Render a terminal outcome as stable text.
    pub fn show_outcome(&self, outcome: &Outcome) -> String {
        match outcome {
            Outcome::Done(value) => {
                let code = &self.module.funcs[self.module.entry as usize];
                let expected = ShowExpected::Module {
                    ty: code.ret,
                    env: TypeEnvId::EMPTY,
                };
                let mut visited = Vec::new();
                let shown = self.show_value_inner(
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
        let debug = lm_bytecode::debug::decode(&self.module.debug).ok();
        let identity = self.identity().ok();
        let mut lines = Vec::new();
        for site in &fault.trace {
            let Some(function) = self.module.funcs.get(site.function as usize) else {
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
        let mut visited = Vec::new();
        self.show_value_inner(
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
        let expected = machine.body_func.map(|func| ShowExpected::Module {
            ty: self.module.funcs[func as usize].ret,
            env: machine.witness,
        });
        let mut visited = Vec::new();
        self.show_value_inner(&machine.vm.heap, value, expected, 0, &mut visited)
    }

    /// Render one dynamic package payload and resume its machine.
    pub(super) fn handle_dynamic_render(&mut self, vm: VmId, value: Value, ty: u32) {
        let text = {
            let mut visited = Vec::new();
            self.show_value_inner(
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

    pub(super) fn resolve_show_expected(&self, expected: ShowExpected) -> Option<ShowExpected> {
        let mut current = expected;
        for _ in 0..=self.module.types.len() {
            let ShowExpected::Module { ty, env } = current else {
                return Some(current);
            };
            match self.module.types.get(ty as usize)? {
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
        expected: ShowExpected,
    ) -> Option<(ShowOption, ShowExpected)> {
        let expected = self.resolve_show_expected(expected)?;
        let option = self.core.option?;
        let some = self.core.option_some?;
        let none = self.core.option_none?;
        let (class, payload) = match expected {
            ShowExpected::Module { ty, env } => match self.module.types.get(ty as usize)? {
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

    pub(super) fn empty_matches_option(&self, expected: ShowExpected, stored: u32) -> bool {
        let Some((_, expected_payload)) = self.show_option_shape(expected) else {
            return false;
        };
        let Some(ClosedType::Inst(class, args)) = self.envs.ty(stored) else {
            return false;
        };
        self.core.option == Some(*class)
            && args.len() == 1
            && self.show_expected_equals_closed(expected_payload, args[0], 0)
    }

    pub(super) fn show_expected_equals_closed(
        &self,
        expected: ShowExpected,
        closed: u32,
        depth: u32,
    ) -> bool {
        if depth > 64 {
            return false;
        }
        let Some(expected) = self.resolve_show_expected(expected) else {
            return false;
        };
        if let ShowExpected::Closed(found) = expected {
            return found == closed;
        }
        let ShowExpected::Module { ty, env } = expected else {
            return false;
        };
        let Some(source) = self.module.types.get(ty as usize) else {
            return false;
        };
        let Some(target) = self.envs.ty(closed) else {
            return false;
        };
        let child = |this: &Self, source: u32, target: u32| {
            this.show_expected_equals_closed(
                ShowExpected::Module { ty: source, env },
                target,
                depth + 1,
            )
        };
        match (source, target) {
            (BcType::Unit, ClosedType::Unit)
            | (BcType::Bool, ClosedType::Bool)
            | (BcType::Int, ClosedType::Int)
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
                    && self.envs.close_row(&self.module, row, env) == *closed_row
            }
            (BcType::Op(op, source), ClosedType::Op(other, target)) => {
                op == other && child(self, *source, *target)
            }
            _ => false,
        }
    }

    pub(super) fn show_list_element(&self, expected: ShowExpected) -> Option<ShowExpected> {
        match self.resolve_show_expected(expected)? {
            ShowExpected::Module { ty, env } => match self.module.types.get(ty as usize)? {
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
        expected: ShowExpected,
    ) -> Option<(ShowExpected, ShowExpected)> {
        match self.resolve_show_expected(expected)? {
            ShowExpected::Module { ty, env } => match self.module.types.get(ty as usize)? {
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

    pub(super) fn show_tuple_elements(&self, expected: ShowExpected) -> Option<Vec<ShowExpected>> {
        match self.resolve_show_expected(expected)? {
            ShowExpected::Module { ty, env } => match self.module.types.get(ty as usize)? {
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
        heap: &Heap,
        value: Value,
        expected: Option<ShowExpected>,
        depth: u32,
        visited: &mut Vec<ObjRef>,
    ) -> String {
        const MAX_SHOW_DEPTH: u32 = 32;
        if let Some(expected) = expected {
            if let Some((case, payload)) = self.show_option_shape(expected) {
                let none = case == ShowOption::None
                    || (case == ShowOption::Family
                        && matches!(value, Value::EmptyCase { ty, arm: 1 } if self.empty_matches_option(expected, ty)));
                if none {
                    return "None".to_string();
                }
                let inner = self.show_value_inner(heap, value, Some(payload), depth + 1, visited);
                return format!("Some({inner})");
            }
        }
        match value {
            Value::Unit => "()".to_string(),
            Value::Bool(v) => v.to_string(),
            Value::Int(v) => v.to_string(),
            Value::Char(value) => format!("{value:?}"),
            Value::Op(op) => format!("<op {}>", lm_abi::op_name(op)),
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
                match heap.get(r) {
                    Object::Str(text) => render_string(text),
                    Object::Substring(text) => render_string(text),
                    Object::List { items, .. } => {
                        visited.push(r);
                        let element = expected.and_then(|ty| self.show_list_element(ty));
                        let parts: Vec<String> = items
                            .iter()
                            .map(|v| self.show_value_inner(heap, *v, element, depth + 1, visited))
                            .collect();
                        visited.pop();
                        format!("[{}]", parts.join(", "))
                    }
                    Object::Map { entries, .. } => {
                        visited.push(r);
                        let elements = expected.and_then(|ty| self.show_map_elements(ty));
                        let parts: Vec<String> = entries
                            .iter()
                            .map(|(k, v)| {
                                format!(
                                    "{}: {}",
                                    self.show_value_inner(
                                        heap,
                                        *k,
                                        elements.map(|pair| pair.0),
                                        depth + 1,
                                        visited,
                                    ),
                                    self.show_value_inner(
                                        heap,
                                        *v,
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
                        let elements = expected.and_then(|ty| self.show_tuple_elements(ty));
                        let parts: Vec<String> = items
                            .iter()
                            .enumerate()
                            .map(|(index, v)| {
                                self.show_value_inner(
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
                        let bc = &self.module.classes[*class as usize];
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
                        format!("<closure {}>", self.module.funcs[*func as usize].name)
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
                    Object::NativeCode(code) => {
                        format!(
                            "<{:?} index {} bytes {}>",
                            code.kind,
                            code.index,
                            code.bytes.len()
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
                        format!("<call {}>", lm_abi::op_name(*op))
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
                    Object::DynValue { value, ty } => {
                        visited.push(r);
                        let text = self.show_value_inner(
                            heap,
                            *value,
                            Some(ShowExpected::Closed(*ty)),
                            depth + 1,
                            visited,
                        );
                        visited.pop();
                        format!("DynValue({text})")
                    }
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
    pub(super) fn func_label(&self, func: u32) -> String {
        let keys: Vec<&str> = self
            .module
            .bindings
            .iter()
            .filter(|b| b.func == func)
            .map(|b| b.key.as_str())
            .collect();
        if keys.is_empty() {
            self.module.funcs[func as usize].name.clone()
        } else {
            keys.join(", ")
        }
    }

    /// Render the live root-machine state: outcome, heap statistics,
    /// frame count, and every live object in slot order.
    pub fn dump_live(&self, outcome: &Outcome) -> String {
        use std::fmt::Write as _;
        let m = &self.machines[0];
        let mut out = String::new();
        let _ = writeln!(out, "outcome: {}", self.show_outcome(outcome));
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
                self.func_label(frame.func),
                frame.block,
                frame.ip
            );
        }
        let _ = writeln!(out, "objects:");
        m.vm.heap.for_each_live(|r, frozen, _object| {
            let state = if frozen { "frozen" } else { "mutable" };
            let mut visited = Vec::new();
            let object = m.vm.heap.get(r);
            let _ = writeln!(
                out,
                "  obj {} gen {} {} {} {}",
                r.slot,
                r.generation,
                object.shape().name,
                state,
                self.show_value_inner(&m.vm.heap, Value::Obj(r), None, 0, &mut visited)
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
