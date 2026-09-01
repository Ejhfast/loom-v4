//! Calls, callbacks, type closure, and native frame entry.

use super::*;

impl Machine {
    /// The type environment of the running frame.
    #[inline]
    pub(super) fn frame_env(&self) -> TypeEnvId {
        self.vm.frames.last().map(|f| f.env).unwrap_or_default()
    }

    /// Push the frame of one generic call.
    ///
    /// The three generic instructions live outside `exec_instr`, so
    /// the hot instruction body stays the size it had before the
    /// witness landed. A monomorphic program never reaches them.
    #[inline(never)]
    pub(super) fn call_generic(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        callee: u32,
        app: u32,
    ) -> Result<(), FaultCode> {
        let argc = module
            .funcs
            .get(callee as usize)
            .ok_or(BAD_STATE)?
            .params
            .len();
        let parent = self.frame_env();
        let env = envs.derive(module, parent, app).map_err(env_fault)?;
        self.push_frame(module, callee, argc, None, env)
    }

    /// Push the frame of one generic virtual call.
    ///
    /// The receiver object carries its class arguments, so the
    /// environment binds them first and the own arguments of the
    /// method after them.
    #[inline(never)]
    pub(super) fn call_virtual_generic(
        &mut self,
        module: &NamespaceRuntime,
        dispatch: &lm_bytecode::CodeTable<crate::DispatchRow>,
        envs: &mut TypeEnvs,
        selector: u32,
        argc: u32,
        app: u32,
    ) -> Result<(), FaultCode> {
        let argc = argc as usize;
        let recv = self.peek(argc)?;
        let parent = self.frame_env();
        let (target, env) = self
            .resolve_virtual_generic_target(module, dispatch, envs, parent, selector, app, recv)?;
        self.push_frame(module, target, argc + 1, None, env)
    }

    /// Resolve one verified generic virtual call without changing its frame.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn resolve_virtual_generic_target(
        &self,
        module: &NamespaceRuntime,
        dispatch: &lm_bytecode::CodeTable<crate::DispatchRow>,
        envs: &mut TypeEnvs,
        parent: TypeEnvId,
        selector: u32,
        app: u32,
        recv: Value,
    ) -> Result<(u32, TypeEnvId), FaultCode> {
        let (class, class_env) = match self.vm.heap.get(recv.as_obj().ok_or(BAD_TYPE)?) {
            Object::Instance { class, env, .. } => (*class, env.env()),
            _ => return Err(BAD_TYPE),
        };
        let target = method_of(dispatch, class, selector)?;
        let own = envs.derive(module, parent, app).map_err(env_fault)?;
        let env = envs
            .method_env(module, target, class, class_env, own)
            .map_err(env_fault)?;
        Ok((target, env))
    }

    /// Push one method frame selected through an interface bound.
    #[inline(never)]
    pub(super) fn call_interface(
        &mut self,
        module: &NamespaceRuntime,
        dispatch: &lm_bytecode::CodeTable<crate::DispatchRow>,
        envs: &mut TypeEnvs,
        call: InterfaceCallSite,
    ) -> Result<(), FaultCode> {
        let requirement = module
            .interfaces
            .get(call.interface as usize)
            .and_then(|contract| contract.methods.get(call.method as usize))
            .ok_or(BAD_STATE)?;
        let argc = u32::try_from(requirement.params.len()).map_err(|_| BAD_STATE)?;
        let argc = argc as usize;
        let recv = self.peek(argc)?;
        let parent = self.frame_env();
        let (target, env) = self.resolve_interface_target(
            module,
            dispatch,
            envs,
            parent,
            call.interface,
            call.method,
            call.recv_ty,
            call.app,
            recv,
        )?;
        self.push_frame(module, target, argc + 1, None, env)
    }

    /// Resolve one verified interface call without changing its frame.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn resolve_interface_target(
        &self,
        module: &NamespaceRuntime,
        dispatch: &lm_bytecode::CodeTable<crate::DispatchRow>,
        envs: &mut TypeEnvs,
        parent: TypeEnvId,
        interface: u32,
        method: u32,
        recv_ty: u32,
        app: u32,
        recv: Value,
    ) -> Result<(u32, TypeEnvId), FaultCode> {
        let requirement = module
            .interfaces
            .get(interface as usize)
            .and_then(|contract| contract.methods.get(method as usize))
            .ok_or(BAD_STATE)?;
        let selector = requirement.selector;
        let default = requirement.default;
        let receiver = envs.close(module, recv_ty, parent).map_err(env_fault)?;
        let option = module.core_roles[lm_bytecode::corepin::ROLE_OPTION];
        let static_class = match envs.ty(receiver) {
            Some(ClosedType::Class(class) | ClosedType::Inst(class, _)) => Some(*class),
            _ => None,
        };
        let class = match (static_class, recv) {
            (Some(class), _) if class == option => class,
            (Some(class), Value::EmptyCase { .. }) => class,
            _ => self.virtual_class(module, recv)?,
        };
        let own = if app == lm_bytecode::NO_APP {
            TypeEnvId::EMPTY
        } else {
            envs.derive(module, parent, app).map_err(env_fault)?
        };
        let witness = dispatch
            .get(class as usize)
            .and_then(|row| row.interface_witness(interface, method));
        let selected = if default == lm_bytecode::NO_FUNC {
            true
        } else {
            witness.ok_or(BAD_TYPE)?.0
        };
        let (target, env) = if selected {
            let target = dispatch
                .get(class as usize)
                .and_then(|row| row.method(selector))
                .ok_or(BAD_TYPE)?;
            let env = envs
                .interface_method_env(module, target, class, receiver, own)
                .map_err(env_fault)?
                .ok_or(BAD_TYPE)?;
            (target, env)
        } else {
            if default == lm_bytecode::NO_FUNC {
                return Err(BAD_TYPE);
            }
            let conformance = witness.ok_or(BAD_TYPE)?.1;
            let env = envs
                .interface_default_env(module, conformance, class, receiver, own)
                .map_err(env_fault)?
                .ok_or(BAD_TYPE)?;
            (default, env)
        };
        Ok((target, env))
    }

    /// Allocate one instance of a generic class.
    ///
    /// The instance records its own class arguments, so a later
    /// dispatch and a later reflection query read them from the object
    /// itself.
    #[inline(never)]
    pub(super) fn new_generic(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        class: u32,
        app: u32,
    ) -> Result<Value, FaultCode> {
        let field_count = module
            .classes
            .get(class as usize)
            .ok_or(BAD_STATE)?
            .fields
            .len();
        let parent = self.frame_env();
        let env = envs.derive(module, parent, app).map_err(env_fault)?;
        self.alloc(Object::Instance {
            class,
            fields: vec![Value::Uninit; field_count].into(),
            env: Witness(env),
        })
    }

    /// Create one callback outside the main dispatch body.
    #[inline(never)]
    pub(super) fn exec_make_callback(&mut self, func: u32, captures: u32) -> Result<(), FaultCode> {
        let split = self
            .vm
            .operands
            .len()
            .checked_sub(captures as usize)
            .ok_or(BAD_STATE)?;
        let captured: Vec<Value> = self.vm.operands.split_off(split);
        let value = self.alloc_callback(func, captured, self.frame_env())?;
        self.push(value)
    }

    /// Validate one heap closure as a nonescaping callback.
    #[inline(never)]
    pub(super) fn exec_as_callback(&mut self) -> Result<(), FaultCode> {
        let value = *self.vm.operands.last().ok_or(BAD_STATE)?;
        let Value::Obj(reference) = value else {
            return Err(BAD_TYPE);
        };
        if !matches!(self.vm.heap.get(reference), Object::Closure { .. }) {
            return Err(BAD_TYPE);
        }
        Ok(())
    }

    /// Close an `Option` family or arm type to its family type.
    pub(super) fn close_option_family(
        &self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        ty: u32,
    ) -> Result<ClosedTypeId, FaultCode> {
        self.close_option_family_at(module, envs, ty, self.frame_env())
    }

    /// Close one `Option` family under an explicit type environment.
    pub(crate) fn close_option_family_at(
        &self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        ty: u32,
        env: TypeEnvId,
    ) -> Result<ClosedTypeId, FaultCode> {
        let closed = envs.close(module, ty, env).map_err(env_fault)?;
        let (class, argument) = match envs.ty(closed) {
            Some(ClosedType::Inst(class, args)) if args.len() == 1 => (*class, args[0]),
            _ => return Err(BAD_STATE),
        };
        let option = module.core_roles[lm_bytecode::corepin::ROLE_OPTION];
        let some = module.core_roles[lm_bytecode::corepin::ROLE_OPTION_SOME];
        let none = module.core_roles[lm_bytecode::corepin::ROLE_OPTION_NONE];
        if option == lm_bytecode::NO_ROLE {
            return Err(BAD_STATE);
        }
        if class == option {
            return Ok(closed);
        }
        if class != some && class != none {
            return Err(BAD_STATE);
        }
        envs.intern(ClosedType::Inst(option, vec![argument]))
            .map_err(env_fault)
    }

    /// Resolve one callback reference.
    pub(crate) fn callback(
        &self,
        reference: CallbackRef,
    ) -> Result<&CallbackDescriptor, FaultCode> {
        let slot = self
            .callbacks
            .get(reference.slot as usize)
            .ok_or(BAD_TYPE)?;
        if slot.generation != reference.generation {
            return Err(BAD_TYPE);
        }
        slot.descriptor.as_ref().ok_or(BAD_TYPE)
    }

    /// Push a frame. The top `consume` operand values become the first
    /// local slots in order. `closure` supplies capture context for a
    /// closure call.
    pub(super) fn push_frame(
        &mut self,
        module: &NamespaceRuntime,
        callee: u32,
        consume: usize,
        closure: Option<FrameCapture>,
        env: TypeEnvId,
    ) -> Result<(), FaultCode> {
        if self.vm.frames.len() as u32 >= self.config.max_frames {
            return Err(FaultCode::StackLimit);
        }
        let func = module.funcs.get(callee as usize).ok_or(BAD_STATE)?;
        let base_local = self.vm.locals.len() as u32;
        let arg_start = self
            .vm
            .operands
            .len()
            .checked_sub(consume)
            .ok_or(BAD_STATE)?;
        let new_locals = self.vm.locals.len() + func.local_count() as usize;
        if new_locals + self.vm.operands.len() > self.config.max_stack_values as usize {
            return Err(FaultCode::StackLimit);
        }
        self.vm
            .locals
            .extend_from_slice(&self.vm.operands[arg_start..]);
        self.vm.operands.truncate(arg_start);
        // The slots after the parameters start without a value. The
        // marker states that fact: an uninitialized slot is not a unit
        // value, and a snapshot keeps the two apart.
        self.vm.locals.resize(new_locals, Value::Uninit);
        let base_operand = self.vm.operands.len() as u32;
        self.vm.frames.push(Frame {
            func: callee,
            block: 0,
            ip: 0,
            base_local,
            base_operand,
            closure,
            env,
        });
        Ok(())
    }
}
