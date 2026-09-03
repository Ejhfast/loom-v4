//! Pure floating-point math for Loom.
//!
//! The pinned `libm` version supplies one implementation for both execution engines.

#![no_std]

/// The canonical quiet NaN encoding.
pub const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;

#[inline]
fn canonical(value: f64) -> f64 {
    if value.is_nan() {
        f64::from_bits(CANONICAL_NAN_BITS)
    } else {
        value
    }
}

macro_rules! unary {
    ($name:ident, $operation:path) => {
        #[doc = concat!("Compute `", stringify!($name), "` and normalize every NaN result.")]
        pub extern "C" fn $name(value: f64) -> f64 {
            canonical($operation(value))
        }
    };
}

macro_rules! binary {
    ($name:ident, $operation:path) => {
        #[doc = concat!("Compute `", stringify!($name), "` and normalize every NaN result.")]
        pub extern "C" fn $name(left: f64, right: f64) -> f64 {
            canonical($operation(left, right))
        }
    };
}

binary!(remainder, libm::fmod);
binary!(copy_sign, libm::copysign);
binary!(pow, libm::pow);
binary!(hypot, libm::hypot);
binary!(atan2, libm::atan2);

/// Compute one fused multiply-add and normalize every NaN result.
pub extern "C" fn mul_add(value: f64, multiplier: f64, addend: f64) -> f64 {
    canonical(libm::fma(value, multiplier, addend))
}

unary!(exp, libm::exp);
unary!(exp2, libm::exp2);
unary!(exp_m1, libm::expm1);
unary!(ln, libm::log);
unary!(log2, libm::log2);
unary!(log10, libm::log10);
unary!(ln_1p, libm::log1p);
unary!(cbrt, libm::cbrt);
unary!(sin, libm::sin);
unary!(cos, libm::cos);
unary!(tan, libm::tan);
unary!(asin, libm::asin);
unary!(acos, libm::acos);
unary!(atan, libm::atan);
unary!(sinh, libm::sinh);
unary!(cosh, libm::cosh);
unary!(tanh, libm::tanh);
unary!(asinh, libm::asinh);
unary!(acosh, libm::acosh);
unary!(atanh, libm::atanh);

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{actual} != {expected}"
        );
    }

    #[test]
    fn elementary_results_are_accurate() {
        close(sin(0.5), 0.479_425_538_604_203, 1.0e-15);
        close(cos(0.5), 0.877_582_561_890_372_8, 1.0e-15);
        close(exp(1.0), core::f64::consts::E, 1.0e-15);
        close(ln(core::f64::consts::E), 1.0, 1.0e-15);
        close(pow(3.0, 4.0), 81.0, 0.0);
        close(hypot(3.0, 4.0), 5.0, 0.0);
    }

    #[test]
    fn signed_zero_is_preserved() {
        assert_eq!(remainder(-4.0, 2.0).to_bits(), (-0.0_f64).to_bits());
        assert_eq!(copy_sign(0.0, -1.0).to_bits(), (-0.0_f64).to_bits());
        assert_eq!(atan2(-0.0, 1.0).to_bits(), (-0.0_f64).to_bits());
    }

    #[test]
    fn every_nan_is_canonical() {
        let signaling = f64::from_bits(0x7ff0_0000_0000_0001);
        for result in [
            remainder(1.0, 0.0),
            copy_sign(signaling, -1.0),
            pow(-1.0, 0.5),
            mul_add(f64::INFINITY, 0.0, 1.0),
            ln(-1.0),
            asin(2.0),
            acosh(0.5),
        ] {
            assert_eq!(result.to_bits(), CANONICAL_NAN_BITS);
        }
    }

    #[test]
    fn boundary_results_follow_binary64() {
        assert_eq!(exp(f64::NEG_INFINITY), 0.0);
        assert_eq!(ln(0.0), f64::NEG_INFINITY);
        assert_eq!(atanh(1.0), f64::INFINITY);
        assert_eq!(tanh(f64::INFINITY), 1.0);
    }
}
