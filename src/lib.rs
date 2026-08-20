// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stroke extensions for [kurbo]: stroke **alignment** (inside / center / outside),
//! independent start/end caps, dash caps, and dashed aligned strokes — the feature
//! set of Figma's *Stroke settings* panel, produced as fill outlines on top of
//! kurbo's public API.
//!
//! kurbo-se is **not a fork**: it depends on unmodified kurbo from crates.io and
//! re-exports it (`kurbo_se::kurbo`), so the types are identical to the ones your
//! renderer already consumes.
//!
//! # Output contract
//!
//! [`stroke_aligned`] returns a [`BezPath`](kurbo::BezPath) describing the
//! stroke as a *fill outline*. Interpret it with the **nonzero winding
//! rule**. The outline may
//! self-overlap by design (inner joins, cusps, 180° turns); those overlaps
//! cancel visually only under nonzero winding.
//!
//! # Conventions
//!
//! - Coordinates are **Y-down** (the usual graphics convention, same as kurbo).
//! - For a unit tangent `t̂`, the unit normal `n̂ = t̂.turn_90()` points to the
//!   **right** of the travel direction.
//! - Positive signed area = clockwise winding (in Y-down), matching
//!   [`kurbo::Shape::area`]. A positive-area subpath has its interior on the
//!   right of travel.
//! - The stroke band at a path point `P` spans `[P − d_left·n̂, P + d_right·n̂]`
//!   with `d_left + d_right = width`. [`StrokeSide::Right`] means
//!   `(d_left, d_right) = (0, width)`, so the *left* boundary is the source
//!   path itself, exactly.
//!
//! # Alignment semantics
//!
//! For closed contours the aligned band is defined as a *set*, matching
//! Figma (whose inside/outside strokes are a doubled centered stroke masked
//! by the fill):
//!
//! ```text
//! Inside(w)  = { p ∈ F : dist(p, path) ≤ w }
//! Outside(w) = { p ∉ F : dist(p, path) ≤ w }
//! ```
//!
//! where `F` is the filled region of the whole path under the nonzero rule.
//! Two consequences are worth stating outright:
//!
//! - an inside stroke **never** leaves the fill and an outside stroke never
//!   enters it, at any width — dashed strokes included, where each dash is
//!   expanded and then masked by the fill;
//! - a width beyond the local thickness **saturates** instead of folding
//!   over itself — the stroke fills the shape completely, exactly as Figma
//!   renders it (a rectangle narrower than `2w`, a thin wedge, a small star).
//!
//! - **Closed subpaths**: `Inside` is the side of the *filled region* of the
//!   whole compound path (holes stroke into the ring, not into the void).
//!   Winding direction of the input does not change what `Inside` means.
//!   Self-intersecting contours are split at their crossings, so each lobe
//!   resolves against the fill on its own.
//! - **Open subpaths**: inside/outside is geometrically undefined, so kurbo-se
//!   uses the documented convention `Inside → Right`, `Outside → Left`
//!   (relative to travel direction). Set [`StrokeStyle::side`] to pick a side
//!   explicitly. Open subpaths are banded independently of the closed
//!   contours' region construction: a nearby open subpath never prunes or
//!   masks a closed contour's inside/outside band, and overlapping bands
//!   simply add under nonzero winding.
//!
//! # Quickstart
//!
//! ```
//! use kurbo_se::{StrokeStyle, StrokeAlignment, stroke_aligned};
//! use kurbo_se::kurbo::{Rect, Shape};
//!
//! let shape = Rect::new(0.0, 0.0, 100.0, 60.0);
//! let style = StrokeStyle::new(8.0).with_alignment(StrokeAlignment::Inside);
//! let outline = stroke_aligned(shape, &style, 0.1);
//! // Fill `outline` with the nonzero rule. Inside alignment never escapes the shape:
//! assert!(!outline.is_empty());
//! assert!(shape.bounding_box().contains_rect(outline.bounding_box()));
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]

#[cfg(not(any(feature = "std", feature = "libm")))]
compile_error!("kurbo-se requires either the `std` or `libm` feature");

// Suppress unused-crate warning when both std and libm are enabled.
#[cfg(all(feature = "std", feature = "libm"))]
use libm as _;

extern crate alloc;

pub use kurbo;

mod clip;
mod ctx;
mod dash;
mod expand;
mod math;
mod normalize;
mod orient;
mod region;
mod split;
mod style;

pub use ctx::{
    AlignedStrokeCtx, StrokeAlignExt, SubpathInfo, analyze_subpaths, stroke_aligned,
    stroke_aligned_with,
};
pub use style::{
    DashStyle, StrokeAlignment, StrokeSide, StrokeStyle, miter_angle_to_limit, miter_limit_to_angle,
};

// Convenience re-exports of the kurbo types used in this crate's public API.
pub use kurbo::{Cap, Dashes, Join};
