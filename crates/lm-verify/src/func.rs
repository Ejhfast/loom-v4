//! Per-function verification and the dataflow fixpoint.
//!
//! One part of the bytecode verifier. `lib.rs` holds the shared
//! context, the error type, and the entry points.

use super::*;

/// The expected operand count of one perform instruction. VM control
/// operations count their receiver.
pub(crate) fn perform_argc(ctx: &Ctx<'_>, op: u32) -> u32 {
    let def = ctx.bundle.op(op).expect("the operation slot was checked");
    match def.kind {
        lm_abi::OpKind::Fixed => def.params.len() as u32,
        lm_abi::OpKind::VmControl => match op {
            lm_abi::OP_VM_NEW => 0,
            lm_abi::OP_VM_RUN
            | lm_abi::OP_VM_STEP
            | lm_abi::OP_VM_DRIVE
            | lm_abi::OP_VM_DRIVE_WAIT
            | lm_abi::OP_VM_TABLE
            | lm_abi::OP_VM_HANDLES
            | lm_abi::OP_VM_RESOURCE_IS_OPEN
            | lm_abi::OP_VM_RESOURCE_CLOSE
            | lm_abi::OP_VM_RESOURCE_KIND
            | lm_abi::OP_VM_ARTIFACT
            | lm_abi::OP_COMPILER_VERIFY
            | lm_abi::OP_VM_INSTANCE_ENTRY
            | lm_abi::OP_VM_STACK => 1,
            lm_abi::OP_VM_DISPATCH => 2,
            lm_abi::OP_VM_ACTIVATE
            | lm_abi::OP_VM_ACTIVATE_OR_FAULT
            | lm_abi::OP_VM_ANSWER
            | lm_abi::OP_VM_BRANCH_ANSWER
            | lm_abi::OP_VM_REJECT
            | lm_abi::OP_VM_SERVE_TCP_STREAM => 3,
            lm_abi::OP_VM_ACTIVATE_DEF
            | lm_abi::OP_VM_REPLACE_FUNCTION
            | lm_abi::OP_VM_REPLACE_CLASS
            | lm_abi::OP_VM_REPLACE_VALUE
            | lm_abi::OP_VM_REPLACE_PROCESS
            | lm_abi::OP_VM_CHANGE_FUNCTION
            | lm_abi::OP_VM_CHANGE_CLASS
            | lm_abi::OP_VM_CHANGE_VALUE
            | lm_abi::OP_VM_CHANGE_PROCESS => 3,
            lm_abi::OP_PROC_RUN
            | lm_abi::OP_PROC_RUN_CLOSURE
            | lm_abi::OP_PROC_CLOSE
            | lm_abi::OP_PROC_DONE
            | lm_abi::OP_PROC_PAUSE
            | lm_abi::OP_PROC_RESUME
            | lm_abi::OP_PROC_RECV
            | lm_abi::OP_PROC_RECV_WAIT
            | lm_abi::OP_WAIT_WAIT
            | lm_abi::OP_WAIT_CANCEL
            | lm_abi::OP_WAIT_ANY
            | lm_abi::OP_VM_MODULE_ENTRY_CODE
            | lm_abi::OP_VM_INSTANCE_ENTRY_BINDING
            | lm_abi::OP_VM_BINDING_SLOT
            | lm_abi::OP_VM_BINDING_SPEC
            | lm_abi::OP_VM_BINDING_INSTANCE
            | lm_abi::OP_VM_BINDING_FUNCTION_TARGET
            | lm_abi::OP_VM_BINDING_CLASS_TARGET
            | lm_abi::OP_VM_BRANCH => 1,
            lm_abi::OP_PROC_SEND => 2,
            lm_abi::OP_PROC_SPAWN => 3,
            lm_abi::OP_VM_SNAPSHOT_SELF => 0,
            lm_abi::OP_VM_SNAPSHOT_HELD
            | lm_abi::OP_VM_LOAD_SNAPSHOT
            | lm_abi::OP_VM_SNAPSHOT_VM
            | lm_abi::OP_VM_RUN_SNAPSHOT_BYTES
            | lm_abi::OP_VM_SNAPSHOT_BYTES
            | lm_abi::OP_VM_RESTORE_VM => 1,
            lm_abi::OP_VM_RESTORE
            | lm_abi::OP_VM_RESTORE_DYNAMIC
            | lm_abi::OP_VM_RESOURCE
            | lm_abi::OP_VM_SERVE_FILE
            | lm_abi::OP_VM_SERVE_TCP_LISTENER
            | lm_abi::OP_VM_SERVE_TLS_STREAM
            | lm_abi::OP_VM_DRIVE_FOR
            | lm_abi::OP_VM_SNAPSHOT_WAIT_HELD
            | lm_abi::OP_PROC_SNAPSHOT_WAIT
            | lm_abi::OP_VM_RESOURCE_SAME
            | lm_abi::OP_VM_REPLACE_ALL
            | lm_abi::OP_WAIT_CHOOSE => 2,
            lm_abi::OP_VM_INSTALL
            | lm_abi::OP_VM_INSTANCE_FUNCTION
            | lm_abi::OP_VM_INSTANCE_CLASS
            | lm_abi::OP_VM_INSTANCE_SLOT_FOR
            | lm_abi::OP_VM_INSTANCE_SLOT_SPEC
            | lm_abi::OP_VM_INSTANCE_FUNCTION_BINDING
            | lm_abi::OP_VM_INSTANCE_CLASS_BINDING => 2,
            lm_abi::OP_VM_MODULE_FUNCTION_CODE | lm_abi::OP_VM_MODULE_CLASS_CODE => 2,
            lm_abi::OP_VM_INSTALL_WITH => 3,
            _ => unreachable!("every VmControl slot has an arity"),
        },
    }
}

/// Validate one type application against a callee's generic arity and
/// the caller's variable scope.
pub(crate) fn check_app(
    ctx: &Ctx<'_>,
    caller: &Func,
    fidx: u32,
    at: &dyn Fn(&str) -> String,
    app_idx: u32,
    want_types: u32,
    want_rows: u32,
) -> Result<(), VerifyError> {
    let app = ctx
        .module
        .apps
        .get(app_idx as usize)
        .ok_or_else(|| err(fidx, at("type application index out of range")))?;
    if app.types.len() != want_types as usize {
        return Err(err(fidx, at("type application arity mismatch")));
    }
    if app.rows.len() != want_rows as usize {
        return Err(err(fidx, at("type application row arity mismatch")));
    }
    for t in &app.types {
        if !ctx.vars_bounded(*t, caller.type_params, caller.effect_params) {
            return Err(err(
                fidx,
                at("type application uses a variable outside the caller scope"),
            ));
        }
        if !ctx.projections_proven(*t, &ctx.module.func_bounds[fidx as usize]) {
            return Err(err(
                fidx,
                at("type application uses an unproven associated type"),
            ));
        }
    }
    for row in &app.rows {
        if !ctx.row_vars_bounded(row, caller.effect_params) {
            return Err(err(
                fidx,
                at("type application row uses a variable outside the caller scope"),
            ));
        }
    }
    Ok(())
}

pub(crate) fn verify_func(
    ctx: &Ctx<'_>,
    func: &Func,
    fidx: u32,
) -> Result<Vec<Option<VerifiedBlockState>>, VerifyError> {
    let module = ctx.module;
    // Reject a forged slot count before any allocation is sized from
    // it. The dataflow pass allocates one state cell per block and
    // local, so both bounds run first.
    if func.local_count() > MAX_LOCAL_SLOTS {
        return Err(err(
            fidx,
            format!(
                "the local slot count {} exceeds the portable limit {MAX_LOCAL_SLOTS}",
                func.local_count()
            ),
        ));
    }
    if (func.blocks.len() as u64) * (func.local_count() as u64 + 1) > MAX_DATAFLOW_CELLS {
        return Err(err(
            fidx,
            "the function exceeds the verifier state budget; split it",
        ));
    }
    if func.blocks.is_empty() {
        return Err(err(fidx, "the function has no blocks"));
    }
    // Structural pass: every block ends with a terminator and every
    // operand index is inside its table.
    for (bidx, block) in func.blocks.iter().enumerate() {
        match block.last() {
            Some(last) if last.is_terminator() => {}
            _ => {
                return Err(err(
                    fidx,
                    format!("block {bidx} does not end with a terminator"),
                ));
            }
        }
        for (iidx, instr) in block.iter().enumerate() {
            if instr.is_terminator() && iidx + 1 != block.len() {
                return Err(err(
                    fidx,
                    format!("block {bidx} has a terminator before its end"),
                ));
            }
            let at = |what: &str| format!("block {bidx}, instruction {iidx}: {what}");
            let at_dyn: &dyn Fn(&str) -> String = &at;
            match instr {
                Instr::ConstStr(idx) => {
                    if *idx as usize >= module.strings.len() {
                        return Err(err(fidx, at("string index out of range")));
                    }
                }
                Instr::LoadLocal(slot) => {
                    if *slot >= func.local_count() {
                        return Err(err(fidx, at("local slot out of range")));
                    }
                }
                Instr::StoreLocal(slot) => {
                    if *slot >= func.local_count() {
                        return Err(err(fidx, at("local slot out of range")));
                    }
                    if matches!(
                        ctx.ty(func.local_types[*slot as usize]),
                        BcType::Callback(..)
                    ) {
                        return Err(err(fidx, at("a callback cannot be stored in a local")));
                    }
                }
                Instr::Call(callee) => {
                    let Some(target) = module.funcs.get(*callee as usize) else {
                        return Err(err(fidx, at("call target out of range")));
                    };
                    if !target.captures.is_empty() {
                        return Err(err(fidx, at("direct call to a function with captures")));
                    }
                    if ctx.is_interface_default(*callee) {
                        return Err(err(
                            fidx,
                            at("an interface default needs interface dispatch"),
                        ));
                    }
                    if target.type_params != 0 || target.effect_params != 0 {
                        return Err(err(fidx, at("a generic callee needs a type application")));
                    }
                }
                Instr::CallG { func: callee, app } => {
                    let Some(target) = module.funcs.get(*callee as usize) else {
                        return Err(err(fidx, at("call target out of range")));
                    };
                    if !target.captures.is_empty() {
                        return Err(err(fidx, at("direct call to a function with captures")));
                    }
                    if ctx.is_interface_default(*callee) {
                        return Err(err(
                            fidx,
                            at("an interface default needs interface dispatch"),
                        ));
                    }
                    if target.type_params == 0 && target.effect_params == 0 {
                        return Err(err(fidx, at("a type application on a non-generic callee")));
                    }
                    check_app(
                        ctx,
                        func,
                        fidx,
                        at_dyn,
                        *app,
                        target.type_params,
                        target.effect_params,
                    )?;
                    let application = &module.apps[*app as usize];
                    if !ctx.type_arguments_meet_bounds(
                        &application.types,
                        &application.rows,
                        &module.func_bounds[*callee as usize],
                        &module.func_bounds[fidx as usize],
                    ) {
                        return Err(err(
                            fidx,
                            at("a call type argument does not meet its interface bounds"),
                        ));
                    }
                    if ctx.constructor_class(*callee).is_some_and(|class| {
                        module.classes[class as usize].is_frozen
                            && application
                                .types
                                .iter()
                                .any(|ty| !ctx.type_always_frozen(*ty, false))
                    }) {
                        return Err(err(
                            fidx,
                            at("a frozen constructor needs always-frozen type arguments"),
                        ));
                    }
                }
                Instr::CallVirtual { selector, .. } => {
                    if *selector as usize >= module.selectors.len() {
                        return Err(err(fidx, at("selector index out of range")));
                    }
                }
                Instr::CallVirtualG { selector, app, .. } => {
                    if *selector as usize >= module.selectors.len() {
                        return Err(err(fidx, at("selector index out of range")));
                    }
                    // The full arity check needs the receiver type and
                    // runs in the dataflow pass. The structural pass
                    // bounds the index and the variable scopes, so the
                    // dataflow pass can index the table safely.
                    let Some(a) = module.apps.get(*app as usize) else {
                        return Err(err(fidx, at("type application index out of range")));
                    };
                    for t in &a.types {
                        if !ctx.vars_bounded(*t, func.type_params, func.effect_params) {
                            return Err(err(
                                fidx,
                                at("type application uses a variable outside the caller scope"),
                            ));
                        }
                    }
                    for row in &a.rows {
                        if !ctx.row_vars_bounded(row, func.effect_params) {
                            return Err(err(
                                fidx,
                                at("type application row uses a variable outside the caller scope"),
                            ));
                        }
                    }
                }
                Instr::MakeClosure { func: f, captures } => {
                    let Some(target) = module.funcs.get(*f as usize) else {
                        return Err(err(fidx, at("closure function out of range")));
                    };
                    if target.captures.len() != *captures as usize {
                        return Err(err(fidx, at("closure capture count mismatch")));
                    }
                    // A closure body shares the generic scope of the
                    // function that creates it, so it must keep the
                    // same arity. A target that declares no generic
                    // parameter at all has no free variable to bind:
                    // its signature is closed, so any scope may close
                    // over it. The `spawn` sugar takes that path.
                    let closed = target.type_params == 0 && target.effect_params == 0;
                    if !closed
                        && (target.type_params != func.type_params
                            || target.effect_params != func.effect_params)
                    {
                        return Err(err(
                            fidx,
                            at("a closure body must keep the enclosing generic arity"),
                        ));
                    }
                }
                Instr::LoadCapture(idx) => {
                    if *idx as usize >= func.captures.len() {
                        return Err(err(fidx, at("capture index out of range")));
                    }
                }
                Instr::New(class) => {
                    let Some(c) = module.classes.get(*class as usize) else {
                        return Err(err(fidx, at("class index out of range")));
                    };
                    if c.kind == BcClassKind::Abstract {
                        return Err(err(fidx, at("cannot allocate an abstract enum parent")));
                    }
                    if ctx.is_native_core_class(*class) {
                        return Err(err(fidx, at("New cannot allocate a native core class")));
                    }
                    if c.type_params != 0 {
                        return Err(err(fidx, at("a generic class needs a type application")));
                    }
                }
                Instr::NewG { class, app } => {
                    let Some(c) = module.classes.get(*class as usize) else {
                        return Err(err(fidx, at("class index out of range")));
                    };
                    if c.kind == BcClassKind::Abstract {
                        return Err(err(fidx, at("cannot allocate an abstract enum parent")));
                    }
                    if ctx.is_native_core_class(*class) {
                        return Err(err(fidx, at("NewG cannot allocate a native core class")));
                    }
                    if c.type_params == 0 {
                        return Err(err(fidx, at("a type application on a non-generic class")));
                    }
                    check_app(ctx, func, fidx, at_dyn, *app, c.type_params, 0)?;
                    let application = &module.apps[*app as usize];
                    if !ctx.type_arguments_meet_bounds(
                        &application.types,
                        &[],
                        &module.class_bounds[*class as usize],
                        &module.func_bounds[fidx as usize],
                    ) {
                        return Err(err(
                            fidx,
                            at("a class type argument does not meet its interface bounds"),
                        ));
                    }
                }
                Instr::ListNew { ty, .. }
                | Instr::MapNew { ty, .. }
                | Instr::TupleNew { ty, .. }
                | Instr::IsType(ty)
                | Instr::CastType(ty)
                | Instr::MapPut { ty, .. } => {
                    if *ty as usize >= module.types.len() {
                        return Err(err(fidx, at("type index out of range")));
                    }
                }
                Instr::Jump(target) | Instr::JumpIfFalse(target) | Instr::JumpIfTrue(target) => {
                    if *target as usize >= func.blocks.len() {
                        return Err(err(fidx, at("jump target is not a block")));
                    }
                }
                Instr::Perform { op, argc, reply_ty } => {
                    if ctx.bundle.op(*op).is_none() {
                        return Err(err(fidx, at("perform operation slot out of range")));
                    }
                    if *argc != perform_argc(ctx, *op) {
                        return Err(err(fidx, at("perform argument count mismatch")));
                    }
                    if *reply_ty as usize >= module.types.len() {
                        return Err(err(fidx, at("perform reply type index out of range")));
                    }
                }
                Instr::PerformValue { reply_ty, .. } => {
                    if *reply_ty as usize >= module.types.len() {
                        return Err(err(fidx, at("perform reply type index out of range")));
                    }
                }
                Instr::OpConst(op) => {
                    if ctx
                        .bundle
                        .op(*op)
                        .is_none_or(|operation| operation.kind != lm_abi::OpKind::Fixed)
                    {
                        return Err(err(
                            fidx,
                            at("first-class operation slot is out of range or not fixed"),
                        ));
                    }
                }
                Instr::CallInterface { site, recv_ty, app } => {
                    let (interface, method) = lm_bytecode::unpack_interface_call_site(*site);
                    let Some(contract) = module.interfaces.get(interface as usize) else {
                        return Err(err(fidx, at("interface index out of range")));
                    };
                    let Some(requirement) = contract.methods.get(method as usize) else {
                        return Err(err(fidx, at("interface method index out of range")));
                    };
                    if !ctx.vars_bounded(*recv_ty, func.type_params, func.effect_params) {
                        return Err(err(
                            fidx,
                            at("interface receiver type uses a variable outside the caller scope"),
                        ));
                    }
                    if !ctx.projections_proven(*recv_ty, &module.func_bounds[fidx as usize]) {
                        return Err(err(
                            fidx,
                            at("interface receiver type uses an unproven associated type"),
                        ));
                    }
                    if requirement.type_params == 0 && requirement.effect_params == 0 {
                        if *app != lm_bytecode::NO_APP {
                            return Err(err(
                                fidx,
                                at("a non-generic interface method has a type application"),
                            ));
                        }
                    } else {
                        if *app == lm_bytecode::NO_APP {
                            return Err(err(
                                fidx,
                                at("a generic interface method needs a type application"),
                            ));
                        }
                        check_app(
                            ctx,
                            func,
                            fidx,
                            at_dyn,
                            *app,
                            requirement.type_params,
                            requirement.effect_params,
                        )?;
                    }
                }
                Instr::Extended(instr) => match instr {
                    ExtendedInstr::PrepareWait { op_argc, reply_ty } => {
                        let (op, argc) = ExtendedInstr::wait_parts(*op_argc);
                        let Some(operation) = ctx.bundle.op(op) else {
                            return Err(err(fidx, at("wait operation slot out of range")));
                        };
                        if operation.kind != lm_abi::OpKind::Fixed || !operation.wait_source {
                            return Err(err(fidx, at("operation is not a wait source")));
                        }
                        if argc != operation.params.len() as u32 {
                            return Err(err(fidx, at("wait argument count mismatch")));
                        }
                        if *reply_ty as usize >= module.types.len() {
                            return Err(err(fidx, at("wait reply type index out of range")));
                        }
                    }
                    ExtendedInstr::MakeCallback { func: f, captures } => {
                        let Some(target) = module.funcs.get(*f as usize) else {
                            return Err(err(fidx, at("closure function out of range")));
                        };
                        if target.captures.len() != *captures as usize {
                            return Err(err(fidx, at("closure capture count mismatch")));
                        }
                        let closed = target.type_params == 0 && target.effect_params == 0;
                        if !closed
                            && (target.type_params != func.type_params
                                || target.effect_params != func.effect_params)
                        {
                            return Err(err(
                                fidx,
                                at("a closure body must keep the enclosing generic arity"),
                            ));
                        }
                    }
                    ExtendedInstr::FunctionCode { func: target } => {
                        let Some(target) = module.funcs.get(*target as usize) else {
                            return Err(err(fidx, at("function code target out of range")));
                        };
                        if target.type_params != 0
                            || target.effect_params != 0
                            || !target.captures.is_empty()
                            || target.param_muts.iter().any(|marker| *marker)
                        {
                            return Err(err(fidx, at("function code target is not portable")));
                        }
                    }
                    ExtendedInstr::ClassCode { class } => {
                        if module.classes.get(*class as usize).is_none() {
                            return Err(err(fidx, at("class code target out of range")));
                        }
                    }
                    ExtendedInstr::OptionSome { ty }
                    | ExtendedInstr::OptionNone { ty }
                    | ExtendedInstr::OptionPayload { ty }
                    | ExtendedInstr::ListGet { ty }
                    | ExtendedInstr::MapGet { ty }
                    | ExtendedInstr::MapPutText { ty, .. }
                    | ExtendedInstr::ListPop { ty }
                    | ExtendedInstr::MapRemove { ty }
                    | ExtendedInstr::DynPack { ty }
                    | ExtendedInstr::CodeSource { ty }
                    | ExtendedInstr::FaultSite { ty }
                    | ExtendedInstr::FaultTrace { ty } => {
                        if *ty as usize >= module.types.len() {
                            return Err(err(fidx, at("type index out of range")));
                        }
                    }
                    ExtendedInstr::CallSlot { slot, app } => {
                        let Some(spec) = module.slots.get(*slot as usize) else {
                            return Err(err(fidx, at("slot index out of range")));
                        };
                        let callable = match &spec.contract {
                            SlotContract::Function(contract) | SlotContract::Method(contract) => {
                                contract
                            }
                            _ => {
                                return Err(err(fidx, at("CALL_SLOT needs a callable slot")));
                            }
                        };
                        match (
                            *app != lm_bytecode::NO_APP,
                            callable.type_params,
                            callable.effect_params,
                        ) {
                            (false, 0, 0) => {}
                            (false, _, _) => {
                                return Err(err(
                                    fidx,
                                    at("a generic slot call needs a type application"),
                                ));
                            }
                            (true, 0, 0) => {
                                return Err(err(
                                    fidx,
                                    at("a non-generic slot call has a type application"),
                                ));
                            }
                            (true, types, rows) => {
                                check_app(ctx, func, fidx, at_dyn, *app, types, rows)?;
                                let application = &module.apps[*app as usize];
                                if !ctx.type_arguments_meet_bounds(
                                    &application.types,
                                    &application.rows,
                                    &callable.type_bounds,
                                    &module.func_bounds[fidx as usize],
                                ) {
                                    return Err(err(
                                        fidx,
                                        at("a slot type argument does not meet its interface bounds"),
                                    ));
                                }
                            }
                        }
                    }
                    ExtendedInstr::NewSlot { slot, app } => {
                        let Some(spec) = module.slots.get(*slot as usize) else {
                            return Err(err(fidx, at("slot index out of range")));
                        };
                        let SlotContract::Class { constructor, .. } = &spec.contract else {
                            return Err(err(fidx, at("NEW_SLOT needs a class slot")));
                        };
                        match (
                            *app != lm_bytecode::NO_APP,
                            constructor.type_params,
                            constructor.effect_params,
                        ) {
                            (false, 0, 0) => {}
                            (false, _, _) => {
                                return Err(err(
                                    fidx,
                                    at("a generic class slot needs a type application"),
                                ));
                            }
                            (true, 0, 0) => {
                                return Err(err(
                                    fidx,
                                    at("a plain class slot has a type application"),
                                ));
                            }
                            (true, types, rows) => {
                                check_app(ctx, func, fidx, at_dyn, *app, types, rows)?;
                                let application = &module.apps[*app as usize];
                                if !ctx.type_arguments_meet_bounds(
                                    &application.types,
                                    &application.rows,
                                    &constructor.type_bounds,
                                    &module.func_bounds[fidx as usize],
                                ) {
                                    return Err(err(
                                        fidx,
                                        at("a class slot type argument does not meet its interface bounds"),
                                    ));
                                }
                                let class = match ctx.ty(constructor.ret) {
                                    BcType::Class(class) | BcType::Inst(class, _) => Some(class),
                                    _ => None,
                                };
                                if class.is_some_and(|class| {
                                    module.classes[class as usize].is_frozen
                                        && application
                                            .types
                                            .iter()
                                            .any(|ty| !ctx.type_always_frozen(*ty, false))
                                }) {
                                    return Err(err(
                                        fidx,
                                        at("a frozen class slot needs always-frozen type arguments"),
                                    ));
                                }
                            }
                        }
                    }
                    ExtendedInstr::LoadSlot { slot } => {
                        let Some(spec) = module.slots.get(*slot as usize) else {
                            return Err(err(fidx, at("slot index out of range")));
                        };
                        if !matches!(&spec.contract, SlotContract::Value { .. }) {
                            return Err(err(fidx, at("LOAD_SLOT needs a value slot")));
                        }
                    }
                    ExtendedInstr::SendSlot { slot } => {
                        let Some(spec) = module.slots.get(*slot as usize) else {
                            return Err(err(fidx, at("slot index out of range")));
                        };
                        if !matches!(&spec.contract, SlotContract::Process { .. }) {
                            return Err(err(fidx, at("SEND_SLOT needs a process slot")));
                        }
                    }
                    _ => {}
                },
                // A typed call token names a fixed host operation, or
                // the receiverless self snapshot. A restored self
                // snapshot holds that request pending, and the
                // restorer answers it through the ordinary typed call
                // path (specification 17.6).
                Instr::AsCall { op, ty } => {
                    let answerable = *op < ctx.bundle.op_count()
                        && (ctx.bundle.op(*op).expect("the operation exists").kind
                            == lm_abi::OpKind::Fixed
                            || *op == lm_abi::OP_VM_SNAPSHOT_SELF);
                    if !answerable {
                        return Err(err(
                            fidx,
                            at("as_call operation slot is out of range or not answerable"),
                        ));
                    }
                    if *ty as usize >= module.types.len() {
                        return Err(err(fidx, at("as_call type index out of range")));
                    }
                }
                Instr::TableEdit { action, kind, slot } => {
                    if *action > 3 || *kind > 1 {
                        return Err(err(fidx, at("invalid table edit encoding")));
                    }
                    let bound = if *kind == 0 {
                        ctx.bundle.op_count()
                    } else {
                        ctx.bundle.group_count()
                    };
                    if *slot >= bound {
                        return Err(err(fidx, at("table edit target out of range")));
                    }
                    if *action == 2
                        && (*kind != 0
                            || ctx.bundle.op(*slot).expect("the operation exists").kind
                                != lm_abi::OpKind::Fixed)
                    {
                        return Err(err(
                            fidx,
                            at("a mock target must be an exact fixed operation"),
                        ));
                    }
                }
                _ => {}
            }
        }
    }
    dataflow(ctx, func, fidx)
}

/// Reconstruct the abstract state at every reachable block entry.
///
/// The pass is the type proof of one function body. It also serves the
/// snapshot loader, which reads the operand types of a stopped frame
/// from exactly this state, so the loader and the verifier can never
/// disagree about what a program point holds.
pub(crate) fn dataflow(
    ctx: &Ctx<'_>,
    func: &Func,
    fidx: u32,
) -> Result<Vec<Option<VerifiedBlockState>>, VerifyError> {
    let mut states: Vec<Option<VerifiedBlockState>> = vec![None; func.blocks.len()];
    let mut locals = vec![None; func.local_count() as usize];
    for (i, p) in func.params.iter().enumerate() {
        locals[i] = Some(*p);
    }
    states[0] = Some(VerifiedBlockState {
        locals,
        stack: Vec::new(),
    });
    let mut worklist = VecDeque::new();
    worklist.push_back(0usize);
    while let Some(bidx) = worklist.pop_front() {
        let mut state = states[bidx].clone().expect("queued block has a state");
        for (iidx, instr) in func.blocks[bidx].iter().enumerate() {
            step(
                ctx,
                func,
                fidx,
                bidx,
                iidx,
                instr,
                &mut state,
                |target, edge_state| {
                    merge(ctx, fidx, target, edge_state, &mut states, &mut worklist)
                },
            )?;
        }
    }
    Ok(states)
}

/// Replay verified blocks and copy only the requested program-point states.
pub(crate) fn states_at_points(
    ctx: &Ctx<'_>,
    func: &Func,
    fidx: u32,
    entries: &[Option<VerifiedBlockState>],
    points: &[(u32, u32)],
) -> Result<Vec<Option<VerifiedBlockState>>, VerifyError> {
    let mut requests = vec![Vec::<(usize, usize)>::new(); func.blocks.len()];
    for (result, (block, instruction)) in points.iter().copied().enumerate() {
        let Some(code) = func.blocks.get(block as usize) else {
            return Err(err(
                fidx,
                "a requested state has a block outside the function",
            ));
        };
        let instruction = instruction as usize;
        if instruction > code.len() {
            return Err(err(
                fidx,
                "a requested state has an instruction outside the block",
            ));
        }
        requests[block as usize].push((instruction, result));
    }
    for block in &mut requests {
        block.sort_unstable_by_key(|(instruction, _)| *instruction);
    }

    let mut results = vec![None; points.len()];
    for (block, requests) in requests.iter().enumerate() {
        if requests.is_empty() {
            continue;
        }
        let Some(mut state) = entries.get(block).and_then(Clone::clone) else {
            continue;
        };
        let code = &func.blocks[block];
        let mut request = 0usize;
        for instruction in 0..=code.len() {
            while requests
                .get(request)
                .is_some_and(|(position, _)| *position == instruction)
            {
                let result = requests[request].1;
                results[result] = Some(state.clone());
                request += 1;
            }
            let Some(operation) = code.get(instruction) else {
                break;
            };
            step(
                ctx,
                func,
                fidx,
                block,
                instruction,
                operation,
                &mut state,
                |_, _| Ok(()),
            )?;
        }
    }
    Ok(results)
}

pub(crate) fn merge(
    ctx: &Ctx<'_>,
    fidx: u32,
    target: usize,
    edge: VerifiedBlockState,
    states: &mut [Option<VerifiedBlockState>],
    worklist: &mut VecDeque<usize>,
) -> Result<(), VerifyError> {
    match &mut states[target] {
        slot @ None => {
            *slot = Some(edge);
            worklist.push_back(target);
        }
        Some(existing) => {
            if existing.stack.len() != edge.stack.len() {
                return Err(err(
                    fidx,
                    format!("block {target} entry stack shapes do not agree"),
                ));
            }
            let mut changed = false;
            for (have, new) in existing.stack.iter_mut().zip(edge.stack.iter()) {
                if *have != *new {
                    let joined = ctx.join(*have, *new).ok_or_else(|| {
                        err(
                            fidx,
                            format!("block {target} entry stack types have no common type"),
                        )
                    })?;
                    if joined != *have {
                        *have = joined;
                        changed = true;
                    }
                }
            }
            for (have, new) in existing.locals.iter_mut().zip(edge.locals.iter()) {
                let merged = match (*have, *new) {
                    (Some(a), Some(b)) => {
                        if a == b {
                            Some(a)
                        } else {
                            ctx.join(a, b)
                        }
                    }
                    _ => None,
                };
                if merged != *have {
                    *have = merged;
                    changed = true;
                }
            }
            if changed {
                worklist.push_back(target);
            }
        }
    }
    Ok(())
}
