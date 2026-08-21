// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Float functions that come from `std` or `libm`, depending on features.
//!
//! kurbo solves the same problem with its sealed `common::FloatFuncs` trait,
//! but that trait only exists when kurbo itself is built without `std`, and
//! feature unification by unrelated dependencies could flip that from under
//! us. A local shim keeps kurbo-se's build self-contained.

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

/// Whether a segment covers no distance.
///
/// Vanishing *squared* length, not exact coincidence: control deltas small
/// enough that their squared length underflows to zero are not
/// point-coincident, but they carry no usable direction either — normalizing
/// such a tangent divides by zero, and kurbo's closed-form quad arc length
/// returns `NaN` for them (see [`arclen`]). Both cases are dropped together
/// wherever segments are consumed: band expansion, dashing, and
/// self-intersection splitting (where runs of them would also read as the
/// path revisiting a vertex).
#[inline]
pub(crate) fn is_degenerate(seg: &kurbo::PathSeg) -> bool {
    match seg {
        kurbo::PathSeg::Line(l) => (l.p1 - l.p0).hypot2() == 0.0,
        kurbo::PathSeg::Quad(q) => (q.p1 - q.p0).hypot2() == 0.0 && (q.p2 - q.p1).hypot2() == 0.0,
        kurbo::PathSeg::Cubic(c) => {
            (c.p1 - c.p0).hypot2() == 0.0
                && (c.p2 - c.p1).hypot2() == 0.0
                && (c.p3 - c.p2).hypot2() == 0.0
        }
    }
}

/// Arc length of a path segment, guarded against non-finite results.
///
/// `QuadBez::arclen` has a closed form that divides by the squared derivative
/// magnitude, so it returns `NaN` whenever that quantity is not a usable
/// number: for coincident control points, for deltas small enough to
/// underflow, and for deltas large enough to overflow. One `NaN` poisons
/// every arc-length accumulator downstream — dash phases go `NaN` and the
/// pattern stops advancing, so the rest of the path renders as one
/// uninterrupted dash.
///
/// Vanishing segments are dropped before they get here (see
/// [`is_degenerate`]); this covers the overflow end, where the
/// segment has real length that simply cannot be measured. Substituting `0`
/// keeps the phase finite so the rest of the path still dashes.
#[inline]
pub(crate) fn arclen(seg: &kurbo::PathSeg, accuracy: f64) -> f64 {
    let len = kurbo::ParamCurveArclen::arclen(seg, accuracy);
    if len.is_finite() { len } else { 0.0 }
}

/// `hypot` without the slow correctly-rounded library call.
///
/// kurbo deliberately avoids `f64::hypot` for speed (its #448/#451); we
/// follow suit. Inputs in stroke geometry are far from overflow.
#[inline]
pub(crate) fn hypot(x: f64, y: f64) -> f64 {
    sqrt(x * x + y * y)
}
