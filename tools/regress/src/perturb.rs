// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Synthetic edits of an outline, for checking the comparator. Some leave
//! the painted region unchanged, or move it by well under the tolerance.
//! Others change it by more.

use kurbo::{Affine, BezPath, CubicBez, ParamCurve, PathEl, Point, Shape, Vec2};

use crate::corpus::XorShift;

/// The subpaths of `path`, each starting with its `MoveTo`.
pub fn subpaths(path: &BezPath) -> Vec<BezPath> {
    let mut out: Vec<BezPath> = Vec::new();
    for el in path.elements() {
        if matches!(el, PathEl::MoveTo(_)) || out.is_empty() {
            out.push(BezPath::new());
        }
        out.last_mut().expect("pushed above").push(*el);
    }
    out
}

fn concat(parts: impl IntoIterator<Item = BezPath>) -> BezPath {
    let mut out = BezPath::new();
    for p in parts {
        out.extend(&p);
    }
    out
}

/// The same subpaths in reverse order.
pub fn reorder_subpaths(path: &BezPath) -> BezPath {
    concat(subpaths(path).into_iter().rev())
}

/// `path` with subpath `index` removed.
#[cfg(test)]
pub fn without_subpath(path: &BezPath, index: usize) -> BezPath {
    concat(
        subpaths(path)
            .into_iter()
            .enumerate()
            .filter(|&(i, _)| i != index)
            .map(|(_, p)| p),
    )
}

/// `path` with only subpath `index` reversed.
#[cfg(test)]
pub fn reverse_subpath(path: &BezPath, index: usize) -> BezPath {
    concat(
        subpaths(path)
            .into_iter()
            .enumerate()
            .map(|(i, p)| if i == index { p.reverse_subpaths() } else { p }),
    )
}

/// Every cubic split at `t = 0.5` into two.
pub fn split_cubics(path: &BezPath) -> BezPath {
    let mut out = BezPath::new();
    let mut cur = Point::ORIGIN;
    let mut start = Point::ORIGIN;
    for el in path.elements() {
        match *el {
            PathEl::CurveTo(p1, p2, p3) => {
                let (a, b) = CubicBez::new(cur, p1, p2, p3).subdivide();
                out.curve_to(a.p1, a.p2, a.p3);
                out.curve_to(b.p1, b.p2, b.p3);
            }
            other => out.push(other),
        }
        cur = match *el {
            PathEl::MoveTo(p) => {
                start = p;
                p
            }
            PathEl::ClosePath => start,
            ref other => other.end_point().unwrap_or(cur),
        };
    }
    out
}

/// `path` plus loops that paint nothing: a zero-area back-and-forth loop
/// inside the bounding box and one outside it, a triangle a billionth of
/// the outline's size, and a single-point subpath.
pub fn add_degenerate_loops(path: &BezPath) -> BezPath {
    let bbox = path.bounding_box();
    let size = bbox.width().max(bbox.height()).max(1e-9);
    let mut out = path.clone();
    for p in [bbox.center(), Point::new(bbox.x1 + 0.1 * size, bbox.y0)] {
        out.move_to(p);
        out.line_to(p + Vec2::new(0.03, 0.04) * size);
        out.line_to(p);
        out.close_path();
    }
    let p = bbox.center() + Vec2::new(0.1, 0.07) * size;
    out.move_to(p);
    out.line_to(p + Vec2::new(1e-9, 0.0) * size);
    out.line_to(p + Vec2::new(0.0, 1e-9) * size);
    out.close_path();
    out.move_to(bbox.center());
    out.line_to(bbox.center());
    out
}

/// Every coordinate moved by a pseudo-random amount in `[-amount, amount]`.
/// The amount depends only on the coordinate's value, so points that
/// coincide keep coinciding. Moving them apart would open cracks between
/// edges that meet or cancel exactly. That changes the region instead of
/// moving its boundary.
pub fn jitter(path: &BezPath, amount: f64, seed: u64) -> BezPath {
    let shift = |v: f64, salt: u64| {
        let mut rng = XorShift::new(v.to_bits() ^ seed ^ salt);
        v + amount * rng.uniform(-1.0, 1.0)
    };
    let j = |p: Point| Point::new(shift(p.x, 0x9e37), shift(p.y, 0x79b9));
    let mut out = BezPath::new();
    for el in path.elements() {
        out.push(match *el {
            PathEl::MoveTo(p) => PathEl::MoveTo(j(p)),
            PathEl::LineTo(p) => PathEl::LineTo(j(p)),
            PathEl::QuadTo(p1, p2) => PathEl::QuadTo(j(p1), j(p2)),
            PathEl::CurveTo(p1, p2, p3) => PathEl::CurveTo(j(p1), j(p2), j(p3)),
            PathEl::ClosePath => PathEl::ClosePath,
        });
    }
    out
}

/// Curves replaced by line segments within `tol` of them.
pub fn reflatten(path: &BezPath, tol: f64) -> BezPath {
    let mut out = BezPath::new();
    let mut cur = kurbo::Point::ORIGIN;
    for el in path.iter() {
        match el {
            PathEl::MoveTo(p) => {
                out.move_to(p);
                cur = p;
            }
            PathEl::ClosePath => out.close_path(),
            _ => {
                crate::compare::polyline_to(cur, el, tol, |p| out.line_to(p));
                cur = el.end_point().unwrap_or(cur);
            }
        }
    }
    out
}

/// `path` moved by `offset`.
pub fn translate(path: &BezPath, offset: Vec2) -> BezPath {
    Affine::translate(offset) * path.clone()
}

/// `path` scaled about its bounding-box centre so that its farthest
/// anchor point moves by `displacement`. The factor is
/// `1 + displacement / size`, where `size` is that anchor's distance from
/// the centre.
pub fn scale_about_center(path: &BezPath, displacement: f64) -> BezPath {
    let c = path.bounding_box().center();
    let size = path
        .elements()
        .iter()
        .filter_map(|el| el.end_point())
        .map(|p| (p - c).hypot())
        .fold(0.0, f64::max);
    let k = 1.0 + displacement / size.max(1e-300);
    Affine::translate(c.to_vec2()) * Affine::scale(k) * Affine::translate(-c.to_vec2()) * path.clone()
}

/// The named perturbations for the runner's `--perturb` mode. That mode
/// applies one to the reference output and uses the result in place of
/// the candidate's. Each takes the outline and the case tolerance, and
/// comes with whether it must pass.
pub fn named(kind: &str) -> Option<(fn(&BezPath, f64) -> BezPath, bool)> {
    Some(match kind {
        "reorder" => (|p, _| reorder_subpaths(p), true),
        "reverse" => (|p, _| p.reverse_subpaths(), true),
        "split" => (|p, _| split_cubics(p), true),
        "degenerate" => (|p, _| add_degenerate_loops(p), true),
        "jitter" => (|p, tol| jitter(p, tol / 10.0, 0x5eed), true),
        "reflatten" => (|p, tol| reflatten(p, tol / 20.0), true),
        "translate" => (|p, tol| translate(p, Vec2::new(3.0 * tol, 0.0)), false),
        "scale" => (|p, tol| scale_about_center(p, 3.0 * tol), false),
        _ => return None,
    })
}

/// The names accepted by [`named`].
pub const NAMES: &[&str] = &[
    "reorder",
    "reverse",
    "split",
    "degenerate",
    "jitter",
    "reflatten",
    "translate",
    "scale",
];
