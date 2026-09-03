//! Interpreter dispatch and numeric instruction execution.

use super::*;

impl Machine {
    /// Execute one added instruction outside the base dispatch body.
    #[inline(never)]
    pub(super) fn exec_extended(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        slots: Option<&[ImageSlotTarget]>,
        instr: ExtendedInstr,
    ) -> Result<ExecOutcome, FaultCode> {
        match instr {
            ExtendedInstr::PrepareWait { op_argc, reply_ty } => {
                let (op, argc) = ExtendedInstr::wait_parts(op_argc);
                return self.exec_prepare_wait(op, argc, reply_ty);
            }
            ExtendedInstr::MakeCallback { func, captures } => {
                self.exec_make_callback(func, captures)?;
            }
            ExtendedInstr::AsCallback => self.exec_as_callback()?,
            ExtendedInstr::OptionSome { .. } => {
                // The payload already has the native `Some` representation.
            }
            ExtendedInstr::OptionNone { ty } => {
                self.exec_option_collection(module, envs, OptionCollectionOp::OptionNone(ty))?;
            }
            ExtendedInstr::OptionPayload { ty } => {
                self.exec_option_collection(module, envs, OptionCollectionOp::OptionPayload(ty))?;
            }
            ExtendedInstr::ListGet { ty } => {
                self.exec_option_collection(module, envs, OptionCollectionOp::ListGet(ty))?;
            }
            ExtendedInstr::MapGet { ty } => {
                self.exec_option_collection(module, envs, OptionCollectionOp::MapGet(ty))?;
            }
            ExtendedInstr::MapPutText { ty, discard } => {
                self.exec_map_put_text(module, envs, ty, discard)?;
            }
            ExtendedInstr::MapInternTextRange => {
                self.exec_map_intern_text_range()?;
            }
            ExtendedInstr::RegexCaptures { ty } => {
                self.exec_regex_captures(module, envs, ty)?;
            }
            ExtendedInstr::RegexMatchGroup { ty } => {
                self.exec_regex_match_group(module, envs, ty)?;
            }
            ExtendedInstr::RegexMatchNamed { ty } => {
                self.exec_regex_match_named(module, envs, ty)?;
            }
            ExtendedInstr::ListEpoch => {
                self.exec_collection_iteration(CollectionIterationOp::ListEpoch)?;
            }
            ExtendedInstr::ListIterLen => {
                self.exec_collection_iteration(CollectionIterationOp::ListIterLen)?;
            }
            ExtendedInstr::MapEpoch => {
                self.exec_collection_iteration(CollectionIterationOp::MapEpoch)?;
            }
            ExtendedInstr::MapIterLen => {
                self.exec_collection_iteration(CollectionIterationOp::MapIterLen)?;
            }
            ExtendedInstr::MapNextIndex => {
                self.exec_collection_iteration(CollectionIterationOp::MapNextIndex)?;
            }
            ExtendedInstr::SealInstance => {
                let reference = self.pop_obj()?;
                let class = match self.vm.heap.get(reference) {
                    Object::Instance { class, .. } => *class,
                    _ => return Err(BAD_TYPE),
                };
                if !module
                    .classes
                    .get(class as usize)
                    .is_some_and(|definition| definition.is_frozen)
                {
                    return Err(BAD_STATE);
                }
                self.vm.heap.set_frozen(reference);
                self.push(Value::Obj(reference))?;
            }
            ExtendedInstr::MapKeyAt => {
                self.exec_collection_iteration(CollectionIterationOp::MapEntry { value: false })?;
            }
            ExtendedInstr::MapValueAt => {
                self.exec_collection_iteration(CollectionIterationOp::MapEntry { value: true })?;
            }
            ExtendedInstr::ListCapacity => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListCapacity)?;
            }
            ExtendedInstr::ListSet => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListSet)?;
            }
            ExtendedInstr::ListSwap => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListSwap)?;
            }
            ExtendedInstr::ListPop { ty } => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListPop(ty))?;
            }
            ExtendedInstr::ListInsert => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListInsert)?;
            }
            ExtendedInstr::ListRemove => {
                self.exec_collection_extension(
                    module,
                    envs,
                    CollectionExtensionOp::ListRemove { swap: false },
                )?;
            }
            ExtendedInstr::ListSwapRemove => {
                self.exec_collection_extension(
                    module,
                    envs,
                    CollectionExtensionOp::ListRemove { swap: true },
                )?;
            }
            ExtendedInstr::ListReserve => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListReserve)?;
            }
            ExtendedInstr::ListTruncate => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListTruncate)?;
            }
            ExtendedInstr::ListContains => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListContains)?;
            }
            ExtendedInstr::ListReorder => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListReorder)?;
            }
            ExtendedInstr::MapRemove { ty } => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::MapRemove(ty))?;
            }
            ExtendedInstr::MapClear => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::MapClear)?;
            }
            ExtendedInstr::MapReserve => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::MapReserve)?;
            }
            ExtendedInstr::MapProbe
            | ExtendedInstr::MapProbeFound
            | ExtendedInstr::MapProbeKey
            | ExtendedInstr::MapProbeValue
            | ExtendedInstr::MapProbeSetValue
            | ExtendedInstr::MapProbeRemove
            | ExtendedInstr::MapInsertHashed
            | ExtendedInstr::MapWriteGuard => {
                self.exec_hashable_map_instr(instr)?;
            }
            ExtendedInstr::CallSlot { slot, app } => {
                let target = match slots.and_then(|slots| slots.get(slot as usize)) {
                    Some(ImageSlotTarget::Function(target)) => *target,
                    Some(ImageSlotTarget::Empty) => return Err(FaultCode::InvalidVmState),
                    _ => return Err(BAD_STATE),
                };
                if app == lm_bytecode::NO_APP {
                    let argc = module
                        .funcs
                        .get(target as usize)
                        .ok_or(BAD_STATE)?
                        .params
                        .len();
                    self.push_frame(module, target, argc, None, TypeEnvId::EMPTY)?;
                } else {
                    self.call_generic(module, envs, target, app)?;
                }
            }
            ExtendedInstr::NewSlot { slot, app } => {
                let constructor = match slots.and_then(|slots| slots.get(slot as usize)) {
                    Some(ImageSlotTarget::Class { constructor, .. }) => *constructor,
                    Some(ImageSlotTarget::Empty) => return Err(FaultCode::InvalidVmState),
                    _ => return Err(BAD_STATE),
                };
                if app == lm_bytecode::NO_APP {
                    let argc = module
                        .funcs
                        .get(constructor as usize)
                        .ok_or(BAD_STATE)?
                        .params
                        .len();
                    self.push_frame(module, constructor, argc, None, TypeEnvId::EMPTY)?;
                } else {
                    self.call_generic(module, envs, constructor, app)?;
                }
            }
            ExtendedInstr::LoadSlot { slot } => {
                match slots.and_then(|slots| slots.get(slot as usize)) {
                    Some(ImageSlotTarget::Value(_)) => return Ok(ExecOutcome::LoadSlot { slot }),
                    Some(ImageSlotTarget::Empty) => return Err(FaultCode::InvalidVmState),
                    _ => return Err(BAD_STATE),
                }
            }
            ExtendedInstr::SendSlot { slot } => {
                let (proc, generation) = match slots.and_then(|slots| slots.get(slot as usize)) {
                    Some(ImageSlotTarget::Process { proc, generation }) => (*proc, *generation),
                    Some(ImageSlotTarget::Empty) => return Err(FaultCode::InvalidVmState),
                    _ => return Err(BAD_STATE),
                };
                let message = self.pop()?;
                let handle = self.alloc(Object::NativeHandle { proc, generation })?;
                return Ok(ExecOutcome::Perform {
                    op: lm_abi::OP_PROC_SEND,
                    args: vec![handle, message],
                });
            }
            ExtendedInstr::SyntaxTreeRoot
            | ExtendedInstr::SyntaxKind
            | ExtendedInstr::SyntaxCategory
            | ExtendedInstr::SyntaxRangeStart
            | ExtendedInstr::SyntaxRangeEnd
            | ExtendedInstr::SyntaxText
            | ExtendedInstr::SyntaxChildren
            | ExtendedInstr::SyntaxDetach
            | ExtendedInstr::SyntaxBuildToken
            | ExtendedInstr::SyntaxBuildTrivia
            | ExtendedInstr::SyntaxBuildNode
            | ExtendedInstr::SyntaxToTree => {
                let tree = module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_TREE];
                let node = module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_NODE];
                let token = module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_TOKEN];
                let trivia = module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_TRIVIA];
                let builder = module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_BUILDER];
                self.exec_syntax(instr, tree, node, token, trivia, builder)?;
            }
            ExtendedInstr::DynPack { ty } => {
                let closed = envs
                    .close(module, ty, self.frame_env())
                    .map_err(env_fault)?;
                let value = self.pop()?;
                let package = self.alloc(Object::DynValue { value, ty: closed })?;
                self.push(package)?;
            }
            ExtendedInstr::DynRender => {
                let package = self.pop_obj()?;
                return match self.vm.heap.get(package) {
                    Object::DynValue { value, ty } => Ok(ExecOutcome::DynamicRender {
                        value: *value,
                        ty: *ty,
                    }),
                    Object::NativeDynRef { vm, generation } => Ok(ExecOutcome::DynamicRenderRef {
                        vm: *vm,
                        generation: *generation,
                    }),
                    _ => Err(BAD_TYPE),
                };
            }
            ExtendedInstr::FunctionCode { func } => {
                return Ok(ExecOutcome::FunctionCode {
                    function: func,
                    origin: self.current_code_origin(module),
                });
            }
            ExtendedInstr::ClassCode { class } => {
                return Ok(ExecOutcome::ClassCode {
                    class,
                    origin: self.current_code_origin(module),
                });
            }
            ExtendedInstr::CodeSource { ty } => {
                self.exec_code_source(module, envs, ty)?;
            }
            ExtendedInstr::CodeDefinition => {
                self.exec_code_definition(module)?;
            }
            ExtendedInstr::FaultSite { ty } => {
                self.exec_fault_locations(module, envs, ty, true)?;
            }
            ExtendedInstr::FaultTrace { ty } => {
                self.exec_fault_locations(module, envs, ty, false)?;
            }
        }
        Ok(ExecOutcome::Continue)
    }

    #[inline(never)]
    pub(super) fn exec_prepare_wait(
        &mut self,
        op: u32,
        argc: u32,
        reply_ty: u32,
    ) -> Result<ExecOutcome, FaultCode> {
        if self.vm.operands.len() < argc as usize {
            return Err(BAD_STATE);
        }
        Ok(ExecOutcome::PrepareWait {
            op,
            argc,
            reply_ty,
            env: self.frame_env(),
        })
    }

    /// Remove one verified instruction argument list from the stack.
    pub(crate) fn take_arguments(&mut self, argc: u32) -> Result<Vec<Value>, FaultCode> {
        let split = self
            .vm
            .operands
            .len()
            .checked_sub(argc as usize)
            .ok_or(BAD_STATE)?;
        Ok(self.vm.operands.split_off(split))
    }

    /// Execute one fetched instruction of the current frame.
    ///
    /// `envs` is the type environment table of the world. A
    /// monomorphic instruction never reads it, so a monomorphic
    /// program performs no type work.
    #[inline(always)]
    pub(super) fn exec_instr(
        &mut self,
        module: &NamespaceRuntime,
        dispatch: &lm_bytecode::CodeTable<crate::DispatchRow>,
        envs: &mut TypeEnvs,
        slots: Option<&[ImageSlotTarget]>,
        instr: Instr,
        native: &mut InterpreterNative<'_>,
    ) -> Result<ExecOutcome, FaultCode> {
        if matches!(instr, Instr::Native(_)) {
            let result = self.exec_native_instr(instr);
            self.execution_metrics.native_calls =
                self.execution_metrics.native_calls.saturating_add(1);
            result?;
            return Ok(ExecOutcome::Continue);
        }
        match instr {
            Instr::ConstUnit => self.push(Value::Unit)?,
            Instr::ConstBool(v) => self.push(Value::Bool(v))?,
            Instr::ConstInt(v) => self.push(Value::Int(v))?,
            Instr::ConstFloat(bits) => self.push(Value::Float(canonical_float_bits(bits)))?,
            Instr::ConstChar(value) => {
                self.push(Value::Char(char::from_u32(value).ok_or(BAD_STATE)?))?;
            }
            Instr::ConstStr(idx) => {
                // Literal strings intern per machine: the first load
                // allocates one frozen object, and every later load
                // reuses it. Literals are collection roots.
                let idx = idx as usize;
                if self.vm.literals.len() <= idx {
                    self.vm.literals.resize(idx + 1, Value::Uninit);
                }
                let value = match self.vm.literals[idx] {
                    Value::Obj(reference) => Value::Obj(reference),
                    Value::Uninit => {
                        let text = module.strings[idx].clone();
                        let value = self.alloc(Object::Str(text.into()))?;
                        self.vm.literals[idx] = value;
                        value
                    }
                    _ => return Err(BAD_STATE),
                };
                self.push(value)?;
            }
            Instr::ConstBytes(idx) => {
                let cache = module.strings.len() + idx as usize;
                if self.vm.literals.len() <= cache {
                    self.vm.literals.resize(cache + 1, Value::Uninit);
                }
                let value = match self.vm.literals[cache] {
                    Value::Obj(reference) => Value::Obj(reference),
                    Value::Uninit => {
                        let bytes = module.bytes.get(idx as usize).ok_or(BAD_STATE)?;
                        let bytes =
                            SharedBytes::try_from_slice(bytes).map_err(|_| FaultCode::HeapLimit)?;
                        let value = self.alloc(Object::Bytes(bytes))?;
                        self.vm.literals[cache] = value;
                        value
                    }
                    _ => return Err(BAD_STATE),
                };
                self.push(value)?;
            }
            Instr::ConstRegex(idx) => {
                let source = idx as usize;
                let cache = module
                    .strings
                    .len()
                    .checked_add(module.bytes.len())
                    .and_then(|base| base.checked_add(source))
                    .ok_or(BAD_STATE)?;
                if self.vm.literals.len() <= cache {
                    self.vm.literals.resize(cache + 1, Value::Uninit);
                }
                let value = match self.vm.literals[cache] {
                    Value::Obj(reference) => Value::Obj(reference),
                    Value::Uninit => {
                        let regex = module
                            .regex_literals
                            .get(source)
                            .and_then(Option::as_ref)
                            .ok_or(BAD_STATE)?
                            .clone();
                        let value = self.alloc(Object::NativeRegex(std::sync::Arc::new(regex)))?;
                        self.vm.literals[cache] = value;
                        value
                    }
                    _ => return Err(BAD_STATE),
                };
                self.push(value)?;
            }
            Instr::Numeric(instruction) => self.exec_numeric_instr(instruction)?,
            Instr::LoadLocal(slot) => {
                let at = self.local_at(slot)?;
                let value = *self.vm.locals.get(at).ok_or(BAD_STATE)?;
                self.push(value)?;
            }
            Instr::StoreLocal(slot) => {
                let value = self.pop()?;
                let at = self.local_at(slot)?;
                *self.vm.locals.get_mut(at).ok_or(BAD_STATE)? = value;
            }
            Instr::Pop => {
                self.pop()?;
            }
            Instr::Add => self.int_binary(i64::checked_add)?,
            Instr::Sub => self.int_binary(i64::checked_sub)?,
            Instr::Mul => self.int_binary(i64::checked_mul)?,
            Instr::Div => {
                let (at, a, b) = self.int_pair()?;
                if b == 0 {
                    self.vm.operands.truncate(at);
                    return Err(FaultCode::DivideByZero);
                }
                if a == i64::MIN && b == -1 {
                    self.vm.operands.truncate(at);
                    return Err(FaultCode::IntegerOverflow);
                }
                self.replace_pair(at, Value::Int(a / b));
            }
            Instr::Rem => {
                let (at, a, b) = self.int_pair()?;
                if b == 0 {
                    self.vm.operands.truncate(at);
                    return Err(FaultCode::DivideByZero);
                }
                if a == i64::MIN && b == -1 {
                    self.vm.operands.truncate(at);
                    return Err(FaultCode::IntegerOverflow);
                }
                self.replace_pair(at, Value::Int(a % b));
            }
            Instr::Neg => {
                let a = self.pop_int()?;
                let value = a.checked_neg().ok_or(FaultCode::IntegerOverflow)?;
                self.push(Value::Int(value))?;
            }
            Instr::Not => {
                let a = self.pop_bool()?;
                self.push(Value::Bool(!a))?;
            }
            Instr::LtInt => self.int_compare(|a, b| a < b)?,
            Instr::LeInt => self.int_compare(|a, b| a <= b)?,
            Instr::GtInt => self.int_compare(|a, b| a > b)?,
            Instr::GeInt => self.int_compare(|a, b| a >= b)?,
            Instr::EqInt => self.int_compare(|a, b| a == b)?,
            Instr::NeInt => self.int_compare(|a, b| a != b)?,
            Instr::EqBool => {
                let b = self.pop_bool()?;
                let a = self.pop_bool()?;
                self.push(Value::Bool(a == b))?;
            }
            Instr::NeBool => {
                let b = self.pop_bool()?;
                let a = self.pop_bool()?;
                self.push(Value::Bool(a != b))?;
            }
            Instr::Jump(target) => {
                let (function, block) = self
                    .vm
                    .frames
                    .last()
                    .map(|frame| (frame.func, frame.block))
                    .ok_or(BAD_STATE)?;
                let frame = self.vm.frames.last_mut().ok_or(BAD_STATE)?;
                frame.block = target;
                frame.ip = 0;
                if target <= block && native.after_backedge(function) {
                    return Ok(ExecOutcome::ContinueNative);
                }
            }
            Instr::JumpIfFalse(target) => {
                if !self.pop_bool()? {
                    let (function, block) = self
                        .vm
                        .frames
                        .last()
                        .map(|frame| (frame.func, frame.block))
                        .ok_or(BAD_STATE)?;
                    let frame = self.vm.frames.last_mut().ok_or(BAD_STATE)?;
                    frame.block = target;
                    frame.ip = 0;
                    if target <= block && native.after_backedge(function) {
                        return Ok(ExecOutcome::ContinueNative);
                    }
                }
            }
            Instr::JumpIfTrue(target) => {
                if self.pop_bool()? {
                    let (function, block) = self
                        .vm
                        .frames
                        .last()
                        .map(|frame| (frame.func, frame.block))
                        .ok_or(BAD_STATE)?;
                    let frame = self.vm.frames.last_mut().ok_or(BAD_STATE)?;
                    frame.block = target;
                    frame.ip = 0;
                    if target <= block && native.after_backedge(function) {
                        return Ok(ExecOutcome::ContinueNative);
                    }
                }
            }
            Instr::Native(_) => unreachable!("native instructions return before dispatch"),
            Instr::EqValue | Instr::NeValue => {
                let b = self.pop()?;
                let a = self.pop()?;
                let equal = self.values_equal(module, a, b)?;
                let want = matches!(instr, Instr::EqValue);
                self.push(Value::Bool(equal == want))?;
            }
            Instr::CallInterface { site, recv_ty, app } => {
                let (interface, method) = lm_bytecode::unpack_interface_call_site(site);
                self.call_interface(
                    module,
                    dispatch,
                    envs,
                    InterfaceCallSite {
                        interface,
                        method,
                        recv_ty,
                        app,
                    },
                )?;
            }
            Instr::Extended(instr) => {
                let outcome = self.exec_extended(module, envs, slots, instr)?;
                if !matches!(outcome, ExecOutcome::Continue) {
                    return Ok(outcome);
                }
            }
            Instr::EqRef => {
                let b = self.pop_obj()?;
                let a = self.pop_obj()?;
                self.push(Value::Bool(self.references_equal(module, a, b)))?;
            }
            Instr::NeRef => {
                let b = self.pop_obj()?;
                let a = self.pop_obj()?;
                self.push(Value::Bool(!self.references_equal(module, a, b)))?;
            }
            // A direct call of a non-generic function copies the empty
            // environment, so it allocates nothing and reads no table.
            Instr::Call(callee) => {
                let argc = module.funcs[callee as usize].params.len();
                self.push_frame(module, callee, argc, None, TypeEnvId::EMPTY)?;
                if native.after_call(callee) {
                    return Ok(ExecOutcome::ContinueNative);
                }
            }
            // A generic call derives one environment from the caller
            // environment and the application of the call site. The
            // table caches the pair, so a repeated call reuses one
            // index.
            Instr::CallG { func: callee, app } => {
                self.call_generic(module, envs, callee, app)?;
                if native.after_call(callee) {
                    return Ok(ExecOutcome::ContinueNative);
                }
            }
            Instr::CallVirtual { selector, argc } => {
                let argc = argc as usize;
                let recv = self.peek(argc)?;
                let class = self.virtual_class(module, recv)?;
                let target = method_of(dispatch, class, selector)?;
                self.push_frame(module, target, argc + 1, None, TypeEnvId::EMPTY)?;
            }
            // A generic virtual call binds the receiver class
            // arguments first and the own arguments of the method
            // after them. The receiver object carries its class
            // arguments, so the runtime reads them from the value it
            // dispatched on.
            Instr::CallVirtualG {
                selector,
                argc,
                app,
            } => {
                self.call_virtual_generic(module, dispatch, envs, selector, argc, app)?;
            }
            // A closure call installs the environment the creator
            // frame held. The call site applies no type argument, so
            // the closure value is the only evidence.
            Instr::CallValue { argc } => {
                let argc = argc as usize;
                let callee_pos = self
                    .vm
                    .operands
                    .len()
                    .checked_sub(argc + 1)
                    .ok_or(BAD_STATE)?;
                let callee = self.vm.operands.remove(callee_pos);
                let (target, env, capture) = match callee {
                    Value::Obj(reference) => match self.vm.heap.get(reference) {
                        Object::Closure { func, env, .. } => {
                            (*func, env.env(), FrameCapture::Closure(reference))
                        }
                        _ => return Err(BAD_TYPE),
                    },
                    Value::Callback(reference) => {
                        let descriptor = self.callback(reference)?;
                        (
                            descriptor.func,
                            descriptor.env,
                            FrameCapture::Callback(reference),
                        )
                    }
                    _ => return Err(BAD_TYPE),
                };
                self.push_frame(module, target, argc, Some(capture), env)?;
                if native.after_call(target) {
                    return Ok(ExecOutcome::ContinueNative);
                }
            }
            // The closure retains the environment of the frame that
            // built it. Capture cannot rebuild it later, because the
            // closure outlives that frame.
            Instr::MakeClosure { func, captures } => {
                let split = self
                    .vm
                    .operands
                    .len()
                    .checked_sub(captures as usize)
                    .ok_or(BAD_STATE)?;
                let captured: Vec<Value> = self.vm.operands.split_off(split);
                let env = Witness(self.frame_env());
                let value = self.alloc(Object::Closure {
                    func,
                    captures: captured.into(),
                    env,
                })?;
                self.push(value)?;
            }
            Instr::LoadCapture(idx) => {
                let frame = self.vm.frames.last().ok_or(BAD_STATE)?;
                let closure = frame.closure.ok_or(BAD_STATE)?;
                let value = match closure {
                    FrameCapture::Closure(reference) => match self.vm.heap.get(reference) {
                        Object::Closure { captures, .. } => {
                            *captures.get(idx as usize).ok_or(BAD_TYPE)?
                        }
                        _ => return Err(BAD_TYPE),
                    },
                    FrameCapture::Callback(reference) => *self
                        .callback(reference)?
                        .captures
                        .get(idx as usize)
                        .ok_or(BAD_TYPE)?,
                };
                self.push(value)?;
            }
            // A plain class takes no type argument, so the instance
            // records the empty environment and allocates nothing.
            Instr::New(class) => {
                let field_count = module.classes[class as usize].fields.len();
                let value = self.alloc(Object::Instance {
                    class,
                    fields: vec![Value::Uninit; field_count].into(),
                    env: Witness::EMPTY,
                })?;
                self.push(value)?;
            }
            // A generic instance records its own class arguments, so a
            // later dispatch and a later reflection query read them
            // from the object itself.
            Instr::NewG { class, app } => {
                let value = self.new_generic(module, envs, class, app)?;
                self.push(value)?;
            }
            Instr::TupleNew { count, .. } => {
                let split = self
                    .vm
                    .operands
                    .len()
                    .checked_sub(count as usize)
                    .ok_or(BAD_STATE)?;
                let items: Vec<Value> = self.vm.operands.split_off(split);
                let value = self.alloc(Object::Tuple {
                    items: items.into(),
                })?;
                self.push(value)?;
            }
            Instr::TupleGet(index) => {
                let r = self.pop_obj()?;
                let value = match self.vm.heap.get(r) {
                    Object::Tuple { items } => *items.get(index as usize).ok_or(BAD_TYPE)?,
                    _ => return Err(BAD_TYPE),
                };
                self.push(value)?;
            }
            Instr::IsType(ty) => {
                let value = self.pop()?;
                let matches = self.value_matches_class(module, envs, value, ty)?;
                self.push(Value::Bool(matches))?;
            }
            Instr::CastType(ty) => {
                let value = self.pop()?;
                if !self.value_matches_class(module, envs, value, ty)? {
                    return Err(FaultCode::BadCast);
                }
                self.push(value)?;
            }
            Instr::LoadField(field) => {
                let r = self.pop_obj()?;
                let value = match self.vm.heap.get(r) {
                    Object::Instance { fields, .. } => {
                        *fields.get(field as usize).ok_or(BAD_TYPE)?
                    }
                    _ => return Err(BAD_TYPE),
                };
                if value == Value::Uninit {
                    return Err(FaultCode::UninitializedField);
                }
                self.push(value)?;
            }
            Instr::StoreField(field) => {
                let value = self.pop()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                match self.vm.heap.get_mut(r) {
                    Object::Instance { fields, .. } => {
                        *fields.get_mut(field as usize).ok_or(BAD_TYPE)? = value;
                    }
                    _ => return Err(BAD_TYPE),
                }
            }
            Instr::ListNew { count, .. } => {
                let split = self
                    .vm
                    .operands
                    .len()
                    .checked_sub(count as usize)
                    .ok_or(BAD_STATE)?;
                let items: Vec<Value> = self.vm.operands.split_off(split);
                let value = self.alloc(Object::List {
                    items: items.into(),
                    epoch: StructuralEpoch::default(),
                })?;
                self.push(value)?;
            }
            Instr::ListLen => {
                let r = self.pop_obj()?;
                let len = match self.vm.heap.get(r) {
                    Object::List { items, .. } => items.len(),
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Int(len as i64))?;
            }
            Instr::ListAt => {
                self.exec_list_at()?;
            }
            Instr::ListPush => {
                let value = self.pop()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                self.reserve(16, &[Value::Obj(r), value])?;
                match self.vm.heap.get_mut(r) {
                    Object::List { items, epoch } => {
                        epoch.bump()?;
                        items.push(value);
                    }
                    _ => return Err(BAD_TYPE),
                }
                self.vm.heap.recharge_local(r);
                self.push(Value::Unit)?;
            }
            Instr::MapNew { count, .. } => {
                let split = self
                    .vm
                    .operands
                    .len()
                    .checked_sub(2 * count as usize)
                    .ok_or(BAD_STATE)?;
                let flat: Vec<Value> = self.vm.operands.split_off(split);
                let mut entries: Vec<MapEntry> = Vec::new();
                let mut index = MapIndex::default();
                for pair in flat.chunks_exact(2) {
                    let (key, value) = (pair[0], pair[1]);
                    let semantic = self.key_semantic_hash(key)?;
                    let hash = Self::map_index_hash(semantic);
                    let hit = index
                        .candidates(hash)
                        .find(|i| self.key_eq(entries[*i as usize].key, key));
                    match hit {
                        Some(pos) => entries[pos as usize].value = value,
                        None => {
                            index.push_live(hash, entries.len() as u32);
                            entries.push(MapEntry {
                                key,
                                value,
                                semantic_hash: semantic,
                            });
                        }
                    }
                }
                let value = self.alloc(Object::Map {
                    entries: entries.into(),
                    index,
                })?;
                self.push(value)?;
            }
            Instr::MapLen => {
                let r = self.pop_obj()?;
                let len = match self.vm.heap.get(r) {
                    Object::Map { index, .. } => index.live_len(),
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Int(len as i64))?;
            }
            Instr::MapHas => {
                let key = self.pop()?;
                let r = self.pop_obj()?;
                let found = self.map_lookup(r, key)?.is_some();
                self.push(Value::Bool(found))?;
            }
            Instr::MapAt => {
                let key = self.pop()?;
                let r = self.pop_obj()?;
                let pos = match self.map_lookup(r, key)? {
                    Some(pos) => pos,
                    None => return Err(FaultCode::MissingKey),
                };
                let value = match self.vm.heap.get(r) {
                    Object::Map { entries, .. } => entries.get(pos).ok_or(BAD_STATE)?.value,
                    _ => return Err(BAD_TYPE),
                };
                self.push(value)?;
            }
            Instr::MapPut { ty, discard } => {
                self.exec_map_put(module, envs, ty, discard)?;
            }
            Instr::Freeze => {
                let r = self.pop_obj()?;
                // The freeze mode validates the whole reachable graph
                // against its limits before any bit goes on, so a
                // rejected freeze changes nothing.
                lm_graph::freeze(&mut self.vm.heap, r, &self.config.graph)?;
                self.push(Value::Obj(r))?;
            }
            Instr::Digest { ty } => {
                let env = self.frame_env();
                let value = self.pop_obj()?;
                return Ok(ExecOutcome::Digest { value, ty, env });
            }
            Instr::EqDigest | Instr::NeDigest => {
                let b = self.pop_obj()?;
                let a = self.pop_obj()?;
                // A digest compares by value, never by reference
                // (specification 6.4).
                let equal = match (self.vm.heap.get(a), self.vm.heap.get(b)) {
                    (Object::NativeDigest(x), Object::NativeDigest(y)) => x == y,
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Bool(equal == matches!(instr, Instr::EqDigest)))?;
            }
            Instr::Return => {
                let value = self.pop()?;
                let frame = self.vm.frames.pop().ok_or(BAD_STATE)?;
                self.vm.operands.truncate(frame.base_operand as usize);
                self.vm.locals.truncate(frame.base_local as usize);
                if self.vm.frames.is_empty() {
                    if !self.callbacks.is_empty() {
                        self.collect_callbacks();
                    }
                    return Ok(ExecOutcome::Terminal(value));
                }
                self.push(value)?;
                if !self.callbacks.is_empty() {
                    self.collect_callbacks();
                }
                if native.after_return(self.vm.frames.len()) {
                    return Ok(ExecOutcome::ContinueNative);
                }
            }
            Instr::Unreachable => {
                return Err(FaultCode::UnreachableCode);
            }
            Instr::Perform { op, argc, .. } => {
                let split = self
                    .vm
                    .operands
                    .len()
                    .checked_sub(argc as usize)
                    .ok_or(BAD_STATE)?;
                let args = self.vm.operands.split_off(split);
                return Ok(ExecOutcome::Perform { op, args });
            }
            Instr::PerformValue { argc, .. } => {
                let split = self
                    .vm
                    .operands
                    .len()
                    .checked_sub(argc as usize)
                    .ok_or(BAD_STATE)?;
                let args = self.vm.operands.split_off(split);
                let callee = self.pop()?;
                let op = match callee {
                    Value::Op(op) => op,
                    _ => return Err(BAD_TYPE),
                };
                return Ok(ExecOutcome::Perform { op, args });
            }
            Instr::OpConst(op) => {
                self.push(Value::Op(op))?;
            }
            Instr::TableEdit { action, kind, slot } => {
                let mock = if action == 2 { Some(self.pop()?) } else { None };
                let table = self.pop_obj()?;
                return Ok(ExecOutcome::TableEdit {
                    table,
                    action,
                    kind,
                    slot,
                    mock,
                });
            }
            Instr::AsCall { op, ty } => {
                let request = self.pop_obj()?;
                return Ok(ExecOutcome::AsCall {
                    request,
                    op,
                    ty,
                    env: self.frame_env(),
                });
            }
            Instr::CallArgs => {
                let call = self.pop_obj()?;
                return Ok(ExecOutcome::CallArgs { call });
            }
            Instr::FaultCode => {
                let r = self.pop_obj()?;
                let code = match self.vm.heap.get(r) {
                    Object::NativeFault { code, .. } => *code,
                    _ => return Err(BAD_TYPE),
                };
                let value = self.alloc(Object::Str(code.to_string().into()))?;
                self.push(value)?;
            }
            Instr::RequestOp => {
                let request = self.pop_obj()?;
                return Ok(ExecOutcome::RequestOp { request });
            }
            Instr::FaultDenied => {
                let r = self.pop_obj()?;
                let reason = match self.vm.heap.get(r) {
                    Object::Str(text) => text.clone(),
                    _ => return Err(BAD_TYPE),
                };
                // The code is fixed. A holder states why it denied
                // the request, and it cannot name another code.
                let value = self.alloc(Object::NativeFault {
                    code: FaultCode::PolicyDenied,
                    message: reason.to_string(),
                    op: None,
                    trace: Box::default(),
                })?;
                self.push(value)?;
            }
            Instr::RaiseUserPanic | Instr::RaiseAssertionFailed => {
                let reference = self.pop_obj()?;
                let message = match self.vm.heap.get(reference) {
                    Object::Str(text) => text.to_string(),
                    _ => return Err(BAD_TYPE),
                };
                let code = if matches!(instr, Instr::RaiseUserPanic) {
                    FaultCode::UserPanic
                } else {
                    FaultCode::AssertionFailed
                };
                return Ok(ExecOutcome::Raise { code, message });
            }
            Instr::RaiseFault => {
                return Ok(ExecOutcome::Reraise(self.pop_fault_record()?));
            }
        }
        Ok(ExecOutcome::Continue)
    }

    /// Execute one numeric or bitwise instruction.
    #[inline(never)]
    pub(super) fn exec_numeric_instr(&mut self, instr: NumericInstr) -> Result<(), FaultCode> {
        match instr {
            NumericInstr::IntBitAnd
            | NumericInstr::IntBitOr
            | NumericInstr::IntBitXor
            | NumericInstr::IntShl
            | NumericInstr::IntShr
            | NumericInstr::IntUshr
            | NumericInstr::IntWrappingAdd
            | NumericInstr::IntWrappingSub
            | NumericInstr::IntWrappingMul
            | NumericInstr::IntRotateLeft
            | NumericInstr::IntRotateRight => {
                let right = self.pop_int()?;
                let left = self.pop_int()?;
                let result = match instr {
                    NumericInstr::IntBitAnd => left & right,
                    NumericInstr::IntBitOr => left | right,
                    NumericInstr::IntBitXor => left ^ right,
                    NumericInstr::IntShl => {
                        let amount = shift_amount(right)?;
                        ((left as u64) << amount) as i64
                    }
                    NumericInstr::IntShr => {
                        let amount = shift_amount(right)?;
                        left >> amount
                    }
                    NumericInstr::IntUshr => {
                        let amount = shift_amount(right)?;
                        ((left as u64) >> amount) as i64
                    }
                    NumericInstr::IntWrappingAdd => left.wrapping_add(right),
                    NumericInstr::IntWrappingSub => left.wrapping_sub(right),
                    NumericInstr::IntWrappingMul => left.wrapping_mul(right),
                    NumericInstr::IntRotateLeft => {
                        (left as u64).rotate_left(shift_amount(right)?) as i64
                    }
                    NumericInstr::IntRotateRight => {
                        (left as u64).rotate_right(shift_amount(right)?) as i64
                    }
                    _ => unreachable!(),
                };
                self.push(Value::Int(result))?;
            }
            NumericInstr::IntBitNot => {
                let value = self.pop_int()?;
                self.push(Value::Int(!value))?;
            }
            NumericInstr::IntCountOnes
            | NumericInstr::IntLeadingZeros
            | NumericInstr::IntTrailingZeros
            | NumericInstr::IntSignum => {
                let value = self.pop_int()?;
                let result = match instr {
                    NumericInstr::IntCountOnes => value.count_ones() as i64,
                    NumericInstr::IntLeadingZeros => value.leading_zeros() as i64,
                    NumericInstr::IntTrailingZeros => value.trailing_zeros() as i64,
                    NumericInstr::IntSignum => value.signum(),
                    _ => unreachable!(),
                };
                self.push(Value::Int(result))?;
            }
            NumericInstr::IntToFloat => {
                let value = self.pop_int()?;
                self.push(Value::Float((value as f64).to_bits()))?;
            }
            NumericInstr::FloatNeg => {
                let value = self.pop_float()?;
                self.push_float(-value)?;
            }
            NumericInstr::FloatAbs
            | NumericInstr::FloatSqrt
            | NumericInstr::FloatFloor
            | NumericInstr::FloatCeil
            | NumericInstr::FloatRound
            | NumericInstr::FloatTrunc => {
                let value = self.pop_float()?;
                let result = match instr {
                    NumericInstr::FloatAbs => value.abs(),
                    NumericInstr::FloatSqrt => value.sqrt(),
                    NumericInstr::FloatFloor => value.floor(),
                    NumericInstr::FloatCeil => value.ceil(),
                    NumericInstr::FloatRound => value.round_ties_even(),
                    NumericInstr::FloatTrunc => value.trunc(),
                    _ => unreachable!(),
                };
                self.push_float(result)?;
            }
            NumericInstr::FloatAdd
            | NumericInstr::FloatSub
            | NumericInstr::FloatMul
            | NumericInstr::FloatDiv => {
                let right = self.pop_float()?;
                let left = self.pop_float()?;
                let value = match instr {
                    NumericInstr::FloatAdd => left + right,
                    NumericInstr::FloatSub => left - right,
                    NumericInstr::FloatMul => left * right,
                    NumericInstr::FloatDiv => left / right,
                    _ => unreachable!(),
                };
                self.push_float(value)?;
            }
            NumericInstr::FloatRem
            | NumericInstr::FloatCopySign
            | NumericInstr::FloatPow
            | NumericInstr::FloatHypot
            | NumericInstr::FloatAtan2 => {
                let right = self.pop_float()?;
                let left = self.pop_float()?;
                let value = match instr {
                    NumericInstr::FloatRem => lm_math::remainder(left, right),
                    NumericInstr::FloatCopySign => lm_math::copy_sign(left, right),
                    NumericInstr::FloatPow => lm_math::pow(left, right),
                    NumericInstr::FloatHypot => lm_math::hypot(left, right),
                    NumericInstr::FloatAtan2 => lm_math::atan2(left, right),
                    _ => unreachable!(),
                };
                self.push_float(value)?;
            }
            NumericInstr::FloatMulAdd => {
                let addend = self.pop_float()?;
                let multiplier = self.pop_float()?;
                let value = self.pop_float()?;
                self.push_float(lm_math::mul_add(value, multiplier, addend))?;
            }
            NumericInstr::FloatExp
            | NumericInstr::FloatExp2
            | NumericInstr::FloatExpM1
            | NumericInstr::FloatLn
            | NumericInstr::FloatLog2
            | NumericInstr::FloatLog10
            | NumericInstr::FloatLn1P
            | NumericInstr::FloatCbrt
            | NumericInstr::FloatSin
            | NumericInstr::FloatCos
            | NumericInstr::FloatTan
            | NumericInstr::FloatAsin
            | NumericInstr::FloatAcos
            | NumericInstr::FloatAtan
            | NumericInstr::FloatSinh
            | NumericInstr::FloatCosh
            | NumericInstr::FloatTanh
            | NumericInstr::FloatAsinh
            | NumericInstr::FloatAcosh
            | NumericInstr::FloatAtanh => {
                let input = self.pop_float()?;
                let value = match instr {
                    NumericInstr::FloatExp => lm_math::exp(input),
                    NumericInstr::FloatExp2 => lm_math::exp2(input),
                    NumericInstr::FloatExpM1 => lm_math::exp_m1(input),
                    NumericInstr::FloatLn => lm_math::ln(input),
                    NumericInstr::FloatLog2 => lm_math::log2(input),
                    NumericInstr::FloatLog10 => lm_math::log10(input),
                    NumericInstr::FloatLn1P => lm_math::ln_1p(input),
                    NumericInstr::FloatCbrt => lm_math::cbrt(input),
                    NumericInstr::FloatSin => lm_math::sin(input),
                    NumericInstr::FloatCos => lm_math::cos(input),
                    NumericInstr::FloatTan => lm_math::tan(input),
                    NumericInstr::FloatAsin => lm_math::asin(input),
                    NumericInstr::FloatAcos => lm_math::acos(input),
                    NumericInstr::FloatAtan => lm_math::atan(input),
                    NumericInstr::FloatSinh => lm_math::sinh(input),
                    NumericInstr::FloatCosh => lm_math::cosh(input),
                    NumericInstr::FloatTanh => lm_math::tanh(input),
                    NumericInstr::FloatAsinh => lm_math::asinh(input),
                    NumericInstr::FloatAcosh => lm_math::acosh(input),
                    NumericInstr::FloatAtanh => lm_math::atanh(input),
                    _ => unreachable!(),
                };
                self.push_float(value)?;
            }
            NumericInstr::FloatMin | NumericInstr::FloatMax => {
                let right = self.pop_float()?;
                let left = self.pop_float()?;
                let value = if left.is_nan() || right.is_nan() {
                    f64::NAN
                } else if left == right && left == 0.0 {
                    let bits = if matches!(instr, NumericInstr::FloatMin) {
                        left.to_bits() | right.to_bits()
                    } else {
                        left.to_bits() & right.to_bits()
                    };
                    f64::from_bits(bits)
                } else if matches!(instr, NumericInstr::FloatMin) {
                    if left < right {
                        left
                    } else {
                        right
                    }
                } else if left > right {
                    left
                } else {
                    right
                };
                self.push_float(value)?;
            }
            NumericInstr::FloatEq
            | NumericInstr::FloatNe
            | NumericInstr::FloatLt
            | NumericInstr::FloatLe
            | NumericInstr::FloatGt
            | NumericInstr::FloatGe => {
                let right = self.pop_float_bits()?;
                let left = self.pop_float_bits()?;
                let a = f64::from_bits(left);
                let b = f64::from_bits(right);
                let value = match instr {
                    NumericInstr::FloatEq => float_eq(left, right),
                    NumericInstr::FloatNe => !float_eq(left, right),
                    NumericInstr::FloatLt => a < b,
                    NumericInstr::FloatLe => a <= b,
                    NumericInstr::FloatGt => a > b,
                    NumericInstr::FloatGe => a >= b,
                    _ => unreachable!(),
                };
                self.push(Value::Bool(value))?;
            }
            NumericInstr::FloatIsNan => {
                let value = self.pop_float()?;
                self.push(Value::Bool(value.is_nan()))?;
            }
            NumericInstr::FloatIsFinite | NumericInstr::FloatIsInfinite => {
                let value = self.pop_float()?;
                let result = if matches!(instr, NumericInstr::FloatIsFinite) {
                    value.is_finite()
                } else {
                    value.is_infinite()
                };
                self.push(Value::Bool(result))?;
            }
            NumericInstr::FloatHash => {
                let value = self.pop_float_bits()?;
                self.push(Value::Int(float_hash(value)))?;
            }
            NumericInstr::FloatBits => {
                let value = self.pop_float_bits()?;
                self.push(Value::Int(canonical_float_bits(value) as i64))?;
            }
            NumericInstr::FloatFromBits => {
                let value = self.pop_int()? as u64;
                self.push(Value::Float(canonical_float_bits(value)))?;
            }
            NumericInstr::FloatToIntStatus => {
                let value = self.pop_float()?;
                let status = if !value.is_finite() {
                    1
                } else if !float_fits_int(value) {
                    2
                } else {
                    0
                };
                self.push(Value::Int(status))?;
            }
            NumericInstr::FloatToIntValue => {
                let value = self.pop_float()?;
                if !value.is_finite() {
                    return Err(FaultCode::BadCast);
                }
                if !float_fits_int(value) {
                    return Err(FaultCode::IntegerOverflow);
                }
                self.push(Value::Int(value.trunc() as i64))?;
            }
            NumericInstr::TextParseFloatStatus | NumericInstr::TextParseFloatValue => {
                let text = self.pop_obj()?;
                let parsed = parse_float_text(self.text_value(text)?.as_str());
                if matches!(instr, NumericInstr::TextParseFloatStatus) {
                    let status = match parsed {
                        Ok(_) => 0,
                        Err(status) => status,
                    };
                    self.push(Value::Int(status))?;
                } else {
                    self.push_float(parsed.unwrap_or(0.0))?;
                }
            }
            NumericInstr::FloatFixed => {
                let digits = self.pop_int()?;
                let value = self.pop_float()?;
                if digits < 0 {
                    return Err(FaultCode::InvalidPrecision);
                }
                let digits = usize::try_from(digits).map_err(|_| FaultCode::HeapLimit)?;
                let capacity = if value.is_finite() {
                    digits.checked_add(312).ok_or(FaultCode::HeapLimit)?
                } else {
                    4
                };
                self.reserve(capacity, &[])?;
                let mut output = String::new();
                output
                    .try_reserve_exact(capacity)
                    .map_err(|_| FaultCode::HeapLimit)?;
                write!(&mut output, "{value:.digits$}").map_err(|_| FaultCode::HeapLimit)?;
                let output =
                    SharedText::try_from_string(output).map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(output))?;
                self.push(value)?;
            }
            NumericInstr::SbAppendFloat => {
                let value = self.pop_float()?;
                let builder = self.pop_obj()?;
                self.frozen_guard(builder)?;
                let text = float_text(value).map_err(|_| FaultCode::HeapLimit)?;
                self.sb_append(builder, text.as_str())?;
            }
            NumericInstr::BytesBitAnd | NumericInstr::BytesBitOr | NumericInstr::BytesBitXor => {
                let right_ref = self.pop_obj()?;
                let left_ref = self.pop_obj()?;
                let (left, right) = match (self.vm.heap.get(left_ref), self.vm.heap.get(right_ref))
                {
                    (Object::Bytes(left), Object::Bytes(right)) => {
                        (left.as_slice(), right.as_slice())
                    }
                    _ => return Err(BAD_TYPE),
                };
                if left.len() != right.len() {
                    return Err(FaultCode::LengthMismatch);
                }
                let mut result = Vec::new();
                result
                    .try_reserve_exact(left.len())
                    .map_err(|_| FaultCode::HeapLimit)?;
                result.extend(left.iter().zip(right).map(|(left, right)| match instr {
                    NumericInstr::BytesBitAnd => left & right,
                    NumericInstr::BytesBitOr => left | right,
                    NumericInstr::BytesBitXor => left ^ right,
                    _ => unreachable!(),
                }));
                let value = self.alloc(Object::Bytes(SharedBytes::from(result)))?;
                self.push(value)?;
            }
            NumericInstr::BytesBitNot => {
                let reference = self.pop_obj()?;
                let bytes = match self.vm.heap.get(reference) {
                    Object::Bytes(bytes) => bytes.as_slice(),
                    _ => return Err(BAD_TYPE),
                };
                let mut result = Vec::new();
                result
                    .try_reserve_exact(bytes.len())
                    .map_err(|_| FaultCode::HeapLimit)?;
                result.extend(bytes.iter().map(|value| !value));
                let value = self.alloc(Object::Bytes(SharedBytes::from(result)))?;
                self.push(value)?;
            }
        }
        Ok(())
    }
}
