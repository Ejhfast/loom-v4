//! Direct math imports for generated code.

use super::*;

#[derive(Clone, Copy)]
pub(super) struct MathFunctions {
    pub(super) remainder: ir::FuncRef,
    pub(super) copy_sign: ir::FuncRef,
    pub(super) mul_add: ir::FuncRef,
    pub(super) pow: ir::FuncRef,
    pub(super) exp: ir::FuncRef,
    pub(super) exp2: ir::FuncRef,
    pub(super) exp_m1: ir::FuncRef,
    pub(super) ln: ir::FuncRef,
    pub(super) log2: ir::FuncRef,
    pub(super) log10: ir::FuncRef,
    pub(super) ln_1p: ir::FuncRef,
    pub(super) cbrt: ir::FuncRef,
    pub(super) hypot: ir::FuncRef,
    pub(super) sin: ir::FuncRef,
    pub(super) cos: ir::FuncRef,
    pub(super) tan: ir::FuncRef,
    pub(super) asin: ir::FuncRef,
    pub(super) acos: ir::FuncRef,
    pub(super) atan: ir::FuncRef,
    pub(super) atan2: ir::FuncRef,
    pub(super) sinh: ir::FuncRef,
    pub(super) cosh: ir::FuncRef,
    pub(super) tanh: ir::FuncRef,
    pub(super) asinh: ir::FuncRef,
    pub(super) acosh: ir::FuncRef,
    pub(super) atanh: ir::FuncRef,
}

pub(super) fn register_symbols(builder: &mut JITBuilder) {
    builder.symbol("loom_math_remainder", lm_math::remainder as *const u8);
    builder.symbol("loom_math_copy_sign", lm_math::copy_sign as *const u8);
    builder.symbol("loom_math_mul_add", lm_math::mul_add as *const u8);
    builder.symbol("loom_math_pow", lm_math::pow as *const u8);
    builder.symbol("loom_math_exp", lm_math::exp as *const u8);
    builder.symbol("loom_math_exp2", lm_math::exp2 as *const u8);
    builder.symbol("loom_math_exp_m1", lm_math::exp_m1 as *const u8);
    builder.symbol("loom_math_ln", lm_math::ln as *const u8);
    builder.symbol("loom_math_log2", lm_math::log2 as *const u8);
    builder.symbol("loom_math_log10", lm_math::log10 as *const u8);
    builder.symbol("loom_math_ln_1p", lm_math::ln_1p as *const u8);
    builder.symbol("loom_math_cbrt", lm_math::cbrt as *const u8);
    builder.symbol("loom_math_hypot", lm_math::hypot as *const u8);
    builder.symbol("loom_math_sin", lm_math::sin as *const u8);
    builder.symbol("loom_math_cos", lm_math::cos as *const u8);
    builder.symbol("loom_math_tan", lm_math::tan as *const u8);
    builder.symbol("loom_math_asin", lm_math::asin as *const u8);
    builder.symbol("loom_math_acos", lm_math::acos as *const u8);
    builder.symbol("loom_math_atan", lm_math::atan as *const u8);
    builder.symbol("loom_math_atan2", lm_math::atan2 as *const u8);
    builder.symbol("loom_math_sinh", lm_math::sinh as *const u8);
    builder.symbol("loom_math_cosh", lm_math::cosh as *const u8);
    builder.symbol("loom_math_tanh", lm_math::tanh as *const u8);
    builder.symbol("loom_math_asinh", lm_math::asinh as *const u8);
    builder.symbol("loom_math_acosh", lm_math::acosh as *const u8);
    builder.symbol("loom_math_atanh", lm_math::atanh as *const u8);
}

fn declare_import(
    module: &mut JITModule,
    function: &mut ir::Function,
    call_conv: CallConv,
    name: &str,
    arity: usize,
) -> Result<ir::FuncRef, CompileError> {
    let mut signature = ir::Signature::new(call_conv);
    for _ in 0..arity {
        signature.params.push(AbiParam::new(types::F64));
    }
    signature.returns.push(AbiParam::new(types::F64));
    let id = module
        .declare_function(name, Linkage::Import, &signature)
        .map_err(|_| CompileError::Backend)?;
    Ok(module.declare_func_in_func(id, function))
}

impl MathFunctions {
    pub(super) fn declare(
        module: &mut JITModule,
        function: &mut ir::Function,
        call_conv: CallConv,
    ) -> Result<MathFunctions, CompileError> {
        Ok(MathFunctions {
            remainder: declare_import(module, function, call_conv, "loom_math_remainder", 2)?,
            copy_sign: declare_import(module, function, call_conv, "loom_math_copy_sign", 2)?,
            mul_add: declare_import(module, function, call_conv, "loom_math_mul_add", 3)?,
            pow: declare_import(module, function, call_conv, "loom_math_pow", 2)?,
            exp: declare_import(module, function, call_conv, "loom_math_exp", 1)?,
            exp2: declare_import(module, function, call_conv, "loom_math_exp2", 1)?,
            exp_m1: declare_import(module, function, call_conv, "loom_math_exp_m1", 1)?,
            ln: declare_import(module, function, call_conv, "loom_math_ln", 1)?,
            log2: declare_import(module, function, call_conv, "loom_math_log2", 1)?,
            log10: declare_import(module, function, call_conv, "loom_math_log10", 1)?,
            ln_1p: declare_import(module, function, call_conv, "loom_math_ln_1p", 1)?,
            cbrt: declare_import(module, function, call_conv, "loom_math_cbrt", 1)?,
            hypot: declare_import(module, function, call_conv, "loom_math_hypot", 2)?,
            sin: declare_import(module, function, call_conv, "loom_math_sin", 1)?,
            cos: declare_import(module, function, call_conv, "loom_math_cos", 1)?,
            tan: declare_import(module, function, call_conv, "loom_math_tan", 1)?,
            asin: declare_import(module, function, call_conv, "loom_math_asin", 1)?,
            acos: declare_import(module, function, call_conv, "loom_math_acos", 1)?,
            atan: declare_import(module, function, call_conv, "loom_math_atan", 1)?,
            atan2: declare_import(module, function, call_conv, "loom_math_atan2", 2)?,
            sinh: declare_import(module, function, call_conv, "loom_math_sinh", 1)?,
            cosh: declare_import(module, function, call_conv, "loom_math_cosh", 1)?,
            tanh: declare_import(module, function, call_conv, "loom_math_tanh", 1)?,
            asinh: declare_import(module, function, call_conv, "loom_math_asinh", 1)?,
            acosh: declare_import(module, function, call_conv, "loom_math_acosh", 1)?,
            atanh: declare_import(module, function, call_conv, "loom_math_atanh", 1)?,
        })
    }
}
