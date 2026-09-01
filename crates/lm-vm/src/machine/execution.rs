//! Quantum execution and native resume policy.

use super::*;

impl Machine {
    pub(crate) fn native_return_depth(&self) -> Option<usize> {
        self.native_return_depth
    }

    pub(crate) fn set_native_return_depth(&mut self, depth: usize) {
        self.native_return_depth = Some(depth);
    }

    pub(crate) fn clear_native_return_depth(&mut self) {
        self.native_return_depth = None;
    }

    pub(crate) fn take_native_continuation(
        &mut self,
    ) -> Option<Box<crate::jit::NativeContinuation>> {
        self.native_continuation.take()
    }

    pub(crate) fn set_native_continuation(
        &mut self,
        continuation: Box<crate::jit::NativeContinuation>,
    ) {
        debug_assert!(self.native_continuation.is_none());
        debug_assert!(self.vm.frames.is_empty());
        debug_assert!(self.vm.locals.is_empty());
        debug_assert!(self.vm.operands.is_empty());
        self.native_continuation = Some(continuation);
    }

    pub(crate) fn has_native_continuation(&self) -> bool {
        self.native_continuation.is_some()
    }

    pub(crate) fn native_effect_reply_type(&self) -> Option<(u32, TypeEnvId)> {
        self.native_continuation
            .as_deref()
            .and_then(crate::jit::NativeContinuation::effect_reply_type)
    }

    pub(crate) fn install_native_effect_reply(&mut self, value: Value) -> Result<bool, FaultCode> {
        let Some(continuation) = self.native_continuation.as_deref_mut() else {
            return Ok(false);
        };
        continuation
            .install_effect_reply(value)
            .map_err(|()| FaultCode::MalformedState)
    }

    /// Execute until a boundary or an instruction count expires.
    ///
    /// `None` means the count expired after `retired` instructions.
    /// A boundary result includes the instruction that produced it.
    pub(crate) fn exec_for_quantum(
        &mut self,
        module: &NamespaceRuntime,
        dispatch: &lm_bytecode::CodeTable<crate::DispatchRow>,
        envs: &mut TypeEnvs,
        slots: Option<&[ImageSlotTarget]>,
        limit: u32,
        native: NativeResume<'_>,
    ) -> (Result<Option<ExecOutcome>, ExecError>, u32) {
        self.exec_for_quantum_mode::<false>(module, dispatch, envs, slots, limit, native)
    }

    /// Execute one restricted worker lease.
    #[cold]
    pub(crate) fn exec_for_quantum_restricted(
        &mut self,
        module: &NamespaceRuntime,
        dispatch: &lm_bytecode::CodeTable<crate::DispatchRow>,
        envs: &mut TypeEnvs,
        slots: Option<&[ImageSlotTarget]>,
        limit: u32,
        native: NativeResume<'_>,
    ) -> (Result<Option<ExecOutcome>, ExecError>, u32) {
        self.exec_for_quantum_mode::<true>(module, dispatch, envs, slots, limit, native)
    }

    pub(super) fn exec_for_quantum_mode<const RESTRICTED_LEASE: bool>(
        &mut self,
        module: &NamespaceRuntime,
        dispatch: &lm_bytecode::CodeTable<crate::DispatchRow>,
        envs: &mut TypeEnvs,
        slots: Option<&[ImageSlotTarget]>,
        limit: u32,
        native: NativeResume<'_>,
    ) -> (Result<Option<ExecOutcome>, ExecError>, u32) {
        debug_assert!(limit > 0);
        let original_fuel = self.vm.fuel;
        let batch_fuel = original_fuel.min(u64::from(limit));
        let held_fuel = original_fuel - batch_fuel;
        let count_expiry = u64::from(limit) <= original_fuel;
        self.vm.fuel = batch_fuel;
        let mut native = InterpreterNative::new(native, original_fuel);

        // Verification bounds every function, block, branch, and instruction.
        // Snapshot admission applies the same bounds to restored frames.
        // Keep the current block until a call or branch changes it.
        let mut cached_func = u32::MAX;
        let mut cached_block = u32::MAX;
        let mut code: &[Instr] = &[];
        let outcome = loop {
            if self.vm.fuel == 0 {
                break Err(ExecError::Fault(FaultCode::OutOfFuel));
            }
            let Some(frame) = self.vm.frames.last() else {
                break Err(ExecError::Fault(BAD_STATE));
            };
            let (func, block, ip) = (frame.func, frame.block, frame.ip);
            let function_changed = func != cached_func;
            if function_changed || block != cached_block {
                code = &module.funcs[func as usize].blocks[block as usize];
                cached_func = func;
                cached_block = block;
            }
            let instr = code[ip as usize];
            self.vm.fuel -= 1;
            let Some(frame) = self.vm.frames.last_mut() else {
                break Err(ExecError::Fault(BAD_STATE));
            };
            frame.ip += 1;
            match self.exec_instr(module, dispatch, envs, slots, instr, &mut native) {
                Ok(ExecOutcome::Continue) => {}
                Ok(ExecOutcome::ContinueNative) => break Ok(ExecOutcome::Continue),
                Ok(outcome) => break Ok(outcome),
                Err(code) => break Err(ExecError::Fault(code)),
            }
        };
        let retired = u32::try_from(batch_fuel - self.vm.fuel)
            .expect("one execution batch retires at most its u32 limit");
        self.vm.fuel += held_fuel;

        match outcome {
            Err(ExecError::Fault(FaultCode::OutOfFuel)) if count_expiry => (Ok(None), retired),
            Ok(outcome) => (Ok(Some(outcome)), retired),
            Err(error) => (Err(error), retired),
        }
    }

    /// Complete one native return after native state materialization.
    pub(crate) fn finish_native_return(&mut self, value: Value) -> Result<ExecOutcome, FaultCode> {
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
        Ok(ExecOutcome::Continue)
    }

    /// Retire one call after a materialized native exit.
    pub(crate) fn start_native_call(
        &mut self,
        module: &NamespaceRuntime,
        callee: u32,
        environment: TypeEnvId,
    ) -> Result<(), FaultCode> {
        let argc = module
            .funcs
            .get(callee as usize)
            .ok_or(BAD_STATE)?
            .params
            .len();
        let frame = self.vm.frames.last_mut().ok_or(BAD_STATE)?;
        frame.ip = frame.ip.checked_add(1).ok_or(BAD_STATE)?;
        self.push_frame(module, callee, argc, None, environment)
    }
}
