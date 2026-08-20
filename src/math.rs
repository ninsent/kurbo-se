// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Float functions that come from `std` or `libm`, depending on features.
//!
//! kurbo solves the same problem with its sealed `common::FloatFuncs` trait,
//! but that trait only exists when kurbo itself is built without `std`, and
//! feature unification by unrelated dependencies could flip that from under
//! us. A local shim keeps kurbo-se's build self-contained.

#![allow(dead_code)] // some shims are used only by later modules

#[inline]
pub(crate) fn sin(x: f64) -> f64 {
    #[cfg(feature = "std")]
    {
        x.sin()
    }
    #[cfg(not(feature = "std"))]
    {
        libm::sin(x)
    }
}

#[inline]
pub(crate) fn asin(x: f64) -> f64 {
    #[cfg(feature = "std")]
    {
        x.asin()
    }
    #[cfg(not(feature = "std"))]
    {
        libm::asin(x)
    }
}

#[inline]
pub(crate) fn atan2(y: f64, x: f64) -> f64 {
    #[cfg(feature = "std")]
    {
        y.atan2(x)
    }
    #[cfg(not(feature = "std"))]
    {
        libm::atan2(y, x)
    }
}

#[inline]
pub(crate) fn sqrt(x: f64) -> f64 {
    #[cfg(feature = "std")]
    {
        x.sqrt()
    }
    #[cfg(not(feature = "std"))]
    {
        libm::sqrt(x)
    }
}

#[inline]
pub(crate) fn abs(x: f64) -> f64 {
    #[cfg(feature = "std")]
    {
        x.abs()
    }
    #[cfg(not(feature = "std"))]
    {
        libm::fabs(x)
    }
}

#[inline]
pub(crate) fn ceil(x: f64) -> f64 {
    #[cfg(feature = "std")]
    {
        x.ceil()
    }
    #[cfg(not(feature = "std"))]
    {
        libm::ceil(x)
    }
}

#[inline]
pub(crate) fn floor(x: f64) -> f64 {
    #[cfg(feature = "std")]
    {
        x.floor()
    }
    #[cfg(not(feature = "std"))]
    {
        libm::floor(x)
    }
}

/// Euclidean modulo with a positive result for positive `rhs`.
///
/// `f64::rem_euclid` is std-only; this mirrors kurbo's internal fallback.
#[inline]
pub(crate) fn rem_euclid(x: f64, rhs: f64) -> f64 {
    let r = x % rhs;
    if r < 0.0 { r + abs(rhs) } else { r }
}

/// `hypot` without the slow correctly-rounded library call.
///
/// kurbo deliberately avoids `f64::hypot` for speed (its #448/#451); we
/// follow suit. Inputs in stroke geometry are far from overflow.
#[inline]
pub(crate) fn hypot(x: f64, y: f64) -> f64 {
    sqrt(x * x + y * y)
}
