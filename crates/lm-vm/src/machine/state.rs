//! Machine construction, terminal state, and fault traces.

use super::*;

impl Machine {
    /// Return the class method table for one runtime value.
    pub(super) fn virtual_class(
        &self,
        module: &NamespaceRuntime,
        value: Value,
    ) -> Result<u32, FaultCode> {
        match value {
            Value::Unit => {
                let class = module.core_roles[lm_bytecode::corepin::ROLE_UNIT];
                if class == lm_bytecode::NO_ROLE {
                    Err(BAD_TYPE)
                } else {
                    Ok(class)
                }
            }
            Value::Int(_) => {
                let class = module.core_roles[lm_bytecode::corepin::ROLE_INT];
                if class == lm_bytecode::NO_ROLE {
                    Err(BAD_TYPE)
                } else {
                    Ok(class)
                }
            }
            Value::Float(_) => {
                let class = module.core_roles[lm_bytecode::corepin::ROLE_FLOAT];
                if class == lm_bytecode::NO_ROLE {
                    Err(BAD_TYPE)
                } else {
                    Ok(class)
                }
            }
            Value::Bool(_) => {
                let class = module.core_roles[lm_bytecode::corepin::ROLE_BOOL];
                if class == lm_bytecode::NO_ROLE {
                    Err(BAD_TYPE)
                } else {
                    Ok(class)
                }
            }
            Value::Char(_) => {
                let class = module.core_roles[lm_bytecode::corepin::ROLE_CHAR];
                if class == lm_bytecode::NO_ROLE {
                    Err(BAD_TYPE)
                } else {
                    Ok(class)
                }
            }
            Value::Obj(reference) if self.vm.heap.is_compact_text(reference) => {
                let class = module.core_roles[lm_bytecode::corepin::ROLE_SUBSTRING];
                if class == lm_bytecode::NO_ROLE {
                    Err(BAD_TYPE)
                } else {
                    Ok(class)
                }
            }
            Value::Obj(reference) => match self.vm.heap.get(reference) {
                Object::Instance { class, .. } => Ok(*class),
                Object::Str(_) => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_STRING];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::Substring(_) => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_SUBSTRING];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::Bytes(_) => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_BYTES];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::StrBuilder(_) => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_STRING_BUILDER];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::ByteBuf(_) => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_BYTE_BUFFER];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::List { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_LIST];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::Map { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_MAP];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::Tuple { items } => {
                    let role = lm_bytecode::corepin::tuple_role(items.len()).ok_or(BAD_TYPE)?;
                    let class = module.core_roles[role];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeRegex(_) => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_REGEX];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeRegexMatch(_) => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_REGEX_MATCH];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeTcpStream { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_TCP_STREAM];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeTcpListener { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_TCP_LISTENER];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeTlsStream { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_TLS_STREAM];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeRawMode { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_RAW_MODE];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeSignalStream { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_SIGNAL_STREAM];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativePipeReader { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_PIPE_READER];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativePipeWriter { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_PIPE_WRITER];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeChild { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_CHILD];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeUdpSocket { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_UDP_SOCKET];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeCode(code) => {
                    let role = match code.kind {
                        lm_heap::PortableCodeKind::Artifact => lm_bytecode::corepin::ROLE_ARTIFACT,
                        lm_heap::PortableCodeKind::VerifiedModule => {
                            lm_bytecode::corepin::ROLE_VERIFIED_MODULE
                        }
                        lm_heap::PortableCodeKind::SlotSpec => lm_bytecode::corepin::ROLE_SLOT_SPEC,
                        lm_heap::PortableCodeKind::Function => {
                            lm_bytecode::corepin::ROLE_FUNCTION_CODE
                        }
                        lm_heap::PortableCodeKind::Class => lm_bytecode::corepin::ROLE_CLASS_CODE,
                    };
                    let class = module.core_roles[role];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeCodeHandle { kind, .. } => {
                    let role = match kind {
                        lm_heap::CodeHandleKind::Instance => lm_bytecode::corepin::ROLE_INSTANCE,
                        lm_heap::CodeHandleKind::Slot => lm_bytecode::corepin::ROLE_SLOT,
                        lm_heap::CodeHandleKind::Function => {
                            lm_bytecode::corepin::ROLE_FUNCTION_DEF
                        }
                        lm_heap::CodeHandleKind::Class => lm_bytecode::corepin::ROLE_CLASS_DEF,
                        lm_heap::CodeHandleKind::FunctionBinding => {
                            lm_bytecode::corepin::ROLE_FUNCTION_BINDING
                        }
                        lm_heap::CodeHandleKind::ClassBinding => {
                            lm_bytecode::corepin::ROLE_CLASS_BINDING
                        }
                    };
                    let class = module.core_roles[role];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::DynValue { .. } | Object::NativeDynRef { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_DYN_VALUE];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                object => Self::cold_native_class(module, object),
            },
            _ => Err(BAD_TYPE),
        }
    }

    /// Resolve native resource classes outside common value dispatch.
    #[cold]
    #[inline(never)]
    pub(super) fn cold_native_class(
        module: &NamespaceRuntime,
        object: &Object,
    ) -> Result<u32, FaultCode> {
        let role = match object {
            Object::NativeFileHandle { .. } => lm_bytecode::corepin::ROLE_FILE_HANDLE,
            _ => return Err(BAD_TYPE),
        };
        let class = module.core_roles[role];
        if class == lm_bytecode::NO_ROLE {
            Err(BAD_TYPE)
        } else {
            Ok(class)
        }
    }

    /// A machine without a loaded entry.
    #[cfg(test)]
    pub fn empty(config: VmConfig, parent: Option<VmId>) -> Machine {
        Machine::empty_at(config, parent, 0)
    }

    /// A machine without a loaded entry, at one slot generation.
    #[cfg(test)]
    pub fn empty_at(config: VmConfig, parent: Option<VmId>, generation: u32) -> Machine {
        Machine::empty_with_optional_resource_budget(config, parent, generation, None)
    }

    /// Create an empty machine with one shared resource ledger.
    pub(crate) fn empty_with_resource_budget(
        config: VmConfig,
        parent: Option<VmId>,
        generation: u32,
        resource_budget: ResourceBudget,
    ) -> Machine {
        Machine::empty_with_optional_resource_budget(
            config,
            parent,
            generation,
            Some(resource_budget),
        )
    }

    pub(super) fn empty_with_optional_resource_budget(
        config: VmConfig,
        parent: Option<VmId>,
        generation: u32,
        resource_budget: Option<ResourceBudget>,
    ) -> Machine {
        Machine {
            namespace: lm_link::NamespaceId::ROOT,
            vm: VmState {
                heap: Heap::new(config.heap_bytes),
                frames: Vec::new(),
                locals: Vec::new(),
                operands: Vec::new(),
                fuel: config.fuel,
                state: MachineState::Empty,
                pending: None,
                nested: None,
                routed: None,
                terminal: None,
                parent,
                next_ordinal: 1,
                next_wait: 1,
                waits: std::collections::BTreeMap::new(),
                // A machine that never becomes a proc keeps a closed
                // mailbox, so no send can reach it.
                mailbox: {
                    let mut mailbox = Mailbox::new(0);
                    mailbox.closed = true;
                    mailbox
                },
                block: None,
                literals: Vec::new(),
            },
            table: PolicyTable::default(),
            active: 0,
            driven: false,
            resources: match resource_budget {
                Some(budget) => ResourceRegistry::with_budget(config.max_resources, budget),
                None => ResourceRegistry::new(config.max_resources),
            },
            config,
            children: 0,
            owner: Ownership::Holder,
            generation,
            paused: false,
            barrier: None,
            body_func: None,
            witness: TypeEnvId::EMPTY,
            is_proc: false,
            dynamic_result: false,
            image: None,
            gate: 0,
            start_body: None,
            callbacks: Vec::new(),
            preparing_wait: None,
            execution_metrics: MachineExecutionMetrics::default(),
            native_continuation: None,
            native_return_depth: None,
            native_type_environments: lm_jit::NativeTypeEnvironmentCache::default(),
            native_resolved_calls: lm_jit::NativeResolvedCallCache::default(),
            pending_regex_compile: None,
        }
    }

    /// The current clock-free execution counters.
    pub fn execution_metrics(&self) -> MachineExecutionMetrics {
        self.execution_metrics
    }

    /// Install the initial frame for a function with its locals
    /// already evaluated. `closure` supplies capture context.
    ///
    /// The arena limit is checked before the slot allocation is sized
    /// from the code. The verifier bounds `local_count` for admitted
    /// modules; this check is the runtime backstop.
    pub fn load_frame(
        &mut self,
        module: &NamespaceRuntime,
        func: u32,
        args: Vec<Value>,
        closure: Option<ObjRef>,
        env: TypeEnvId,
    ) {
        let local_count = match module.funcs.get(func as usize) {
            Some(code) => code.local_count() as usize,
            None => {
                self.set_fault(BAD_STATE, "the frame names no function", None);
                return;
            }
        };
        if local_count > self.config.max_stack_values as usize {
            self.set_fault(
                FaultCode::StackLimit,
                "the initial frame exceeds the arena",
                None,
            );
            return;
        }
        self.body_func = Some(func);
        self.witness = env;
        self.vm.locals = args;
        // A slot past the parameters holds no value yet. The marker
        // states that fact, so a snapshot never spells an
        // uninitialized slot as a real unit value. The verifier proves
        // that no read reaches such a slot before its first store.
        self.vm.locals.resize(local_count, Value::Uninit);
        self.vm.operands.clear();
        self.vm.frames.push(Frame {
            func,
            block: 0,
            ip: 0,
            base_local: 0,
            base_operand: 0,
            closure: closure.map(FrameCapture::Closure),
            env,
        });
        self.vm.state = MachineState::Ready;
    }

    pub fn set_done(&mut self, value: Value) {
        if crate::jit::materialize_native_continuation(self).is_err() {
            self.set_fault(
                FaultCode::MalformedState,
                "the native machine state did not materialize",
                None,
            );
            return;
        }
        self.vm.terminal = Some(Terminal::Done(value));
        self.vm.state = MachineState::Done;
        self.vm.pending = None;
        self.preparing_wait = None;
        self.vm.nested = None;
        self.vm.routed = None;
        self.callbacks.clear();
        self.close_resources();
        self.compact_terminal_proc();
    }

    pub fn set_fault(&mut self, mut code: FaultCode, message: impl Into<String>, op: Option<u32>) {
        let mut message = message.into();
        if crate::jit::materialize_native_continuation(self).is_err() {
            code = FaultCode::MalformedState;
            message = "the native machine state did not materialize".to_string();
        }
        let trace = self.execution_trace_from(code == FaultCode::OutOfFuel);
        self.set_fault_record(FaultRec {
            code,
            message,
            op,
            trace,
        });
    }

    /// Stop this machine with one complete stored fault.
    pub fn set_fault_record(&mut self, mut fault: FaultRec) {
        if crate::jit::materialize_native_continuation(self).is_err() {
            fault = FaultRec {
                code: FaultCode::MalformedState,
                message: "the native machine state did not materialize".to_string(),
                op: None,
                trace: self.execution_trace(),
            };
        }
        self.vm.terminal = Some(Terminal::Fault(fault));
        self.vm.state = MachineState::Faulted;
        self.vm.pending = None;
        self.preparing_wait = None;
        self.vm.nested = None;
        self.vm.routed = None;
        self.callbacks.clear();
        self.close_resources();
        self.compact_terminal_proc();
    }

    /// Read one stored fault for an explicit re-raise.
    #[cold]
    #[inline(never)]
    pub(super) fn pop_fault_record(&mut self) -> Result<FaultRec, FaultCode> {
        let reference = self.pop_obj()?;
        match self.vm.heap.get(reference) {
            Object::NativeFault {
                code,
                message,
                op,
                trace,
            } => Ok(FaultRec {
                code: *code,
                message: message.clone(),
                op: *op,
                trace: trace.to_vec(),
            }),
            _ => Err(BAD_TYPE),
        }
    }

    /// Capture the current bounded execution trace.
    pub(crate) fn execution_trace(&self) -> Vec<FaultSite> {
        self.execution_trace_from(false)
    }

    /// Capture a trace from the current or next top instruction.
    pub(super) fn execution_trace_from(&self, next_top: bool) -> Vec<FaultSite> {
        if let Some(continuation) = &self.native_continuation {
            return continuation.execution_trace(next_top);
        }
        self.vm
            .frames
            .iter()
            .rev()
            .take(64)
            .enumerate()
            .map(|(depth, frame)| FaultSite {
                function: frame.func,
                block: frame.block,
                instruction: if next_top && depth == 0 {
                    frame.ip
                } else {
                    frame.ip.saturating_sub(1)
                },
            })
            .collect()
    }

    /// Remove state that a terminal proc cannot use again.
    pub(crate) fn compact_terminal_proc(&mut self) {
        if !self.is_proc || !matches!(self.vm.state, MachineState::Done | MachineState::Faulted) {
            return;
        }
        self.vm.frames = Vec::new();
        self.vm.locals = Vec::new();
        self.vm.operands = Vec::new();
        self.vm.literals = Vec::new();
        self.vm.pending = None;
        self.vm.nested = None;
        self.vm.routed = None;
        self.vm.block = None;
        self.vm.waits.clear();
        self.vm.mailbox.queue = std::collections::VecDeque::new();
        self.start_body = None;
        let retain_policy = self.children > 0;
        if !retain_policy {
            self.table.clear();
        }
        self.resources.compact_closed();
        self.collect_garbage(&[]);
        // A live child can route through this table. Heap collection
        // preserves its mocks, but heap compaction needs remapping.
        if retain_policy {
            return;
        }
        let root = match self.vm.terminal.as_ref() {
            Some(Terminal::Done(Value::Obj(reference))) => Some(*reference),
            _ => None,
        };
        match root {
            Some(mut reference) => {
                if self
                    .vm
                    .heap
                    .compact_live(std::slice::from_mut(&mut reference))
                    .is_ok()
                {
                    self.vm.terminal = Some(Terminal::Done(Value::Obj(reference)));
                }
            }
            None => {
                let _ = self.vm.heap.compact_live(&mut []);
            }
        }
    }

    /// Take the next request ordinal without wrapping it.
    pub fn take_request_ordinal(&mut self) -> Result<u64, FaultCode> {
        let ordinal = self.vm.next_ordinal;
        self.vm.next_ordinal = ordinal.checked_add(1).ok_or(FaultCode::IntegerOverflow)?;
        Ok(ordinal)
    }

    /// Close every scoped host resource this machine registered.
    ///
    /// Termination calls this. It invokes no guest callback, and it
    /// never replaces the stored terminal result, so a cleanup does
    /// not hide an existing machine fault.
    pub fn close_resources(&mut self) -> usize {
        self.resources.close_all()
    }
}
