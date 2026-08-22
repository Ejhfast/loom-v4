//! The per-instruction transfer function.
//!
//! One part of the bytecode verifier. `lib.rs` holds the shared
//! context, the error type, and the entry points.

use super::*;

/// Merge an edge state into a target block. Queue the block again when
/// its entry state changes.
/// Prove that a perform instruction states the reply type its program
/// point proves.
///
/// The world reads `reply_ty` at run time and checks the reply value
/// against it at every boundary crossing. The check is worth nothing
/// unless the stated index is the type the dataflow pushes, so this
/// rule ties the two together. The rule reads the module type table
/// and the dataflow state alone, so no snapshot container takes part.
///
/// The test is equality, never subtyping. The consumer of the reply
/// reads it at exactly the type the dataflow pushed, so a wider stated
/// type would weaken the run-time check.
pub(crate) fn check_reply_ty(
    ctx: &Ctx<'_>,
    state: &State,
    reply_ty: u32,
    fail: &dyn Fn(String) -> VerifyError,
) -> Result<(), VerifyError> {
    let Some(pushed) = state.stack.last().copied() else {
        return Err(fail("a perform pushed no reply".to_string()));
    };
    if reply_ty as usize >= ctx.module.types.len() {
        return Err(fail(format!(
            "the perform states reply type {reply_ty}, which the module has not"
        )));
    }
    // The universe starts with the module type table and interns by
    // content, so equal types take one index.
    if pushed != reply_ty {
        return Err(fail(format!(
            "the perform states reply type {reply_ty} and the program point proves {pushed}"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn step(
    ctx: &Ctx<'_>,
    func: &Func,
    fidx: u32,
    bidx: usize,
    iidx: usize,
    instr: &Instr,
    state: &mut State,
    mut edge: impl FnMut(usize, State) -> Result<(), VerifyError>,
) -> Result<(), VerifyError> {
    let module = ctx.module;
    let fail = |what: String| err(fidx, format!("block {bidx}, instruction {iidx}: {what}"));
    let pop = |state: &mut State| -> Result<u32, VerifyError> {
        state
            .stack
            .pop()
            .ok_or_else(|| fail("pop from an empty stack".to_string()))
    };
    let pop_expect = |state: &mut State, want: u32| -> Result<u32, VerifyError> {
        let ty = pop(state)?;
        if !ctx.is_subtype(ty, want) {
            return Err(fail(format!(
                "expected type {want} on the stack, found type {ty}"
            )));
        }
        Ok(ty)
    };
    let push = |state: &mut State, ty: u32| -> Result<(), VerifyError> {
        if state.stack.len() >= MAX_STATIC_STACK {
            return Err(fail("static stack depth limit exceeded".to_string()));
        }
        state.stack.push(ty);
        Ok(())
    };
    // Pop `count` values that must match `params` in declaration order.
    let pop_args = |state: &mut State, params: &[u32]| -> Result<(), VerifyError> {
        for want in params.iter().rev() {
            pop_expect(state, *want)?;
        }
        Ok(())
    };
    let as_list = |ty: u32| -> Result<u32, VerifyError> {
        match ctx.ty(ty) {
            BcType::List(e) => Ok(e),
            _ => Err(fail(format!("expected a list type, found type {ty}"))),
        }
    };
    let as_map = |ty: u32| -> Result<(u32, u32), VerifyError> {
        match ctx.ty(ty) {
            BcType::Map(k, v) => Ok((k, v)),
            _ => Err(fail(format!("expected a map type, found type {ty}"))),
        }
    };
    // The claimed row of a call must sit inside the caller's row.
    let charge_row = |row: &[BcRow]| -> Result<(), VerifyError> {
        if ctx.row_included(row, &func.row) {
            Ok(())
        } else {
            Err(fail(
                "the callee row is not inside the caller's declared row".to_string(),
            ))
        }
    };
    match instr {
        Instr::ConstUnit => push(state, TY_UNIT)?,
        Instr::ConstBool(_) => push(state, TY_BOOL)?,
        Instr::ConstInt(_) => push(state, TY_INT)?,
        Instr::ConstStr(_) => push(state, TY_STR)?,
        Instr::LoadLocal(slot) => {
            let ty = state.locals[*slot as usize]
                .ok_or_else(|| fail("load from a local without a value".to_string()))?;
            push(state, ty)?;
        }
        Instr::StoreLocal(slot) => {
            // The declared local-type table is the typing judgment:
            // a store must fit the declared slot type, and the slot
            // holds the declared type afterwards. This keeps a
            // widened local at its declared type instead of the
            // concrete stored type.
            let ty = pop(state)?;
            let declared = func.local_types[*slot as usize];
            if !ctx.is_subtype(ty, declared) {
                return Err(fail(format!(
                    "store to local {slot} expects the declared type {declared}, \
                     found type {ty}"
                )));
            }
            state.locals[*slot as usize] = Some(declared);
        }
        Instr::Pop => {
            pop(state)?;
        }
        Instr::Add | Instr::Sub | Instr::Mul | Instr::Div | Instr::Rem => {
            pop_expect(state, TY_INT)?;
            pop_expect(state, TY_INT)?;
            push(state, TY_INT)?;
        }
        Instr::Neg => {
            pop_expect(state, TY_INT)?;
            push(state, TY_INT)?;
        }
        Instr::Not => {
            pop_expect(state, TY_BOOL)?;
            push(state, TY_BOOL)?;
        }
        Instr::LtInt | Instr::LeInt | Instr::GtInt | Instr::GeInt | Instr::EqInt | Instr::NeInt => {
            pop_expect(state, TY_INT)?;
            pop_expect(state, TY_INT)?;
            push(state, TY_BOOL)?;
        }
        Instr::EqBool | Instr::NeBool => {
            pop_expect(state, TY_BOOL)?;
            pop_expect(state, TY_BOOL)?;
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::EqStr)
        | Instr::Native(lm_bytecode::NativeInstr::NeStr) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::StrByteLen)
        | Instr::Native(lm_bytecode::NativeInstr::StrCharCount) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::StrConcat) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            push(state, TY_STR)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::StrStartsWith)
        | Instr::Native(lm_bytecode::NativeInstr::StrEndsWith)
        | Instr::Native(lm_bytecode::NativeInstr::StrContains) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::StrFindIndex)
        | Instr::Native(lm_bytecode::NativeInstr::TextFindByteIndex) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            push(state, TY_INT)?;
        }
        Instr::Native(
            lm_bytecode::NativeInstr::TextLt
            | lm_bytecode::NativeInstr::TextLe
            | lm_bytecode::NativeInstr::TextGt
            | lm_bytecode::NativeInstr::TextGe,
        ) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextAt | lm_bytecode::NativeInstr::TextAtByte) => {
            pop_expect(state, TY_INT)?;
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            let value = ctx.plain_inst(ctx.core.char_value, "Char").map_err(&fail)?;
            push(state, value)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextSlice) => {
            pop_expect(state, TY_INT)?;
            pop_expect(state, TY_INT)?;
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            let value = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            push(state, value)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextIsBoundary) => {
            pop_expect(state, TY_INT)?;
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextSliceBytes) => {
            pop_expect(state, TY_INT)?;
            pop_expect(state, TY_INT)?;
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            let value = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            push(state, value)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextBytes) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            push(state, ctx.intern(BcType::Bytes))?;
        }
        Instr::Native(
            lm_bytecode::NativeInstr::TextTrim
            | lm_bytecode::NativeInstr::TextTrimStart
            | lm_bytecode::NativeInstr::TextTrimEnd,
        ) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            let value = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            push(state, value)?;
        }
        Instr::Native(
            lm_bytecode::NativeInstr::TextToLowerAscii | lm_bytecode::NativeInstr::TextToUpperAscii,
        ) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            push(state, TY_STR)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextReplace) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            push(state, TY_STR)?;
        }
        Instr::Native(
            lm_bytecode::NativeInstr::TextParseIntStatus
            | lm_bytecode::NativeInstr::TextParseIntValue,
        ) => {
            pop_expect(state, TY_INT)?;
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextSplit) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            let piece = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            let list = ctx.intern(BcType::List(piece));
            push(state, list)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextLines) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            let piece = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            let list = ctx.intern(BcType::List(piece));
            push(state, list)?;
        }
        Instr::Native(
            lm_bytecode::NativeInstr::BytesEndsWith | lm_bytecode::NativeInstr::BytesContains,
        ) => {
            let bytes = ctx.intern(BcType::Bytes);
            pop_expect(state, bytes)?;
            pop_expect(state, bytes)?;
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::SubstringToString) => {
            let value = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            pop_expect(state, value)?;
            push(state, TY_STR)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::CharCodepoint)
        | Instr::Native(lm_bytecode::NativeInstr::CharUtf8Len) => {
            let value = ctx.plain_inst(ctx.core.char_value, "Char").map_err(&fail)?;
            pop_expect(state, value)?;
            push(state, TY_INT)?;
        }
        Instr::Native(
            lm_bytecode::NativeInstr::EqChar
            | lm_bytecode::NativeInstr::NeChar
            | lm_bytecode::NativeInstr::LtChar
            | lm_bytecode::NativeInstr::LeChar
            | lm_bytecode::NativeInstr::GtChar
            | lm_bytecode::NativeInstr::GeChar,
        ) => {
            let value = ctx.plain_inst(ctx.core.char_value, "Char").map_err(&fail)?;
            pop_expect(state, value)?;
            pop_expect(state, value)?;
            push(state, TY_BOOL)?;
        }
        Instr::EqValue | Instr::NeValue => {
            // Structural equality reads two related enum values. The
            // machine walks the case and the fields, so the verifier
            // proves the operand kind and nothing about the walk.
            let b = pop(state)?;
            let a = pop(state)?;
            let enum_side = |t: u32| {
                ctx.as_instance(t)
                    .map(|(class, _)| ctx.is_enum_class(class))
                    .unwrap_or(false)
            };
            if !enum_side(a) || !enum_side(b) {
                return Err(fail(format!(
                    "structural equality needs two enum values, found {a} and {b}"
                )));
            }
            if !(ctx.is_subtype(a, b) || ctx.is_subtype(b, a)) {
                return Err(fail(format!(
                    "structural equality needs related types, found {a} and {b}"
                )));
            }
            push(state, TY_BOOL)?;
        }
        Instr::EqRef | Instr::NeRef => {
            let b = pop(state)?;
            let a = pop(state)?;
            let excluded = |t: u32| {
                if matches!(ctx.ty(t), BcType::Str | BcType::Bytes | BcType::Tuple(_)) {
                    return true;
                }
                let Some((class, args)) = ctx.as_instance(t) else {
                    return false;
                };
                args.is_empty()
                    && (ctx
                        .core
                        .text
                        .and_then(|text| ctx.ancestor_args(class, &[], text))
                        .is_some()
                        || ctx.core.char_value == Some(class))
            };
            let heap_ok = ctx.is_heap(a) && ctx.is_heap(b) && !excluded(a) && !excluded(b);
            if !heap_ok || !(ctx.is_subtype(a, b) || ctx.is_subtype(b, a)) {
                return Err(fail(format!(
                    "reference equality needs related object types, found {a} and {b}"
                )));
            }
            push(state, TY_BOOL)?;
        }
        Instr::Call(callee) => {
            let sig = &module.funcs[*callee as usize];
            charge_row(&sig.row)?;
            pop_args(state, &sig.params)?;
            push(state, sig.ret)?;
        }
        Instr::CallG { func: callee, app } => {
            let sig = &module.funcs[*callee as usize];
            let app = &module.apps[*app as usize];
            let row = ctx.row_subst(&sig.row, &app.rows);
            charge_row(&row)?;
            let params: Vec<u32> = sig
                .params
                .iter()
                .map(|p| ctx.subst(*p, &app.types, &app.rows))
                .collect();
            pop_args(state, &params)?;
            let ret = ctx.subst(sig.ret, &app.types, &app.rows);
            push(state, ret)?;
        }
        Instr::CallVirtual { selector, argc } => {
            let argc = *argc as usize;
            if state.stack.len() < argc + 1 {
                return Err(fail("virtual call on a short stack".to_string()));
            }
            let recv_ty = state.stack[state.stack.len() - 1 - argc];
            let class = match ctx.ty(recv_ty) {
                BcType::Class(c) => c,
                BcType::Int => ctx.core.int.ok_or_else(|| {
                    fail("an Int method call needs the Int core role".to_string())
                })?,
                BcType::Bool => ctx.core.boolean.ok_or_else(|| {
                    fail("a Bool method call needs the Bool core role".to_string())
                })?,
                BcType::Str => ctx.core.string.ok_or_else(|| {
                    fail("a String method call needs the String core role".to_string())
                })?,
                BcType::Bytes => ctx.core.bytes.ok_or_else(|| {
                    fail("a Bytes method call needs the Bytes core role".to_string())
                })?,
                _ => {
                    return Err(fail(format!(
                        "virtual call receiver type {recv_ty} needs the generic form \
                         or is not a class"
                    )));
                }
            };
            let target = ctx
                .find_method(class, *selector)
                .ok_or_else(|| fail(format!("selector {selector} is not a class method")))?;
            let sig = &module.funcs[target as usize];
            if sig.type_params != 0 || sig.effect_params != 0 {
                return Err(fail(
                    "a generic method call needs a type application".to_string(),
                ));
            }
            charge_row(&sig.row)?;
            if sig.params.len() != argc + 1 {
                return Err(fail("virtual call argument count mismatch".to_string()));
            }
            pop_args(state, &sig.params[1..])?;
            pop_expect(state, sig.params[0])?;
            push(state, sig.ret)?;
        }
        Instr::CallVirtualG {
            selector,
            argc,
            app,
        } => {
            let argc = *argc as usize;
            if state.stack.len() < argc + 1 {
                return Err(fail("virtual call on a short stack".to_string()));
            }
            let recv_ty = state.stack[state.stack.len() - 1 - argc];
            let Some((class, class_args)) = ctx.as_instance(recv_ty) else {
                return Err(fail(format!(
                    "virtual call receiver type {recv_ty} is not a class"
                )));
            };
            let target = ctx
                .find_method(class, *selector)
                .ok_or_else(|| fail(format!("selector {selector} is not a class method")))?;
            let sig = &module.funcs[target as usize];
            let app = &module.apps[*app as usize];
            // The declaring class may be a generic ancestor. Its type
            // arguments come from the class table, not from the call
            // site, so no application can forge them.
            let owner = ctx
                .method_owner(class, *selector)
                .ok_or_else(|| fail(format!("selector {selector} is not a class method")))?;
            let mut targs = ctx
                .ancestor_args(class, &class_args, owner)
                .ok_or_else(|| fail("the method owner is not an ancestor".to_string()))?;
            targs.extend_from_slice(&app.types);
            if sig.type_params as usize != targs.len()
                || sig.effect_params as usize != app.rows.len()
            {
                return Err(fail(
                    "virtual call type application arity mismatch".to_string(),
                ));
            }
            if !ctx.type_arguments_meet_bounds(
                &targs,
                &app.rows,
                &module.func_bounds[target as usize],
                &module.func_bounds[fidx as usize],
            ) {
                return Err(fail(
                    "a virtual call type argument does not meet its interface bounds".to_string(),
                ));
            }
            let row = ctx.row_subst(&sig.row, &app.rows);
            charge_row(&row)?;
            if sig.params.len() != argc + 1 {
                return Err(fail("virtual call argument count mismatch".to_string()));
            }
            let params: Vec<u32> = sig
                .params
                .iter()
                .map(|p| ctx.subst(*p, &targs, &app.rows))
                .collect();
            pop_args(state, &params[1..])?;
            pop_expect(state, params[0])?;
            let ret = ctx.subst(sig.ret, &targs, &app.rows);
            push(state, ret)?;
        }
        Instr::CallInterface {
            interface,
            method,
            recv_ty: declared_recv_ty,
        } => {
            let contract = &module.interfaces[*interface as usize];
            let requirement = &contract.methods[*method as usize];
            let argc = requirement.params.len();
            if state.stack.len() < argc + 1 {
                return Err(fail("interface call on a short stack".to_string()));
            }
            let recv_ty = state.stack[state.stack.len() - 1 - argc];
            if !ctx.is_subtype(recv_ty, *declared_recv_ty)
                || !ctx.is_subtype(*declared_recv_ty, recv_ty)
            {
                return Err(fail("interface receiver type mismatch".to_string()));
            }
            let application = ctx
                .interface_application(fidx, recv_ty, *interface, 0)
                .ok_or_else(|| fail("interface call lacks a proven receiver bound".to_string()))?;
            let mut types = Vec::with_capacity(application.types.len() + 1);
            types.push(recv_ty);
            types.extend_from_slice(&application.types);
            let row = ctx.row_subst(&requirement.row, &application.rows);
            charge_row(&row)?;
            let params: Vec<u32> = requirement
                .params
                .iter()
                .map(|param| ctx.subst(*param, &types, &application.rows))
                .collect();
            pop_args(state, &params)?;
            pop_expect(state, recv_ty)?;
            let ret = ctx.subst(requirement.ret, &types, &application.rows);
            push(state, ret)?;
        }
        Instr::CallValue { argc } => {
            let argc = *argc as usize;
            if state.stack.len() < argc + 1 {
                return Err(fail("closure call on a short stack".to_string()));
            }
            let callee_ty = state.stack[state.stack.len() - 1 - argc];
            let (params, ret, row) = match ctx.ty(callee_ty) {
                BcType::Fn(params, _, ret, row) | BcType::Callback(params, _, ret, row) => {
                    (params, ret, row)
                }
                _ => {
                    return Err(fail(format!(
                        "closure call target type {callee_ty} is not a function type"
                    )));
                }
            };
            charge_row(&row)?;
            if params.len() != argc {
                return Err(fail("closure call argument count mismatch".to_string()));
            }
            pop_args(state, &params)?;
            pop(state)?;
            push(state, ret)?;
        }
        Instr::MakeClosure { func: f, .. } => {
            let target = &module.funcs[*f as usize];
            pop_args(state, &target.captures)?;
            let fn_ty = BcType::Fn(
                target.params.clone(),
                target.param_muts.clone(),
                target.ret,
                target.row.clone(),
            );
            let idx = {
                let uni = ctx.uni.borrow();
                uni.index.get(&fn_ty).copied()
            };
            let idx = idx.filter(|i| (*i as usize) < module.types.len());
            let idx = idx.ok_or_else(|| {
                fail("the closure function type is not in the type table".to_string())
            })?;
            push(state, idx)?;
        }
        Instr::Extended(ExtendedInstr::MakeCallback { func: f, .. }) => {
            let target = &module.funcs[*f as usize];
            pop_args(state, &target.captures)?;
            let callback_ty = BcType::Callback(
                target.params.clone(),
                target.param_muts.clone(),
                target.ret,
                target.row.clone(),
            );
            let idx = {
                let uni = ctx.uni.borrow();
                uni.index.get(&callback_ty).copied()
            };
            let idx = idx.filter(|i| (*i as usize) < module.types.len());
            let idx = idx.ok_or_else(|| {
                fail("the callback function type is not in the type table".to_string())
            })?;
            push(state, idx)?;
        }
        Instr::Extended(ExtendedInstr::FunctionCode { func: target }) => {
            let target = &module.funcs[*target as usize];
            let input = if target.params.is_empty() {
                TY_UNIT
            } else {
                ctx.intern(BcType::Tuple(target.params.clone()))
            };
            let function_code = ctx.core.function_code.ok_or_else(|| {
                fail(
                    "the module does not carry the pinned core FunctionCode definition".to_string(),
                )
            })?;
            let result = ctx.intern(BcType::Inst(function_code, vec![input, target.ret]));
            push(state, result)?;
        }
        Instr::Extended(ExtendedInstr::ClassCode { .. }) => {
            let class_code = ctx
                .plain_inst(ctx.core.class_code, "ClassCode")
                .map_err(&fail)?;
            push(state, class_code)?;
        }
        Instr::Extended(ExtendedInstr::CodeSource { ty }) => {
            let code = pop(state)?;
            let Some((class, _)) = ctx.as_instance(code) else {
                return Err(fail("code source needs a portable definition".to_string()));
            };
            if Some(class) != ctx.core.function_code && Some(class) != ctx.core.class_code {
                return Err(fail(
                    "code source needs FunctionCode or ClassCode".to_string(),
                ));
            }
            let source = ctx
                .plain_inst(ctx.core.definition_source, "DefinitionSource")
                .map_err(&fail)?;
            let found = ctx
                .option_arg(*ty)
                .ok_or_else(|| fail(format!("type {ty} is not pinned Option")))?;
            if found != source {
                return Err(fail(
                    "code source result must be Option[DefinitionSource]".to_string(),
                ));
            }
            push(state, *ty)?;
        }
        Instr::Extended(ExtendedInstr::CodeDefinition) => {
            let code = pop(state)?;
            let Some((class, _)) = ctx.as_instance(code) else {
                return Err(fail("code definition needs portable code".to_string()));
            };
            if Some(class) != ctx.core.function_code && Some(class) != ctx.core.class_code {
                return Err(fail(
                    "code definition needs FunctionCode or ClassCode".to_string(),
                ));
            }
            let definition = ctx
                .plain_inst(ctx.core.definition_spec, "DefinitionSpec")
                .map_err(&fail)?;
            push(state, definition)?;
        }
        Instr::Extended(ExtendedInstr::FaultSite { ty }) => {
            let fault = pop(state)?;
            if ctx.ty(fault) != BcType::Fault {
                return Err(fail("fault site needs a Fault value".to_string()));
            }
            let location = ctx
                .plain_inst(ctx.core.code_location, "CodeLocation")
                .map_err(&fail)?;
            let found = ctx
                .option_arg(*ty)
                .ok_or_else(|| fail(format!("type {ty} is not pinned Option")))?;
            if found != location {
                return Err(fail(
                    "fault site result must be Option[CodeLocation]".to_string(),
                ));
            }
            push(state, *ty)?;
        }
        Instr::Extended(ExtendedInstr::FaultTrace { ty }) => {
            let fault = pop(state)?;
            if ctx.ty(fault) != BcType::Fault {
                return Err(fail("fault trace needs a Fault value".to_string()));
            }
            let location = ctx
                .plain_inst(ctx.core.code_location, "CodeLocation")
                .map_err(&fail)?;
            if ctx.ty(*ty) != BcType::List(location) {
                return Err(fail(
                    "fault trace result must be List[CodeLocation]".to_string(),
                ));
            }
            push(state, *ty)?;
        }
        Instr::Extended(ExtendedInstr::AsCallback) => {
            let function = pop(state)?;
            let BcType::Fn(params, muts, ret, row) = ctx.ty(function) else {
                return Err(fail(
                    "callback conversion needs a function value".to_string(),
                ));
            };
            let callback_ty = BcType::Callback(params, muts, ret, row);
            let idx = {
                let uni = ctx.uni.borrow();
                uni.index.get(&callback_ty).copied()
            };
            let idx = idx.filter(|i| (*i as usize) < module.types.len());
            let idx =
                idx.ok_or_else(|| fail("the callback type is not in the type table".to_string()))?;
            push(state, idx)?;
        }
        Instr::LoadCapture(idx) => {
            let ty = func.captures[*idx as usize];
            push(state, ty)?;
        }
        Instr::New(class) => {
            if ctx.is_native_core_class(*class) {
                return Err(fail("New cannot allocate a native core class".to_string()));
            }
            let ty = ctx.class_ty[*class as usize]
                .ok_or_else(|| fail("the class type is not in the type table".to_string()))?;
            push(state, ty)?;
        }
        Instr::NewG { class, app } => {
            if ctx.is_native_core_class(*class) {
                return Err(fail("NewG cannot allocate a native core class".to_string()));
            }
            let app = &module.apps[*app as usize];
            let ty = ctx.intern(BcType::Inst(*class, app.types.clone()));
            push(state, ty)?;
        }
        Instr::LoadField(field) => {
            let recv = pop(state)?;
            let Some((class, class_args)) = ctx.as_instance(recv) else {
                return Err(fail(format!("field load on non-class type {recv}")));
            };
            if Some(class) == ctx.core.option_some {
                return Err(fail(
                    "native Option payloads require OptionPayload".to_string(),
                ));
            }
            let fields = &module.classes[class as usize].fields;
            let (_, fty) = fields
                .get(*field as usize)
                .ok_or_else(|| fail("field index out of range".to_string()))?;
            let fty = ctx.subst(*fty, &class_args, &[]);
            push(state, fty)?;
        }
        Instr::StoreField(field) => {
            let value = pop(state)?;
            let recv = pop(state)?;
            let Some((class, class_args)) = ctx.as_instance(recv) else {
                return Err(fail(format!("field store on non-class type {recv}")));
            };
            if Some(class) == ctx.core.option_some {
                return Err(fail("native Option payloads cannot be stored".to_string()));
            }
            let fields = &module.classes[class as usize].fields;
            let (_, fty) = fields
                .get(*field as usize)
                .ok_or_else(|| fail("field index out of range".to_string()))?;
            let fty = ctx.subst(*fty, &class_args, &[]);
            if !ctx.is_subtype(value, fty) {
                return Err(fail(format!(
                    "field store expects type {fty}, found type {value}"
                )));
            }
        }
        Instr::TupleNew { ty, count } => {
            let elems = match ctx.ty(*ty) {
                BcType::Tuple(elems) => elems,
                _ => return Err(fail(format!("expected a tuple type, found type {ty}"))),
            };
            if elems.len() != *count as usize {
                return Err(fail("tuple arity does not match its type".to_string()));
            }
            for want in elems.iter().rev() {
                pop_expect(state, *want)?;
            }
            push(state, *ty)?;
        }
        Instr::TupleGet(index) => {
            let t = pop(state)?;
            let elems = match ctx.ty(t) {
                BcType::Tuple(elems) => elems,
                _ => return Err(fail(format!("tuple read on non-tuple type {t}"))),
            };
            let elem = elems
                .get(*index as usize)
                .ok_or_else(|| fail("tuple index out of range".to_string()))?;
            push(state, *elem)?;
        }
        Instr::IsType(ty) | Instr::CastType(ty) => {
            let value = pop(state)?;
            let Some((vc, va)) = ctx.as_instance(value) else {
                return Err(fail(format!("type test on non-instance type {value}")));
            };
            let Some((tc, ta)) = ctx.as_instance(*ty) else {
                return Err(fail(format!(
                    "type test target {ty} is not an instance type"
                )));
            };
            // Sibling enum cases share their family parent, so a test
            // between them is legal and false at run time. The
            // exhaustiveness backstop emits such tests on flow-narrowed
            // values. Classes without a common ancestor stay rejected.
            if ctx.common_ancestor(vc, tc).is_none() {
                return Err(fail("type test between unrelated classes".to_string()));
            }
            // Class arguments are invariant, and every legal nominal
            // relation in this slice keeps the argument vector. A test
            // that changes an argument would forge a generic type.
            if va != ta {
                return Err(fail("type test changes the generic arguments".to_string()));
            }
            match instr {
                Instr::IsType(_) => push(state, TY_BOOL)?,
                _ => push(state, *ty)?,
            }
        }
        Instr::ListNew { ty, count } => {
            let elem = as_list(*ty)?;
            for _ in 0..*count {
                pop_expect(state, elem)?;
            }
            push(state, *ty)?;
        }
        Instr::ListLen => {
            let l = pop(state)?;
            as_list(l)?;
            push(state, TY_INT)?;
        }
        Instr::ListAt => {
            pop_expect(state, TY_INT)?;
            let l = pop(state)?;
            let elem = as_list(l)?;
            push(state, elem)?;
        }
        Instr::ListPush => {
            let value = pop(state)?;
            let l = pop(state)?;
            let elem = as_list(l)?;
            if !ctx.is_subtype(value, elem) {
                return Err(fail(format!(
                    "list push expects element type {elem}, found type {value}"
                )));
            }
            push(state, TY_UNIT)?;
        }
        Instr::MapNew { ty, count } => {
            let (k, v) = as_map(*ty)?;
            for _ in 0..*count {
                pop_expect(state, v)?;
                pop_expect(state, k)?;
            }
            push(state, *ty)?;
        }
        Instr::MapLen => {
            let m = pop(state)?;
            as_map(m)?;
            push(state, TY_INT)?;
        }
        Instr::MapHas => {
            let key = pop(state)?;
            let m = pop(state)?;
            let (k, _) = as_map(m)?;
            if !ctx.accepts_map_query_key(key, k) {
                return Err(fail(format!("map key expects type {k}, found type {key}")));
            }
            push(state, TY_BOOL)?;
        }
        Instr::MapAt => {
            let key = pop(state)?;
            let m = pop(state)?;
            let (k, v) = as_map(m)?;
            if !ctx.accepts_map_query_key(key, k) {
                return Err(fail(format!("map key expects type {k}, found type {key}")));
            }
            push(state, v)?;
        }
        Instr::MapPut { ty, discard } => {
            let value = pop(state)?;
            let key = pop(state)?;
            let m = pop(state)?;
            let (k, v) = as_map(m)?;
            if !ctx.is_subtype(key, k) || !ctx.is_subtype(value, v) {
                return Err(fail("map put entry types do not match".to_string()));
            }
            let want = ctx
                .option_arg(*ty)
                .ok_or_else(|| fail(format!("type {ty} is not pinned Option")))?;
            if want != v {
                return Err(fail("map put option type does not match".to_string()));
            }
            if !discard {
                push(state, *ty)?;
            }
        }
        Instr::Extended(ExtendedInstr::OptionSome { ty }) => {
            let want = ctx
                .option_arg(*ty)
                .ok_or_else(|| fail(format!("type {ty} is not pinned Option")))?;
            let value = pop(state)?;
            if !ctx.is_subtype(value, want) {
                return Err(fail(format!(
                    "Option payload expects type {want}, found type {value}"
                )));
            }
            push(state, *ty)?;
        }
        Instr::Extended(ExtendedInstr::OptionNone { ty }) => {
            ctx.option_arg(*ty)
                .ok_or_else(|| fail(format!("type {ty} is not pinned Option")))?;
            push(state, *ty)?;
        }
        Instr::Extended(ExtendedInstr::OptionPayload { ty }) => {
            let option = pop(state)?;
            if !ctx.is_subtype(option, *ty) || !ctx.is_subtype(*ty, option) {
                return Err(fail("OptionPayload type mismatch".to_string()));
            }
            let Some((class, args)) = ctx.as_instance(*ty) else {
                return Err(fail("OptionPayload needs Option.Some".to_string()));
            };
            if Some(class) != ctx.core.option_some || args.len() != 1 {
                return Err(fail("OptionPayload needs Option.Some".to_string()));
            }
            push(state, args[0])?;
        }
        Instr::Extended(ExtendedInstr::ListGet { ty }) => {
            let want = ctx
                .option_arg(*ty)
                .ok_or_else(|| fail(format!("type {ty} is not pinned Option")))?;
            pop_expect(state, TY_INT)?;
            let list = pop(state)?;
            let found = as_list(list)?;
            if found != want {
                return Err(fail("list get option type does not match".to_string()));
            }
            push(state, *ty)?;
        }
        Instr::Extended(ExtendedInstr::MapGet { ty }) => {
            let want = ctx
                .option_arg(*ty)
                .ok_or_else(|| fail(format!("type {ty} is not pinned Option")))?;
            let key = pop(state)?;
            let map = pop(state)?;
            let (expected_key, found) = as_map(map)?;
            if !ctx.accepts_map_query_key(key, expected_key) {
                return Err(fail(format!(
                    "map key expects type {expected_key}, found type {key}"
                )));
            }
            if found != want {
                return Err(fail("map get option type does not match".to_string()));
            }
            push(state, *ty)?;
        }
        Instr::Extended(ExtendedInstr::ListEpoch) => {
            let list = pop(state)?;
            as_list(list)?;
            push(state, TY_INT)?;
        }
        Instr::Extended(ExtendedInstr::ListIterLen) => {
            pop_expect(state, TY_INT)?;
            let list = pop(state)?;
            as_list(list)?;
            push(state, TY_INT)?;
        }
        Instr::Extended(ExtendedInstr::MapEpoch) => {
            let map = pop(state)?;
            as_map(map)?;
            push(state, TY_INT)?;
        }
        Instr::Extended(ExtendedInstr::MapIterLen) => {
            pop_expect(state, TY_INT)?;
            let map = pop(state)?;
            as_map(map)?;
            push(state, TY_INT)?;
        }
        Instr::Extended(ExtendedInstr::MapKeyAt) => {
            pop_expect(state, TY_INT)?;
            let map = pop(state)?;
            let (key, _) = as_map(map)?;
            push(state, key)?;
        }
        Instr::Extended(ExtendedInstr::MapValueAt) => {
            pop_expect(state, TY_INT)?;
            let map = pop(state)?;
            let (_, value) = as_map(map)?;
            push(state, value)?;
        }
        Instr::Extended(ExtendedInstr::ListCapacity) => {
            let list = pop(state)?;
            as_list(list)?;
            push(state, TY_INT)?;
        }
        Instr::Extended(ExtendedInstr::ListSet) => {
            let value = pop(state)?;
            pop_expect(state, TY_INT)?;
            let list = pop(state)?;
            let element = as_list(list)?;
            if !ctx.is_subtype(value, element) {
                return Err(fail("list set element type does not match".to_string()));
            }
            push(state, TY_UNIT)?;
        }
        Instr::Extended(ExtendedInstr::ListPop { ty }) => {
            let list = pop(state)?;
            let element = as_list(list)?;
            let want = ctx
                .option_arg(*ty)
                .ok_or_else(|| fail(format!("type {ty} is not pinned Option")))?;
            if want != element {
                return Err(fail("list pop option type does not match".to_string()));
            }
            push(state, *ty)?;
        }
        Instr::Extended(ExtendedInstr::ListInsert) => {
            let value = pop(state)?;
            pop_expect(state, TY_INT)?;
            let list = pop(state)?;
            let element = as_list(list)?;
            if !ctx.is_subtype(value, element) {
                return Err(fail("list insert element type does not match".to_string()));
            }
            push(state, TY_UNIT)?;
        }
        Instr::Extended(ExtendedInstr::ListRemove)
        | Instr::Extended(ExtendedInstr::ListSwapRemove) => {
            pop_expect(state, TY_INT)?;
            let list = pop(state)?;
            let element = as_list(list)?;
            push(state, element)?;
        }
        Instr::Extended(ExtendedInstr::ListReserve)
        | Instr::Extended(ExtendedInstr::ListTruncate) => {
            pop_expect(state, TY_INT)?;
            let list = pop(state)?;
            as_list(list)?;
            push(state, TY_UNIT)?;
        }
        Instr::Extended(ExtendedInstr::ListContains) => {
            let value = pop(state)?;
            let list = pop(state)?;
            let element = as_list(list)?;
            if !ctx.is_subtype(value, element) {
                return Err(fail(
                    "list contains element type does not match".to_string(),
                ));
            }
            push(state, TY_BOOL)?;
        }
        Instr::Extended(ExtendedInstr::ListReorder) => {
            let list = pop(state)?;
            as_list(list)?;
            push(state, TY_UNIT)?;
        }
        Instr::Extended(ExtendedInstr::MapRemove { ty }) => {
            let key = pop(state)?;
            let map = pop(state)?;
            let (expected_key, value) = as_map(map)?;
            if !ctx.is_subtype(key, expected_key) {
                return Err(fail("map remove key type does not match".to_string()));
            }
            let want = ctx
                .option_arg(*ty)
                .ok_or_else(|| fail(format!("type {ty} is not pinned Option")))?;
            if want != value {
                return Err(fail("map remove option type does not match".to_string()));
            }
            push(state, *ty)?;
        }
        Instr::Extended(ExtendedInstr::MapClear) => {
            let map = pop(state)?;
            as_map(map)?;
            push(state, TY_UNIT)?;
        }
        Instr::Extended(ExtendedInstr::MapReserve) => {
            pop_expect(state, TY_INT)?;
            let map = pop(state)?;
            as_map(map)?;
            push(state, TY_UNIT)?;
        }
        Instr::Extended(ExtendedInstr::CallSlot { slot, app }) => {
            let contract = match &module.slots[*slot as usize].contract {
                SlotContract::Function(contract) | SlotContract::Method(contract) => contract,
                _ => unreachable!("the structural pass checked the slot kind"),
            };
            let (types, rows): (&[u32], &[Vec<BcRow>]) = if *app == lm_bytecode::NO_APP {
                (&[], &[])
            } else {
                let application = &module.apps[*app as usize];
                (&application.types, &application.rows)
            };
            let row = ctx.row_subst(&contract.row, rows);
            charge_row(&row)?;
            let params: Vec<u32> = contract
                .params
                .iter()
                .map(|param| ctx.subst(*param, types, rows))
                .collect();
            pop_args(state, &params)?;
            push(state, ctx.subst(contract.ret, types, rows))?;
        }
        Instr::Extended(ExtendedInstr::NewSlot { slot, app }) => {
            let SlotContract::Class { constructor, .. } = &module.slots[*slot as usize].contract
            else {
                unreachable!("the structural pass checked the slot kind");
            };
            let (types, rows): (&[u32], &[Vec<BcRow>]) = if *app == lm_bytecode::NO_APP {
                (&[], &[])
            } else {
                let application = &module.apps[*app as usize];
                (&application.types, &application.rows)
            };
            let row = ctx.row_subst(&constructor.row, rows);
            charge_row(&row)?;
            let params: Vec<u32> = constructor
                .params
                .iter()
                .map(|param| ctx.subst(*param, types, rows))
                .collect();
            pop_args(state, &params)?;
            push(state, ctx.subst(constructor.ret, types, rows))?;
        }
        Instr::Extended(ExtendedInstr::LoadSlot { slot }) => {
            let SlotContract::Value { ty } = &module.slots[*slot as usize].contract else {
                unreachable!("the structural pass checked the slot kind");
            };
            push(state, *ty)?;
        }
        Instr::Extended(ExtendedInstr::SendSlot { slot }) => {
            let SlotContract::Process { message, .. } = &module.slots[*slot as usize].contract
            else {
                unreachable!("the structural pass checked the slot kind");
            };
            let name = lm_abi::op_name(lm_abi::OP_PROC_SEND);
            if !ctx.row_has_name(&func.row, &name) {
                return Err(fail(format!(
                    "the send through a slot is not inside the claimed `{name}` row"
                )));
            }
            pop_expect(state, *message)?;
            let result = ctx
                .plain_inst(ctx.core.send_result, "SendResult")
                .map_err(&fail)?;
            push(state, result)?;
        }
        Instr::Extended(ExtendedInstr::SyntaxTreeRoot) => {
            let tree = ctx
                .plain_inst(ctx.core.syntax_tree, "SyntaxTree")
                .map_err(&fail)?;
            let result = ctx
                .plain_inst(ctx.core.syntax_node, "SyntaxNode")
                .map_err(&fail)?;
            pop_expect(state, tree)?;
            push(state, result)?;
        }
        Instr::Extended(ExtendedInstr::SyntaxKind)
        | Instr::Extended(ExtendedInstr::SyntaxCategory)
        | Instr::Extended(ExtendedInstr::SyntaxRangeStart)
        | Instr::Extended(ExtendedInstr::SyntaxRangeEnd) => {
            let element = ctx
                .plain_inst(ctx.core.syntax_element, "SyntaxElement")
                .map_err(&fail)?;
            pop_expect(state, element)?;
            push(state, TY_INT)?;
        }
        Instr::Extended(ExtendedInstr::SyntaxText) => {
            let element = ctx
                .plain_inst(ctx.core.syntax_element, "SyntaxElement")
                .map_err(&fail)?;
            let text = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            pop_expect(state, element)?;
            push(state, text)?;
        }
        Instr::Extended(ExtendedInstr::SyntaxChildren)
        | Instr::Extended(ExtendedInstr::SyntaxDetach) => {
            let element = ctx
                .plain_inst(ctx.core.syntax_element, "SyntaxElement")
                .map_err(&fail)?;
            pop_expect(state, element)?;
            if matches!(instr, Instr::Extended(ExtendedInstr::SyntaxChildren)) {
                push(state, ctx.intern(BcType::List(element)))?;
            } else {
                push(state, element)?;
            }
        }
        Instr::Extended(ExtendedInstr::DynPack { ty }) => {
            let value = pop(state)?;
            if !ctx.is_subtype(value, *ty) {
                return Err(fail(format!(
                    "dynamic package expects type {ty}, found type {value}"
                )));
            }
            let package = ctx
                .plain_inst(ctx.core.dyn_value, "DynValue")
                .map_err(&fail)?;
            push(state, package)?;
        }
        Instr::Extended(ExtendedInstr::DynRender) => {
            let package = ctx
                .plain_inst(ctx.core.dyn_value, "DynValue")
                .map_err(&fail)?;
            pop_expect(state, package)?;
            push(state, TY_STR)?;
        }
        Instr::Extended(ExtendedInstr::SyntaxBuildToken)
        | Instr::Extended(ExtendedInstr::SyntaxBuildTrivia) => {
            let builder = ctx
                .plain_inst(ctx.core.syntax_builder, "SyntaxBuilder")
                .map_err(&fail)?;
            let result = if matches!(instr, Instr::Extended(ExtendedInstr::SyntaxBuildToken)) {
                ctx.plain_inst(ctx.core.syntax_token, "SyntaxToken")
                    .map_err(&fail)?
            } else {
                ctx.plain_inst(ctx.core.syntax_trivia, "SyntaxTrivia")
                    .map_err(&fail)?
            };
            pop_expect(state, TY_STR)?;
            pop_expect(state, TY_INT)?;
            pop_expect(state, builder)?;
            push(state, result)?;
        }
        Instr::Extended(ExtendedInstr::SyntaxBuildNode) => {
            let builder = ctx
                .plain_inst(ctx.core.syntax_builder, "SyntaxBuilder")
                .map_err(&fail)?;
            let element = ctx
                .plain_inst(ctx.core.syntax_element, "SyntaxElement")
                .map_err(&fail)?;
            let result = ctx
                .plain_inst(ctx.core.syntax_node, "SyntaxNode")
                .map_err(&fail)?;
            let children = ctx.intern(BcType::List(element));
            pop_expect(state, children)?;
            pop_expect(state, TY_INT)?;
            pop_expect(state, builder)?;
            push(state, result)?;
        }
        Instr::Extended(ExtendedInstr::SyntaxToTree) => {
            let node = ctx
                .plain_inst(ctx.core.syntax_node, "SyntaxNode")
                .map_err(&fail)?;
            let tree = ctx
                .plain_inst(ctx.core.syntax_tree, "SyntaxTree")
                .map_err(&fail)?;
            pop_expect(state, node)?;
            push(state, tree)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::SbNew) => {
            let class = ctx
                .core
                .string_builder
                .ok_or_else(|| fail("StringBuilder needs its core role".to_string()))?;
            let idx = ctx.intern(BcType::Class(class));
            push(state, idx)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::SbAppendStr)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendInt)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendBool)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendChar) => {
            let want = match instr {
                Instr::Native(lm_bytecode::NativeInstr::SbAppendStr) => {
                    ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?
                }
                Instr::Native(lm_bytecode::NativeInstr::SbAppendInt) => TY_INT,
                Instr::Native(lm_bytecode::NativeInstr::SbAppendBool) => TY_BOOL,
                Instr::Native(lm_bytecode::NativeInstr::SbAppendChar) => {
                    ctx.plain_inst(ctx.core.char_value, "Char").map_err(&fail)?
                }
                _ => unreachable!("the builder append group is complete"),
            };
            pop_expect(state, want)?;
            let class = ctx
                .core
                .string_builder
                .ok_or_else(|| fail("StringBuilder needs its core role".to_string()))?;
            let builder = ctx.intern(BcType::Class(class));
            let sb = pop_expect(state, builder)?;
            push(state, sb)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::SbBuild)
        | Instr::Native(lm_bytecode::NativeInstr::SbFinish) => {
            let class = ctx
                .core
                .string_builder
                .ok_or_else(|| fail("StringBuilder needs its core role".to_string()))?;
            let builder = ctx.intern(BcType::Class(class));
            pop_expect(state, builder)?;
            push(state, TY_STR)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::SbLen)
        | Instr::Native(lm_bytecode::NativeInstr::SbByteLen) => {
            let class = ctx
                .core
                .string_builder
                .ok_or_else(|| fail("StringBuilder needs its core role".to_string()))?;
            let builder = ctx.intern(BcType::Class(class));
            pop_expect(state, builder)?;
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::SbClear) => {
            let class = ctx
                .core
                .string_builder
                .ok_or_else(|| fail("StringBuilder needs its core role".to_string()))?;
            let builder = ctx.intern(BcType::Class(class));
            pop_expect(state, builder)?;
            push(state, builder)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbNew) => {
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let idx = ctx.intern(BcType::Class(class));
            push(state, idx)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbAppend) => {
            pop_expect(state, TY_INT)?;
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            let bb = pop_expect(state, buffer)?;
            push(state, bb)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbLen) => {
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            pop_expect(state, buffer)?;
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbBuild)
        | Instr::Native(lm_bytecode::NativeInstr::BbFinish) => {
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            pop_expect(state, buffer)?;
            let bytes = ctx.intern(BcType::Bytes);
            push(state, bytes)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbExtend) => {
            let bytes = ctx.intern(BcType::Bytes);
            pop_expect(state, bytes)?;
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            pop_expect(state, buffer)?;
            push(state, buffer)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbReserve) => {
            pop_expect(state, TY_INT)?;
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            pop_expect(state, buffer)?;
            push(state, buffer)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbClear) => {
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            pop_expect(state, buffer)?;
            push(state, buffer)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbAt) => {
            pop_expect(state, TY_INT)?;
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            pop_expect(state, buffer)?;
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbFindFrom) => {
            pop_expect(state, TY_INT)?;
            let bytes = ctx.intern(BcType::Bytes);
            pop_expect(state, bytes)?;
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            pop_expect(state, buffer)?;
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesNew) => {
            pop_expect(state, TY_STR)?;
            let idx = {
                let uni = ctx.uni.borrow();
                uni.index.get(&BcType::Bytes).copied()
            };
            let idx = idx.ok_or_else(|| fail("Bytes is not in the type table".to_string()))?;
            push(state, idx)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesLen) => {
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("len on non-bytes type {bytes}")));
            }
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesText) => {
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("text on non-bytes type {bytes}")));
            }
            push(state, TY_STR)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesTextView) => {
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("text view on non-bytes type {bytes}")));
            }
            let view = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            push(state, view)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesAt)
        | Instr::Native(lm_bytecode::NativeInstr::BytesGet) => {
            pop_expect(state, TY_INT)?;
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("index on non-bytes type {bytes}")));
            }
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesSlice) => {
            pop_expect(state, TY_INT)?;
            pop_expect(state, TY_INT)?;
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("slice on non-bytes type {bytes}")));
            }
            push(state, bytes)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesConcat) => {
            let right = pop(state)?;
            let left = pop(state)?;
            if ctx.ty(left) != BcType::Bytes || ctx.ty(right) != BcType::Bytes {
                return Err(fail("concat needs two Bytes values".to_string()));
            }
            push(state, left)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesStartsWith) => {
            let right = pop(state)?;
            let left = pop(state)?;
            if ctx.ty(left) != BcType::Bytes || ctx.ty(right) != BcType::Bytes {
                return Err(fail("starts_with needs two Bytes values".to_string()));
            }
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesFindIndex) => {
            let right = pop(state)?;
            let left = pop(state)?;
            if ctx.ty(left) != BcType::Bytes || ctx.ty(right) != BcType::Bytes {
                return Err(fail("find needs two Bytes values".to_string()));
            }
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesHex) => {
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("hex on non-bytes type {bytes}")));
            }
            push(state, TY_STR)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesIsUtf8) => {
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("UTF-8 test on non-bytes type {bytes}")));
            }
            push(state, TY_BOOL)?;
        }
        Instr::Native(
            lm_bytecode::NativeInstr::EqBytes
            | lm_bytecode::NativeInstr::NeBytes
            | lm_bytecode::NativeInstr::LtBytes
            | lm_bytecode::NativeInstr::LeBytes
            | lm_bytecode::NativeInstr::GtBytes
            | lm_bytecode::NativeInstr::GeBytes,
        ) => {
            let right = pop(state)?;
            let left = pop(state)?;
            if ctx.ty(left) != BcType::Bytes || ctx.ty(right) != BcType::Bytes {
                return Err(fail("Bytes comparison needs two Bytes values".to_string()));
            }
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesCompact) => {
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("compact on non-bytes type {bytes}")));
            }
            push(state, bytes)?;
        }
        Instr::Freeze => {
            let ty = pop(state)?;
            if !ctx.is_heap(ty) {
                return Err(fail(format!("freeze on non-object type {ty}")));
            }
            push(state, ty)?;
        }
        Instr::Digest { ty } => {
            let found = pop(state)?;
            if found != *ty {
                return Err(fail(format!(
                    "digest states type {ty}, but its operand has type {found}"
                )));
            }
            if !ctx.is_heap(*ty) {
                return Err(fail(format!("digest on non-object type {ty}")));
            }
            let idx = {
                let uni = ctx.uni.borrow();
                uni.index.get(&BcType::Digest).copied()
            };
            let idx = idx.ok_or_else(|| fail("Digest is not in the type table".to_string()))?;
            push(state, idx)?;
        }
        Instr::EqDigest | Instr::NeDigest => {
            let b = pop(state)?;
            let a = pop(state)?;
            if ctx.ty(a) != BcType::Digest || ctx.ty(b) != BcType::Digest {
                return Err(fail(format!(
                    "digest comparison on non-digest types {a} and {b}"
                )));
            }
            push(state, TY_BOOL)?;
        }
        Instr::Jump(target) => {
            edge(*target as usize, state.clone())?;
        }
        Instr::JumpIfFalse(target) | Instr::JumpIfTrue(target) => {
            pop_expect(state, TY_BOOL)?;
            edge(*target as usize, state.clone())?;
        }
        Instr::Return => {
            pop_expect(state, func.ret)?;
        }
        Instr::Perform { op, reply_ty, .. } => {
            let reply_ty = *reply_ty;
            let op = *op;
            let name = lm_abi::op_name(op);
            if !ctx.row_has_name(&func.row, &name) {
                return Err(fail(format!(
                    "the perform of `{name}` is not inside the claimed row"
                )));
            }
            let def = lm_abi::op(op);
            match def.kind {
                lm_abi::OpKind::Fixed => {
                    for want in def.params.iter().rev() {
                        let want = ctx.abi_ty(*want).map_err(&fail)?;
                        pop_expect(state, want)?;
                    }
                    let reply = ctx.abi_ty(def.reply).map_err(&fail)?;
                    push(state, reply)?;
                }
                lm_abi::OpKind::VmControl => {
                    let pop_run = |state: &mut State| -> Result<u32, VerifyError> {
                        let v = pop(state)?;
                        match ctx.ty(v) {
                            BcType::Run(t) => Ok(t),
                            _ => Err(fail(format!(
                                "`{name}` needs an active Run receiver, found type {v}"
                            ))),
                        }
                    };
                    match op {
                        lm_abi::OP_VM_NEW => {
                            let vm = ctx.intern(BcType::Vm);
                            push(state, vm)?;
                        }
                        lm_abi::OP_VM_ARTIFACT => {
                            pop_expect(state, ctx.intern(BcType::Bytes))?;
                            let artifact = ctx
                                .plain_inst(ctx.core.artifact, "Artifact")
                                .map_err(&fail)?;
                            push(state, artifact)?;
                        }
                        lm_abi::OP_COMPILER_VERIFY => {
                            let artifact = ctx
                                .plain_inst(ctx.core.artifact, "Artifact")
                                .map_err(&fail)?;
                            pop_expect(state, artifact)?;
                            let verified = ctx
                                .plain_inst(ctx.core.verified_module, "VerifiedModule")
                                .map_err(&fail)?;
                            let error = ctx
                                .plain_inst(ctx.core.code_error, "CodeError")
                                .map_err(&fail)?;
                            let result = ctx.result_inst(verified, error).map_err(&fail)?;
                            push(state, result)?;
                        }
                        lm_abi::OP_VM_INSTALL | lm_abi::OP_VM_INSTALL_WITH => {
                            if op == lm_abi::OP_VM_INSTALL_WITH {
                                let links = ctx
                                    .plain_inst(ctx.core.link_env, "LinkEnv")
                                    .map_err(&fail)?;
                                pop_expect(state, links)?;
                            }
                            let code = pop(state)?;
                            let code_ty = ctx.ty(code);
                            let installed = if code
                                == ctx
                                    .plain_inst(ctx.core.verified_module, "VerifiedModule")
                                    .map_err(&fail)?
                            {
                                ctx.plain_inst(ctx.core.instance, "Instance")
                                    .map_err(&fail)?
                            } else if let BcType::Inst(class, args) = code_ty.clone() {
                                let function_code = ctx.core.function_code.ok_or_else(|| {
                                    fail(
                                        "the module does not carry the pinned core FunctionCode definition"
                                            .to_string(),
                                    )
                                })?;
                                if class != function_code || args.len() != 2 {
                                    return Err(fail(
                                        "`Vm.Install` has invalid code input".to_string(),
                                    ));
                                }
                                let function_binding =
                                    ctx.core.function_binding.ok_or_else(|| {
                                    fail(
                                        "the module does not carry the pinned core FunctionBinding definition"
                                            .to_string(),
                                    )
                                })?;
                                ctx.intern(BcType::Inst(function_binding, args))
                            } else if code
                                == ctx
                                    .plain_inst(ctx.core.class_code, "ClassCode")
                                    .map_err(&fail)?
                            {
                                ctx.plain_inst(ctx.core.class_binding, "ClassBinding")
                                    .map_err(&fail)?
                            } else if let BcType::Fn(params, muts, ret, _) = code_ty {
                                if muts.iter().any(|marker| *marker) {
                                    return Err(fail(
                                        "`Vm.Install` cannot install a function with a mut parameter"
                                            .to_string(),
                                    ));
                                }
                                let input = if params.is_empty() {
                                    TY_UNIT
                                } else {
                                    ctx.intern(BcType::Tuple(params))
                                };
                                let function_binding =
                                    ctx.core.function_binding.ok_or_else(|| {
                                    fail(
                                        "the module does not carry the pinned core FunctionBinding definition"
                                            .to_string(),
                                    )
                                })?;
                                ctx.intern(BcType::Inst(function_binding, vec![input, ret]))
                            } else {
                                return Err(fail(
                                    "`Vm.Install` has invalid code input".to_string(),
                                ));
                            };
                            pop_expect(state, ctx.intern(BcType::Vm))?;
                            let error = ctx
                                .plain_inst(ctx.core.code_error, "CodeError")
                                .map_err(&fail)?;
                            let result = ctx.result_inst(installed, error).map_err(&fail)?;
                            if reply_ty != result {
                                return Err(fail(
                                    "`Vm.Install` has the wrong result type".to_string(),
                                ));
                            }
                            push(state, reply_ty)?;
                        }
                        lm_abi::OP_VM_MODULE_ENTRY_CODE | lm_abi::OP_VM_MODULE_FUNCTION_CODE => {
                            if op == lm_abi::OP_VM_MODULE_FUNCTION_CODE {
                                pop_expect(state, TY_STR)?;
                            }
                            let verified = ctx
                                .plain_inst(ctx.core.verified_module, "VerifiedModule")
                                .map_err(&fail)?;
                            pop_expect(state, verified)?;
                            let result_class = ctx.core.result.ok_or_else(|| {
                                fail(
                                    "the module does not carry the pinned core Result definition"
                                        .to_string(),
                                )
                            })?;
                            let function_code = ctx.core.function_code.ok_or_else(|| {
                                fail(
                                    "the module does not carry the pinned core FunctionCode definition"
                                        .to_string(),
                                )
                            })?;
                            let error = ctx
                                .plain_inst(ctx.core.code_error, "CodeError")
                                .map_err(&fail)?;
                            let BcType::Inst(found_result, result_args) = ctx.ty(reply_ty) else {
                                return Err(fail(
                                    "a module function lookup needs a Result reply".to_string(),
                                ));
                            };
                            if found_result != result_class
                                || result_args.len() != 2
                                || result_args[1] != error
                            {
                                return Err(fail(
                                    "a module function lookup has the wrong error type".to_string(),
                                ));
                            }
                            let BcType::Inst(found_function, function_args) =
                                ctx.ty(result_args[0])
                            else {
                                return Err(fail(
                                    "a module function lookup needs a FunctionCode result"
                                        .to_string(),
                                ));
                            };
                            if found_function != function_code || function_args.len() != 2 {
                                return Err(fail(
                                    "a module function lookup has the wrong function type"
                                        .to_string(),
                                ));
                            }
                            if !matches!(ctx.ty(function_args[0]), BcType::Unit | BcType::Tuple(_))
                            {
                                return Err(fail(
                                    "a FunctionCode argument view must be unit or a tuple"
                                        .to_string(),
                                ));
                            }
                            push(state, reply_ty)?;
                        }
                        lm_abi::OP_VM_MODULE_CLASS_CODE => {
                            pop_expect(state, TY_STR)?;
                            let verified = ctx
                                .plain_inst(ctx.core.verified_module, "VerifiedModule")
                                .map_err(&fail)?;
                            pop_expect(state, verified)?;
                            let class_code = ctx
                                .plain_inst(ctx.core.class_code, "ClassCode")
                                .map_err(&fail)?;
                            let error = ctx
                                .plain_inst(ctx.core.code_error, "CodeError")
                                .map_err(&fail)?;
                            let result = ctx.result_inst(class_code, error).map_err(&fail)?;
                            if reply_ty != result {
                                return Err(fail(
                                    "a module class lookup has the wrong result type".to_string(),
                                ));
                            }
                            push(state, reply_ty)?;
                        }
                        lm_abi::OP_VM_INSTANCE_ENTRY
                        | lm_abi::OP_VM_INSTANCE_FUNCTION
                        | lm_abi::OP_VM_INSTANCE_ENTRY_BINDING
                        | lm_abi::OP_VM_INSTANCE_FUNCTION_BINDING => {
                            if matches!(
                                op,
                                lm_abi::OP_VM_INSTANCE_FUNCTION
                                    | lm_abi::OP_VM_INSTANCE_FUNCTION_BINDING
                            ) {
                                pop_expect(state, TY_STR)?;
                            }
                            let instance = ctx
                                .plain_inst(ctx.core.instance, "Instance")
                                .map_err(&fail)?;
                            pop_expect(state, instance)?;
                            let Some(result) = ctx.core.result else {
                                return Err(fail(
                                    "the module does not carry the pinned core Result definition"
                                        .to_string(),
                                ));
                            };
                            let function_class = if matches!(
                                op,
                                lm_abi::OP_VM_INSTANCE_ENTRY_BINDING
                                    | lm_abi::OP_VM_INSTANCE_FUNCTION_BINDING
                            ) {
                                ctx.core.function_binding
                            } else {
                                ctx.core.function_def
                            };
                            let Some(function_class) = function_class else {
                                return Err(fail(
                                    "the module does not carry the pinned core function handle definition"
                                        .to_string(),
                                ));
                            };
                            let error = ctx
                                .plain_inst(ctx.core.code_error, "CodeError")
                                .map_err(&fail)?;
                            let BcType::Inst(found_result, result_args) = ctx.ty(reply_ty) else {
                                return Err(fail(
                                    "an instance function lookup needs a Result reply".to_string(),
                                ));
                            };
                            if found_result != result
                                || result_args.len() != 2
                                || result_args[1] != error
                            {
                                return Err(fail(
                                    "an instance function lookup has the wrong error type"
                                        .to_string(),
                                ));
                            }
                            let BcType::Inst(found_function, function_args) =
                                ctx.ty(result_args[0])
                            else {
                                return Err(fail(
                                    "an instance function lookup needs a FunctionDef result"
                                        .to_string(),
                                ));
                            };
                            if found_function != function_class || function_args.len() != 2 {
                                return Err(fail(
                                    "an instance function lookup has the wrong handle type"
                                        .to_string(),
                                ));
                            }
                            if !matches!(ctx.ty(function_args[0]), BcType::Unit | BcType::Tuple(_))
                            {
                                return Err(fail(
                                    "a function argument view must be unit or a tuple".to_string(),
                                ));
                            }
                            push(state, reply_ty)?;
                        }
                        lm_abi::OP_VM_INSTANCE_CLASS | lm_abi::OP_VM_INSTANCE_CLASS_BINDING => {
                            pop_expect(state, TY_STR)?;
                            let instance = ctx
                                .plain_inst(ctx.core.instance, "Instance")
                                .map_err(&fail)?;
                            pop_expect(state, instance)?;
                            let class = if op == lm_abi::OP_VM_INSTANCE_CLASS_BINDING {
                                ctx.plain_inst(ctx.core.class_binding, "ClassBinding")
                            } else {
                                ctx.plain_inst(ctx.core.class_def, "ClassDef")
                            }
                            .map_err(&fail)?;
                            let error = ctx
                                .plain_inst(ctx.core.code_error, "CodeError")
                                .map_err(&fail)?;
                            let result = ctx.result_inst(class, error).map_err(&fail)?;
                            if reply_ty != result {
                                return Err(fail(
                                    "an instance class lookup has the wrong result type"
                                        .to_string(),
                                ));
                            }
                            push(state, result)?;
                        }
                        lm_abi::OP_VM_INSTANCE_SLOT_FOR | lm_abi::OP_VM_INSTANCE_SLOT_SPEC => {
                            let argument = if op == lm_abi::OP_VM_INSTANCE_SLOT_FOR {
                                ctx.plain_inst(ctx.core.slot_spec, "SlotSpec")
                                    .map_err(&fail)?
                            } else {
                                TY_STR
                            };
                            pop_expect(state, argument)?;
                            let instance = ctx
                                .plain_inst(ctx.core.instance, "Instance")
                                .map_err(&fail)?;
                            pop_expect(state, instance)?;
                            let value = if op == lm_abi::OP_VM_INSTANCE_SLOT_FOR {
                                ctx.plain_inst(ctx.core.slot, "Slot").map_err(&fail)?
                            } else {
                                ctx.plain_inst(ctx.core.slot_spec, "SlotSpec")
                                    .map_err(&fail)?
                            };
                            let error = ctx
                                .plain_inst(ctx.core.code_error, "CodeError")
                                .map_err(&fail)?;
                            let result = ctx.result_inst(value, error).map_err(&fail)?;
                            push(state, result)?;
                        }
                        lm_abi::OP_VM_BINDING_SLOT
                        | lm_abi::OP_VM_BINDING_SPEC
                        | lm_abi::OP_VM_BINDING_INSTANCE
                        | lm_abi::OP_VM_BINDING_FUNCTION_TARGET
                        | lm_abi::OP_VM_BINDING_CLASS_TARGET => {
                            let binding = pop(state)?;
                            let function_binding = ctx.core.function_binding.ok_or_else(|| {
                                fail(
                                    "the module does not carry the pinned core FunctionBinding definition"
                                        .to_string(),
                                )
                            })?;
                            let class_binding = ctx
                                .plain_inst(ctx.core.class_binding, "ClassBinding")
                                .map_err(&fail)?;
                            let function_args = match ctx.ty(binding) {
                                BcType::Inst(class, args)
                                    if class == function_binding && args.len() == 2 =>
                                {
                                    Some(args)
                                }
                                _ => None,
                            };
                            let is_class = binding == class_binding;
                            if function_args.is_none() && !is_class {
                                return Err(fail(
                                    "a binding operation needs an installed binding".to_string(),
                                ));
                            }
                            let value = match op {
                                lm_abi::OP_VM_BINDING_SLOT => {
                                    ctx.plain_inst(ctx.core.slot, "Slot").map_err(&fail)?
                                }
                                lm_abi::OP_VM_BINDING_SPEC => ctx
                                    .plain_inst(ctx.core.slot_spec, "SlotSpec")
                                    .map_err(&fail)?,
                                lm_abi::OP_VM_BINDING_INSTANCE => ctx
                                    .plain_inst(ctx.core.instance, "Instance")
                                    .map_err(&fail)?,
                                lm_abi::OP_VM_BINDING_FUNCTION_TARGET => {
                                    let Some(args) = function_args else {
                                        return Err(fail(
                                            "a function target needs a FunctionBinding".to_string(),
                                        ));
                                    };
                                    let function_def = ctx.core.function_def.ok_or_else(|| {
                                        fail(
                                            "the module does not carry the pinned core FunctionDef definition"
                                                .to_string(),
                                        )
                                    })?;
                                    ctx.intern(BcType::Inst(function_def, args))
                                }
                                _ if is_class => ctx
                                    .plain_inst(ctx.core.class_def, "ClassDef")
                                    .map_err(&fail)?,
                                _ => {
                                    return Err(fail(
                                        "a class target needs a ClassBinding".to_string(),
                                    ));
                                }
                            };
                            let error = ctx
                                .plain_inst(ctx.core.code_error, "CodeError")
                                .map_err(&fail)?;
                            let result = ctx.result_inst(value, error).map_err(&fail)?;
                            if reply_ty != result {
                                return Err(fail(
                                    "a binding operation has the wrong result type".to_string(),
                                ));
                            }
                            push(state, reply_ty)?;
                        }
                        lm_abi::OP_VM_ACTIVATE | lm_abi::OP_VM_ACTIVATE_OR_FAULT => {
                            let args_ty = pop(state)?;
                            let fn_ty = pop(state)?;
                            let recv = pop(state)?;
                            if ctx.ty(recv) != BcType::Vm {
                                return Err(fail("`Vm.Activate` needs a Vm receiver".to_string()));
                            }
                            let BcType::Fn(params, _, ret, _) = ctx.ty(fn_ty) else {
                                return Err(fail(
                                    "`Vm.Activate` needs a function value".to_string(),
                                ));
                            };
                            let want = if params.is_empty() {
                                TY_UNIT
                            } else {
                                ctx.intern(BcType::Tuple(params))
                            };
                            if !ctx.is_subtype(args_ty, want) {
                                return Err(fail(
                                    "`Vm.Activate` arguments do not match the \
                                     program parameters"
                                        .to_string(),
                                ));
                            }
                            let run = ctx.intern(BcType::Run(ret));
                            if op == lm_abi::OP_VM_ACTIVATE_OR_FAULT {
                                if reply_ty != run {
                                    return Err(fail(
                                        "`Vm.ActivateOrFault` has the wrong result type"
                                            .to_string(),
                                    ));
                                }
                                push(state, run)?;
                            } else {
                                let error = ctx
                                    .plain_inst(ctx.core.code_error, "CodeError")
                                    .map_err(&fail)?;
                                let result = ctx.result_inst(run, error).map_err(&fail)?;
                                if reply_ty != result {
                                    return Err(fail(
                                        "`Vm.Activate` has the wrong result type".to_string(),
                                    ));
                                }
                                push(state, result)?;
                            }
                        }
                        lm_abi::OP_VM_ACTIVATE_DEF => {
                            let args_ty = pop(state)?;
                            let definition = pop(state)?;
                            pop_expect(state, ctx.intern(BcType::Vm))?;
                            let Some(function_def) = ctx.core.function_def else {
                                return Err(fail(
                                    "the module does not carry the pinned core FunctionDef definition"
                                        .to_string(),
                                ));
                            };
                            let Some(function_binding) = ctx.core.function_binding else {
                                return Err(fail(
                                    "the module does not carry the pinned core FunctionBinding definition"
                                        .to_string(),
                                ));
                            };
                            let BcType::Inst(found, parts) = ctx.ty(definition) else {
                                return Err(fail(
                                    "`Vm.ActivateDef` needs an installed function".to_string(),
                                ));
                            };
                            if (found != function_def && found != function_binding)
                                || parts.len() != 2
                                || !ctx.is_subtype(args_ty, parts[0])
                            {
                                return Err(fail(
                                    "`Vm.ActivateDef` arguments do not match the definition"
                                        .to_string(),
                                ));
                            }
                            let run = ctx.intern(BcType::Run(parts[1]));
                            let error = ctx
                                .plain_inst(ctx.core.code_error, "CodeError")
                                .map_err(&fail)?;
                            let result = ctx.result_inst(run, error).map_err(&fail)?;
                            if reply_ty != result {
                                return Err(fail(
                                    "`Vm.ActivateDef` has the wrong result type".to_string(),
                                ));
                            }
                            push(state, result)?;
                        }
                        lm_abi::OP_VM_REPLACE_FUNCTION | lm_abi::OP_VM_CHANGE_FUNCTION => {
                            let definition = pop(state)?;
                            let slot = pop(state)?;
                            pop_expect(state, ctx.intern(BcType::Vm))?;
                            let function_def = ctx.core.function_def.ok_or_else(|| {
                                fail(
                                    "the module does not carry the pinned core FunctionDef definition"
                                        .to_string(),
                                )
                            })?;
                            let function_binding = ctx.core.function_binding.ok_or_else(|| {
                                fail(
                                    "the module does not carry the pinned core FunctionBinding definition"
                                        .to_string(),
                                )
                            })?;
                            let definition_ty = ctx.ty(definition);
                            let valid = matches!(
                                &definition_ty,
                                BcType::Inst(class, args)
                                    if (*class == function_def || *class == function_binding)
                                        && args.len() == 2
                            ) || matches!(
                                &definition_ty,
                                BcType::Fn(_, muts, _, _)
                                    if !muts.iter().any(|marker| *marker)
                            );
                            if !valid {
                                return Err(fail(
                                    "`Vm.ReplaceFunction` needs a function target".to_string(),
                                ));
                            }
                            let slot_ty = ctx.plain_inst(ctx.core.slot, "Slot").map_err(&fail)?;
                            let binding = matches!(
                                ctx.ty(slot),
                                BcType::Inst(class, args)
                                    if class == function_binding && args.len() == 2
                            );
                            if slot != slot_ty && !binding {
                                return Err(fail(
                                    "`Vm.ReplaceFunction` needs a function binding".to_string(),
                                ));
                            }
                            let error = ctx
                                .plain_inst(ctx.core.code_error, "CodeError")
                                .map_err(&fail)?;
                            let success = if op == lm_abi::OP_VM_CHANGE_FUNCTION {
                                ctx.plain_inst(ctx.core.slot_change, "SlotChange")
                                    .map_err(&fail)?
                            } else {
                                TY_UNIT
                            };
                            let result = ctx.result_inst(success, error).map_err(&fail)?;
                            push(state, result)?;
                        }
                        lm_abi::OP_VM_REPLACE_CLASS | lm_abi::OP_VM_CHANGE_CLASS => {
                            let definition = pop(state)?;
                            let slot = pop(state)?;
                            pop_expect(state, ctx.intern(BcType::Vm))?;
                            let class_def = ctx
                                .plain_inst(ctx.core.class_def, "ClassDef")
                                .map_err(&fail)?;
                            let class_binding = ctx
                                .plain_inst(ctx.core.class_binding, "ClassBinding")
                                .map_err(&fail)?;
                            if definition != class_def && definition != class_binding {
                                return Err(fail(
                                    "`Vm.ReplaceClass` needs a class target".to_string(),
                                ));
                            }
                            let slot_ty = ctx.plain_inst(ctx.core.slot, "Slot").map_err(&fail)?;
                            if slot != slot_ty && slot != class_binding {
                                return Err(fail(
                                    "`Vm.ReplaceClass` needs a class binding".to_string(),
                                ));
                            }
                            let error = ctx
                                .plain_inst(ctx.core.code_error, "CodeError")
                                .map_err(&fail)?;
                            let success = if op == lm_abi::OP_VM_CHANGE_CLASS {
                                ctx.plain_inst(ctx.core.slot_change, "SlotChange")
                                    .map_err(&fail)?
                            } else {
                                TY_UNIT
                            };
                            let result = ctx.result_inst(success, error).map_err(&fail)?;
                            push(state, result)?;
                        }
                        lm_abi::OP_VM_REPLACE_VALUE | lm_abi::OP_VM_CHANGE_VALUE => {
                            pop(state)?;
                            let slot = pop(state)?;
                            pop_expect(state, ctx.intern(BcType::Vm))?;
                            let slot_ty = ctx.plain_inst(ctx.core.slot, "Slot").map_err(&fail)?;
                            if slot != slot_ty {
                                return Err(fail("`Vm.ReplaceValue` needs a Slot".to_string()));
                            }
                            let error = ctx
                                .plain_inst(ctx.core.code_error, "CodeError")
                                .map_err(&fail)?;
                            let success = if op == lm_abi::OP_VM_CHANGE_VALUE {
                                ctx.plain_inst(ctx.core.slot_change, "SlotChange")
                                    .map_err(&fail)?
                            } else {
                                TY_UNIT
                            };
                            let result = ctx.result_inst(success, error).map_err(&fail)?;
                            push(state, result)?;
                        }
                        lm_abi::OP_VM_REPLACE_PROCESS | lm_abi::OP_VM_CHANGE_PROCESS => {
                            let process = pop(state)?;
                            if !matches!(ctx.ty(process), BcType::Handle(_, _)) {
                                return Err(fail(
                                    "`Vm.ReplaceProcess` needs a process handle".to_string(),
                                ));
                            }
                            let slot = pop(state)?;
                            pop_expect(state, ctx.intern(BcType::Vm))?;
                            let slot_ty = ctx.plain_inst(ctx.core.slot, "Slot").map_err(&fail)?;
                            if slot != slot_ty {
                                return Err(fail("`Vm.ReplaceProcess` needs a Slot".to_string()));
                            }
                            let error = ctx
                                .plain_inst(ctx.core.code_error, "CodeError")
                                .map_err(&fail)?;
                            let success = if op == lm_abi::OP_VM_CHANGE_PROCESS {
                                ctx.plain_inst(ctx.core.slot_change, "SlotChange")
                                    .map_err(&fail)?
                            } else {
                                TY_UNIT
                            };
                            let result = ctx.result_inst(success, error).map_err(&fail)?;
                            push(state, result)?;
                        }
                        lm_abi::OP_VM_REPLACE_ALL => {
                            let changes = pop(state)?;
                            pop_expect(state, ctx.intern(BcType::Vm))?;
                            let change = ctx
                                .plain_inst(ctx.core.slot_change, "SlotChange")
                                .map_err(&fail)?;
                            if ctx.ty(changes) != BcType::List(change) {
                                return Err(fail(
                                    "`Vm.ReplaceAll` needs a SlotChange list".to_string(),
                                ));
                            }
                            let error = ctx
                                .plain_inst(ctx.core.code_error, "CodeError")
                                .map_err(&fail)?;
                            let result = ctx.result_inst(TY_UNIT, error).map_err(&fail)?;
                            push(state, result)?;
                        }
                        lm_abi::OP_VM_RUN | lm_abi::OP_VM_STEP | lm_abi::OP_VM_DRIVE => {
                            let t = pop_run(state)?;
                            let (parent, what) = match op {
                                lm_abi::OP_VM_RUN => (ctx.core.run_result, "RunResult"),
                                lm_abi::OP_VM_STEP => (ctx.core.step_event, "StepEvent"),
                                _ => (ctx.core.drive_event, "DriveEvent"),
                            };
                            let event = ctx.event_inst(parent, what, t).map_err(&fail)?;
                            push(state, event)?;
                        }
                        lm_abi::OP_VM_DRIVE_WAIT => {
                            let t = pop_run(state)?;
                            let event = ctx
                                .event_inst(ctx.core.drive_event, "DriveEvent", t)
                                .map_err(&fail)?;
                            let wait = ctx.intern(BcType::Wait(event));
                            push(state, wait)?;
                        }
                        lm_abi::OP_VM_TABLE => {
                            pop_run(state)?;
                            let table = ctx.intern(BcType::PolicyTable);
                            push(state, table)?;
                        }
                        lm_abi::OP_VM_HANDLES => {
                            pop_run(state)?;
                            let control = ctx.intern(BcType::ResourceHandle);
                            let list = ctx.intern(BcType::List(control));
                            push(state, list)?;
                        }
                        lm_abi::OP_VM_RESOURCE => {
                            let handle = pop(state)?;
                            pop_run(state)?;
                            let tcp = ctx
                                .core
                                .tcp_resource
                                .map(|class| ctx.intern(BcType::Class(class)));
                            let tls = ctx
                                .core
                                .tls_stream
                                .map(|class| ctx.intern(BcType::Class(class)));
                            if ctx.ty(handle) != BcType::FileHandle
                                && tcp.is_none_or(|tcp| !ctx.is_subtype(handle, tcp))
                                && tls.is_none_or(|tls| handle != tls)
                            {
                                return Err(fail(
                                    "`Vm.Resource` needs a file or stream resource".to_string(),
                                ));
                            }
                            let control = ctx.intern(BcType::ResourceHandle);
                            push(state, control)?;
                        }
                        lm_abi::OP_VM_SERVE_FILE => {
                            let call = pop(state)?;
                            pop_run(state)?;
                            let args = ctx.op_args_view(lm_abi::OP_FS_OPEN).map_err(&fail)?;
                            let reply = ctx
                                .abi_ty(lm_abi::op(lm_abi::OP_FS_OPEN).reply)
                                .map_err(&fail)?;
                            if ctx.ty(call) != BcType::PendingCall(args, reply) {
                                return Err(fail(
                                    "`Vm.ServeFile` needs an Fs.Open call".to_string(),
                                ));
                            }
                            let control = ctx.intern(BcType::ResourceHandle);
                            push(state, control)?;
                        }
                        lm_abi::OP_VM_SERVE_TCP_STREAM => {
                            let peer = pop(state)?;
                            let call = pop(state)?;
                            pop_run(state)?;
                            let address =
                                ctx.abi_ty(lm_abi::AbiType::SOCKET_ADDRESS).map_err(&fail)?;
                            if !ctx.is_subtype(peer, address) {
                                return Err(fail(
                                    "`Vm.ServeTcpStream` needs a SocketAddress".to_string(),
                                ));
                            }
                            let connect_args =
                                ctx.op_args_view(lm_abi::OP_TCP_CONNECT).map_err(&fail)?;
                            let connect_reply = ctx
                                .abi_ty(lm_abi::op(lm_abi::OP_TCP_CONNECT).reply)
                                .map_err(&fail)?;
                            let accept_args =
                                ctx.op_args_view(lm_abi::OP_TCP_ACCEPT).map_err(&fail)?;
                            let accept_reply = ctx
                                .abi_ty(lm_abi::op(lm_abi::OP_TCP_ACCEPT).reply)
                                .map_err(&fail)?;
                            let valid = ctx.ty(call)
                                == BcType::PendingCall(connect_args, connect_reply)
                                || ctx.ty(call) == BcType::PendingCall(accept_args, accept_reply);
                            if !valid {
                                return Err(fail(
                                    "`Vm.ServeTcpStream` needs a Tcp.Connect or Tcp.Accept call"
                                        .to_string(),
                                ));
                            }
                            let control = ctx.intern(BcType::ResourceHandle);
                            push(state, control)?;
                        }
                        lm_abi::OP_VM_SERVE_TCP_LISTENER => {
                            let call = pop(state)?;
                            pop_run(state)?;
                            let args = ctx.op_args_view(lm_abi::OP_TCP_LISTEN).map_err(&fail)?;
                            let reply = ctx
                                .abi_ty(lm_abi::op(lm_abi::OP_TCP_LISTEN).reply)
                                .map_err(&fail)?;
                            if ctx.ty(call) != BcType::PendingCall(args, reply) {
                                return Err(fail(
                                    "`Vm.ServeTcpListener` needs a Tcp.Listen call".to_string(),
                                ));
                            }
                            let control = ctx.intern(BcType::ResourceHandle);
                            push(state, control)?;
                        }
                        lm_abi::OP_VM_SERVE_TLS_STREAM => {
                            let call = pop(state)?;
                            pop_run(state)?;
                            let args = ctx.op_args_view(lm_abi::OP_TLS_HANDSHAKE).map_err(&fail)?;
                            let reply = ctx
                                .abi_ty(lm_abi::op(lm_abi::OP_TLS_HANDSHAKE).reply)
                                .map_err(&fail)?;
                            if ctx.ty(call) != BcType::PendingCall(args, reply) {
                                return Err(fail(
                                    "`Vm.ServeTlsStream` needs a Tls.Handshake call".to_string(),
                                ));
                            }
                            let control = ctx.intern(BcType::ResourceHandle);
                            push(state, control)?;
                        }
                        lm_abi::OP_VM_RESOURCE_IS_OPEN | lm_abi::OP_VM_RESOURCE_CLOSE => {
                            let control = pop(state)?;
                            if ctx.ty(control) != BcType::ResourceHandle {
                                return Err(fail(format!("`{name}` needs a ResourceHandle")));
                            }
                            push(state, TY_BOOL)?;
                        }
                        lm_abi::OP_VM_RESOURCE_KIND => {
                            let control = pop(state)?;
                            if ctx.ty(control) != BcType::ResourceHandle {
                                return Err(fail(
                                    "`Vm.ResourceKind` needs a ResourceHandle".to_string(),
                                ));
                            }
                            push(state, TY_STR)?;
                        }
                        lm_abi::OP_VM_RESOURCE_SAME => {
                            let other = pop(state)?;
                            let control = pop(state)?;
                            if ctx.ty(control) != BcType::ResourceHandle
                                || ctx.ty(other) != BcType::ResourceHandle
                            {
                                return Err(fail(
                                    "`Vm.ResourceSame` needs two ResourceHandle values".to_string(),
                                ));
                            }
                            push(state, TY_BOOL)?;
                        }
                        lm_abi::OP_VM_ANSWER => {
                            let value = pop(state)?;
                            let call = pop(state)?;
                            pop_run(state)?;
                            let BcType::PendingCall(_, reply) = ctx.ty(call) else {
                                return Err(fail(
                                    "`Vm.Answer` needs a PendingCall token".to_string(),
                                ));
                            };
                            if !ctx.is_subtype(value, reply) {
                                return Err(fail(format!(
                                    "`Vm.Answer` reply expects type {reply}, found \
                                     type {value}"
                                )));
                            }
                            push(state, TY_UNIT)?;
                        }
                        lm_abi::OP_VM_REJECT => {
                            let fault = pop(state)?;
                            let request = pop(state)?;
                            pop_run(state)?;
                            if ctx.ty(fault) != BcType::Fault || ctx.ty(request) != BcType::Request
                            {
                                return Err(fail(
                                    "`Vm.Reject` needs a Request and a Fault".to_string(),
                                ));
                            }
                            push(state, TY_UNIT)?;
                        }
                        lm_abi::OP_VM_DISPATCH => {
                            let request = pop(state)?;
                            pop_run(state)?;
                            if ctx.ty(request) != BcType::Request {
                                return Err(fail("`Vm.Dispatch` needs a Request".to_string()));
                            }
                            push(state, TY_UNIT)?;
                        }
                        lm_abi::OP_PROC_RUN => {
                            let t = pop_run(state)?;
                            // The mailbox-bearing launch is
                            // `Proc.Spawn`. This form takes no message,
                            // so `M` is the bottom type, which the
                            // bytecode encodes as `()`.
                            let handle = ctx.intern(BcType::Handle(TY_UNIT, t));
                            push(state, handle)?;
                        }
                        lm_abi::OP_PROC_SPAWN => {
                            let args_ty = pop(state)?;
                            let body = pop(state)?;
                            let ctor = pop(state)?;
                            let BcType::Fn(ctor_params, _, proc_ty, _) = ctx.ty(ctor) else {
                                return Err(fail(
                                    "`Proc.Spawn` needs a constructor function".to_string(),
                                ));
                            };
                            // The mailbox type comes from the class
                            // table, through the proc class the
                            // constructor builds. No call site can
                            // claim another one.
                            let mailbox = ctx.proc_mailbox(proc_ty).ok_or_else(|| {
                                fail(
                                    "`Proc.Spawn` needs a constructor of a `Proc` subclass"
                                        .to_string(),
                                )
                            })?;
                            let BcType::Fn(body_params, _, result, _) = ctx.ty(body) else {
                                return Err(fail("`Proc.Spawn` needs a body function".to_string()));
                            };
                            // The body may come from an ancestor of the
                            // proc class, so the constructed instance
                            // must satisfy its receiver, not equal it.
                            if body_params.len() != 1 || !ctx.is_subtype(proc_ty, body_params[0]) {
                                return Err(fail(
                                    "`Proc.Spawn` body does not take the constructed proc"
                                        .to_string(),
                                ));
                            }
                            let want = if ctor_params.is_empty() {
                                TY_UNIT
                            } else {
                                ctx.intern(BcType::Tuple(ctor_params))
                            };
                            if !ctx.is_subtype(args_ty, want) {
                                return Err(fail(
                                    "`Proc.Spawn` arguments do not match the constructor \
                                     parameters"
                                        .to_string(),
                                ));
                            }
                            let handle = ctx.intern(BcType::Handle(mailbox, result));
                            push(state, handle)?;
                        }
                        lm_abi::OP_PROC_SEND => {
                            let message = pop(state)?;
                            let handle = pop(state)?;
                            let BcType::Handle(mailbox, _) = ctx.ty(handle) else {
                                return Err(fail("`Proc.Send` needs a proc handle".to_string()));
                            };
                            if !ctx.is_subtype(message, mailbox) {
                                return Err(fail(format!(
                                    "`Proc.Send` expects a message of type {mailbox}, \
                                     found type {message}"
                                )));
                            }
                            let result = ctx
                                .plain_inst(ctx.core.send_result, "SendResult")
                                .map_err(&fail)?;
                            push(state, result)?;
                        }
                        lm_abi::OP_PROC_CLOSE => {
                            let handle = pop(state)?;
                            if !matches!(ctx.ty(handle), BcType::Handle(_, _)) {
                                return Err(fail("`Proc.Close` needs a proc handle".to_string()));
                            }
                            let result = ctx
                                .plain_inst(ctx.core.send_result, "SendResult")
                                .map_err(&fail)?;
                            push(state, result)?;
                        }
                        lm_abi::OP_PROC_DONE => {
                            let handle = pop(state)?;
                            let BcType::Handle(_, result) = ctx.ty(handle) else {
                                return Err(fail("`Proc.Done` needs a proc handle".to_string()));
                            };
                            let event = ctx
                                .event_inst(ctx.core.proc_result, "ProcResult", result)
                                .map_err(&fail)?;
                            push(state, event)?;
                        }
                        lm_abi::OP_PROC_PAUSE | lm_abi::OP_PROC_RESUME => {
                            let handle = pop(state)?;
                            let BcType::Handle(_, result) = ctx.ty(handle) else {
                                return Err(fail(format!("`{name}` needs a proc handle")));
                            };
                            let ok = if op == lm_abi::OP_PROC_PAUSE {
                                ctx.intern(BcType::Run(result))
                            } else {
                                TY_UNIT
                            };
                            let error = ctx
                                .plain_inst(ctx.core.proc_error, "ProcError")
                                .map_err(&fail)?;
                            let Some(result_family) = ctx.core.result else {
                                return Err(fail(
                                    "the module does not carry the pinned core Result \
                                     definition"
                                        .to_string(),
                                ));
                            };
                            let out = ctx.intern(BcType::Inst(result_family, vec![ok, error]));
                            push(state, out)?;
                        }
                        lm_abi::OP_PROC_RECV | lm_abi::OP_PROC_RECV_WAIT => {
                            // The receiver is the performing proc. Its
                            // class fixes the mailbox type, so the
                            // rule reads the class table.
                            let recv = pop(state)?;
                            let mailbox = ctx.proc_mailbox(recv).ok_or_else(|| {
                                fail("`Proc.Recv` needs a `Proc` subclass receiver".to_string())
                            })?;
                            let event = ctx
                                .event_inst(ctx.core.recv, "Recv", mailbox)
                                .map_err(&fail)?;
                            if op == lm_abi::OP_PROC_RECV {
                                push(state, event)?;
                            } else {
                                let wait = ctx.intern(BcType::Wait(event));
                                push(state, wait)?;
                            }
                        }
                        lm_abi::OP_WAIT_WAIT => {
                            let wait = pop(state)?;
                            let BcType::Wait(result) = ctx.ty(wait) else {
                                return Err(fail("`Wait.Wait` needs a Wait value".to_string()));
                            };
                            push(state, result)?;
                        }
                        lm_abi::OP_WAIT_CHOOSE => {
                            let right = pop(state)?;
                            let left = pop(state)?;
                            let BcType::Wait(right) = ctx.ty(right) else {
                                return Err(fail(
                                    "`Wait.Choose` needs two Wait values".to_string(),
                                ));
                            };
                            let BcType::Wait(left) = ctx.ty(left) else {
                                return Err(fail(
                                    "`Wait.Choose` needs two Wait values".to_string(),
                                ));
                            };
                            let Some(choice) = ctx.core.choice else {
                                return Err(fail(
                                    "the module does not carry the pinned core Choice definition"
                                        .to_string(),
                                ));
                            };
                            let choice = ctx.intern(BcType::Inst(choice, vec![left, right]));
                            let wait = ctx.intern(BcType::Wait(choice));
                            push(state, wait)?;
                        }
                        lm_abi::OP_WAIT_CANCEL => {
                            let wait = pop(state)?;
                            if !matches!(ctx.ty(wait), BcType::Wait(_)) {
                                return Err(fail("`Wait.Cancel` needs a Wait value".to_string()));
                            }
                            push(state, TY_BOOL)?;
                        }
                        lm_abi::OP_VM_SNAPSHOT_HELD => {
                            let t = pop_run(state)?;
                            let snapshot = ctx.intern(BcType::RunSnapshot(t));
                            let error = ctx
                                .plain_inst(ctx.core.snapshot_error, "SnapshotError")
                                .map_err(&fail)?;
                            let out = ctx.result_inst(snapshot, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_VM_DRIVE_FOR => {
                            let count = pop(state)?;
                            if ctx.ty(count) != BcType::Int {
                                return Err(fail(
                                    "`Vm.DriveFor` needs an instruction count".to_string(),
                                ));
                            }
                            let t = pop_run(state)?;
                            let event = ctx
                                .event_inst(ctx.core.drive_event, "DriveEvent", t)
                                .map_err(&fail)?;
                            let out = ctx
                                .event_inst(ctx.core.option, "Option", event)
                                .map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_VM_SNAPSHOT_WAIT_HELD => {
                            let fuel = pop(state)?;
                            if ctx.ty(fuel) != BcType::Int {
                                return Err(fail(
                                    "`Vm.SnapshotWaitHeld` needs a fuel count".to_string(),
                                ));
                            }
                            let t = pop_run(state)?;
                            let snapshot = ctx.intern(BcType::RunSnapshot(t));
                            let error = ctx
                                .plain_inst(ctx.core.snapshot_error, "SnapshotError")
                                .map_err(&fail)?;
                            let out = ctx.result_inst(snapshot, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_PROC_SNAPSHOT_WAIT => {
                            pop_expect(state, TY_INT)?;
                            let handle = pop(state)?;
                            let BcType::Handle(_, result) = ctx.ty(handle) else {
                                return Err(fail(
                                    "`Proc.SnapshotWait` needs a proc handle".to_string(),
                                ));
                            };
                            let snapshot = ctx.intern(BcType::RunSnapshot(result));
                            let error = ctx
                                .plain_inst(ctx.core.snapshot_error, "SnapshotError")
                                .map_err(&fail)?;
                            let out = ctx.result_inst(snapshot, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_VM_SNAPSHOT_SELF => {
                            let image = ctx.intern(BcType::VmSnapshot);
                            let error = ctx
                                .plain_inst(ctx.core.snapshot_error, "SnapshotError")
                                .map_err(&fail)?;
                            let out = ctx.result_inst(image, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_VM_SNAPSHOT_VM => {
                            let recv = pop(state)?;
                            if ctx.ty(recv) != BcType::Vm {
                                return Err(fail("`Vm.SnapshotVm` needs a Vm".to_string()));
                            }
                            let image = ctx.intern(BcType::VmSnapshot);
                            let error = ctx
                                .plain_inst(ctx.core.snapshot_error, "SnapshotError")
                                .map_err(&fail)?;
                            let out = ctx.result_inst(image, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_VM_RESTORE => {
                            let snapshot = pop(state)?;
                            let recv = pop(state)?;
                            if ctx.ty(recv) != BcType::Vm {
                                return Err(fail("`Vm.Restore` needs a Vm receiver".to_string()));
                            }
                            let BcType::RunSnapshot(t) = ctx.ty(snapshot) else {
                                return Err(fail(
                                    "`Vm.Restore` needs a typed snapshot".to_string(),
                                ));
                            };
                            let run = ctx.intern(BcType::Run(t));
                            let error = ctx
                                .plain_inst(ctx.core.restore_error, "RestoreError")
                                .map_err(&fail)?;
                            let out = ctx.result_inst(run, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_VM_LOAD_SNAPSHOT => {
                            let bytes = pop(state)?;
                            if ctx.ty(bytes) != BcType::Bytes {
                                return Err(fail("`Vm.LoadSnapshot` needs Bytes".to_string()));
                            }
                            let image = ctx.intern(BcType::VmSnapshot);
                            let error = ctx
                                .plain_inst(ctx.core.snapshot_error, "SnapshotError")
                                .map_err(&fail)?;
                            let out = ctx.result_inst(image, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_VM_RUN_SNAPSHOT_BYTES => {
                            let image = pop(state)?;
                            if !matches!(ctx.ty(image), BcType::RunSnapshot(_)) {
                                return Err(fail(
                                    "`Vm.RunSnapshotBytes` needs a RunSnapshot".to_string(),
                                ));
                            }
                            let error = ctx
                                .plain_inst(ctx.core.snapshot_error, "SnapshotError")
                                .map_err(&fail)?;
                            let bytes = ctx.intern(BcType::Bytes);
                            let out = ctx.result_inst(bytes, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_VM_SNAPSHOT_BYTES => {
                            let image = pop(state)?;
                            if ctx.ty(image) != BcType::VmSnapshot {
                                return Err(fail(
                                    "`Vm.SnapshotBytes` needs a VmSnapshot".to_string(),
                                ));
                            }
                            let error = ctx
                                .plain_inst(ctx.core.snapshot_error, "SnapshotError")
                                .map_err(&fail)?;
                            let bytes = ctx.intern(BcType::Bytes);
                            let out = ctx.result_inst(bytes, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_VM_RESTORE_VM => {
                            let image = pop(state)?;
                            if ctx.ty(image) != BcType::VmSnapshot {
                                return Err(fail("`Vm.RestoreVm` needs a VmSnapshot".to_string()));
                            }
                            let error = ctx
                                .plain_inst(ctx.core.restore_error, "RestoreError")
                                .map_err(&fail)?;
                            let vm = ctx.intern(BcType::Vm);
                            let out = ctx.result_inst(vm, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        _ => unreachable!("every VmControl slot has a rule"),
                    }
                }
            }
            check_reply_ty(ctx, state, reply_ty, &fail)?;
        }
        Instr::PerformValue { argc, reply_ty } => {
            let reply_ty = *reply_ty;
            let argc = *argc as usize;
            if state.stack.len() < argc + 1 {
                return Err(fail("perform through a value on a short stack".to_string()));
            }
            let callee_ty = state.stack[state.stack.len() - 1 - argc];
            let BcType::Op(op, fn_ty) = ctx.ty(callee_ty) else {
                return Err(fail(format!(
                    "perform target type {callee_ty} is not an operation value"
                )));
            };
            let name = lm_abi::op_name(op);
            if !ctx.row_has_name(&func.row, &name) {
                return Err(fail(format!(
                    "the perform of `{name}` is not inside the claimed row"
                )));
            }
            let BcType::Fn(params, _, ret, _) = ctx.ty(fn_ty) else {
                unreachable!("a verified Op type embeds a function type");
            };
            if params.len() != argc {
                return Err(fail("perform argument count mismatch".to_string()));
            }
            pop_args(state, &params)?;
            pop(state)?;
            push(state, ret)?;
            check_reply_ty(ctx, state, reply_ty, &fail)?;
        }
        Instr::OpConst(op) => {
            let sig = ctx.fixed_sig_type(*op).map_err(&fail)?;
            let ty = ctx.intern(BcType::Op(*op, sig));
            push(state, ty)?;
        }
        Instr::TableEdit { action, kind, slot } => {
            if *action == 2 {
                let handler = pop(state)?;
                let want = ctx.fixed_sig_type(*slot).map_err(&fail)?;
                if !ctx.is_subtype(handler, want) {
                    return Err(fail(format!(
                        "a mock handler must have the exact operation signature \
                         with an empty row, found type {handler}"
                    )));
                }
            }
            let table = pop(state)?;
            if ctx.ty(table) != BcType::PolicyTable {
                return Err(fail(format!("table edit on non-table type {table}")));
            }
            if *action == 0 {
                // The dependent grant rule: `pass` is charged to the
                // granter's claimed row.
                let name = if *kind == 0 {
                    lm_abi::op_name(*slot)
                } else {
                    lm_abi::GROUPS[*slot as usize].to_string()
                };
                if !ctx.row_has_name(&func.row, &name) {
                    return Err(fail(format!(
                        "the pass of `{name}` is not inside the claimed row"
                    )));
                }
            }
            push(state, TY_UNIT)?;
        }
        Instr::AsCall { op, ty } => {
            let request = pop(state)?;
            if ctx.ty(request) != BcType::Request {
                return Err(fail(format!("as_call on non-request type {request}")));
            }
            let view = ctx.op_args_view(*op).map_err(&fail)?;
            let def = lm_abi::op(*op);
            let reply = ctx.abi_ty(def.reply).map_err(&fail)?;
            let call = ctx.intern(BcType::PendingCall(view, reply));
            let out = ctx
                .event_inst(ctx.core.option, "Option", call)
                .map_err(&fail)?;
            if !ctx.is_subtype(*ty, out) || !ctx.is_subtype(out, *ty) {
                return Err(fail("as_call option type mismatch".to_string()));
            }
            push(state, *ty)?;
        }
        Instr::CallArgs => {
            let call = pop(state)?;
            let BcType::PendingCall(view, _) = ctx.ty(call) else {
                return Err(fail(format!("args view on non-call type {call}")));
            };
            push(state, view)?;
        }
        Instr::FaultCode => {
            let fault = pop(state)?;
            if ctx.ty(fault) != BcType::Fault {
                return Err(fail(format!("fault code on non-fault type {fault}")));
            }
            push(state, TY_STR)?;
        }
        Instr::FaultDenied => {
            pop_expect(state, TY_STR)?;
            let fault = ctx.intern(BcType::Fault);
            push(state, fault)?;
        }
        Instr::RaiseUserPanic | Instr::RaiseAssertionFailed => {
            pop_expect(state, TY_STR)?;
        }
        Instr::RequestOp => {
            let request = pop(state)?;
            if ctx.ty(request) != BcType::Request {
                return Err(fail(format!("request op on non-request type {request}")));
            }
            push(state, TY_STR)?;
        }
        Instr::Unreachable => {
            // A diverging terminator: no stack effect, no successor.
        }
    }
    Ok(())
}
