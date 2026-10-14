//! [`Real`]: the scalar a network computes in.

use std::fmt::Debug;
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub};

/// The scalar a [`crate::Network`] computes in: `f32` to train and infer,
/// `f64` for [`crate::grad_check`], whose central differences an `f32` loss
/// is too coarse to resolve.
///
/// Only the operations the kit uses, each the plain IEEE one: no fused
/// multiply-add, so a result does not depend on the target's features.
pub trait Real:
    Copy
    + Debug
    + Default
    + PartialOrd
    + Send
    + Sync
    + 'static
    + Add<Output = Self>
    + Sub<Output = Self>
    + Mul<Output = Self>
    + Div<Output = Self>
    + Neg<Output = Self>
    + AddAssign
{
    const ZERO: Self;
    const ONE: Self;
    fn from_f32(v: f32) -> Self;
    fn from_f64(v: f64) -> Self;
    fn to_f64(self) -> f64;
    fn exp(self) -> Self;
    fn ln_1p(self) -> Self;
    fn abs(self) -> Self;
}

macro_rules! real {
    ($t:ty) => {
        impl Real for $t {
            const ZERO: Self = 0.0;
            const ONE: Self = 1.0;
            #[inline]
            fn from_f32(v: f32) -> Self {
                v as $t
            }
            #[inline]
            fn from_f64(v: f64) -> Self {
                v as $t
            }
            #[inline]
            fn to_f64(self) -> f64 {
                self as f64
            }
            #[inline]
            fn exp(self) -> Self {
                <$t>::exp(self)
            }
            #[inline]
            fn ln_1p(self) -> Self {
                <$t>::ln_1p(self)
            }
            #[inline]
            fn abs(self) -> Self {
                <$t>::abs(self)
            }
        }
    };
}

real!(f32);
real!(f64);
