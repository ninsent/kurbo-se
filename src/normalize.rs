// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Input canonicalization and subpath inspection helpers.
//!
//! The expansion pipeline wants a stream where every subpath is cleanly
//! delimited: it starts with a `MoveTo` and contains at most one
//! `ClosePath`, as its final element. SVG, and kurbo's stroker, allow
//! segments to follow a `ClosePath` with no intervening `MoveTo`; the new
//! subpath then implicitly starts at the closed subpath's start point.
//! That implicit `MoveTo` gets materialized here.

use kurbo::{BezPath, CubicBez, Line, PathEl, PathSeg, Point, QuadBez};

use crate::math;

/// Collect `elements` into `out` (reusing its allocation), canonicalizing
/// subpath boundaries.
///
/// Guarantees on the result:
/// - the first element of every subpath is a `MoveTo`;
/// - `ClosePath` only ever appears as the last element of its subpath;
/// - no drawing element forms a degenerate segment, as judged by
///   [`math::is_degenerate`]. Such segments have no normalizable tangent
///   and a `NaN` arc length upstream, so every stage would have to skip
///   them anyway; dropping them here, once, protects all of them. The next
///   element re-anchors at the previous on-curve point, which moves it by
///   less than the pipeline can resolve.
pub(crate) fn collect_canonical(elements: impl Iterator<Item = PathEl>, out: &mut BezPath) {
    out.truncate(0);
    // Start point of the current subpath, used both for the implicit `MoveTo`
    // after a `ClosePath` and for leading-element repair.
    let mut start = Point::ORIGIN;
    let mut last = Point::ORIGIN;
    let mut seen_moveto = false;
    let mut after_close = false;
    for el in elements {
        match el {
            PathEl::MoveTo(p) => {
                out.move_to(p);
                start = p;
                last = p;
                seen_moveto = true;
                after_close = false;
            }
            PathEl::ClosePath => {
                if !seen_moveto {
                    // A leading ClosePath closes nothing; drop it.
                    continue;
                }
                if after_close {
                    // `Z Z`: the second close applies to the implicit new
                    // subpath at the close point — a zero-length closed
                    // subpath, materialized explicitly.
                    out.move_to(start);
                }
                out.close_path();
                last = start;
                after_close = true;
            }
            _ => {
                if !seen_moveto {
                    // Leading drawing element: kurbo treats it as a MoveTo to
                    // its end point (the segment itself would be degenerate).
                    let p = el.end_point().unwrap_or(Point::ORIGIN);
                    out.move_to(p);
                    start = p;
                    last = p;
                    seen_moveto = true;
                    continue;
                }
                if after_close {
                    out.move_to(start);
                    last = start;
                    after_close = false;
                }
                let degenerate = match el {
                    PathEl::LineTo(p) => math::is_degenerate(&PathSeg::Line(Line::new(last, p))),
                    PathEl::QuadTo(p1, p2) => {
                        math::is_degenerate(&PathSeg::Quad(QuadBez::new(last, p1, p2)))
                    }
                    PathEl::CurveTo(p1, p2, p3) => {
                        math::is_degenerate(&PathSeg::Cubic(CubicBez::new(last, p1, p2, p3)))
                    }
                    _ => false, // MoveTo/ClosePath handled above
                };
                if degenerate {
                    continue;
                }
                out.push(el);
                last = el.end_point().unwrap_or(last);
            }
        }
    }
}

/// Iterator over subpath slices of a canonicalized element list.
///
/// Same splitting rule as `BezPath::subpaths`, where a new subpath begins
/// at each `MoveTo`. It is reimplemented over a plain slice so the caller
/// can keep the backing `BezPath` inside a context struct.
pub(crate) fn subpaths(elements: &[PathEl]) -> impl Iterator<Item = &[PathEl]> {
    let mut i = 0;
    core::iter::from_fn(move || {
        if i >= elements.len() {
            return None;
        }
        let start = i;
        i += 1;
        while i < elements.len() && !matches!(elements[i], PathEl::MoveTo(_)) {
            i += 1;
        }
        Some(&elements[start..i])
    })
}

/// Whether a canonicalized subpath slice is closed.
pub(crate) fn is_closed(subpath: &[PathEl]) -> bool {
    matches!(subpath.last(), Some(PathEl::ClosePath))
}

/// The start point of a canonicalized subpath slice.
pub(crate) fn start_point(subpath: &[PathEl]) -> Point {
    match subpath.first() {
        Some(PathEl::MoveTo(p)) => *p,
        _ => Point::ORIGIN, // canonicalization makes this unreachable
    }
}

/// The end point of a subpath slice (start point if closed or degenerate).
pub(crate) fn end_point(subpath: &[PathEl]) -> Point {
    let start = start_point(subpath);
    match subpath.last() {
        Some(PathEl::ClosePath) | None => start,
        Some(el) => el.end_point().unwrap_or(start),
    }
}

/// Whether the subpath produces no band geometry, because every drawing
/// element sits on the start point and the expansion skips all of them.
///
/// "On" is underflow-aware, matching [`crate::math::is_degenerate`]. A
/// point whose offset from the start is too small to square is coincident
/// as far as every downstream consumer is concerned, so such a subpath
/// renders like the exactly coincident one — a dot at most — instead of
/// feeding the expansion tangents it cannot normalize.
pub(crate) fn is_zero_length(subpath: &[PathEl]) -> bool {
    let start = start_point(subpath);
    let at_start = |p: &Point| (*p - start).hypot2() == 0.0;
    subpath.iter().skip(1).all(|el| match el {
        PathEl::LineTo(p) => at_start(p),
        PathEl::QuadTo(p1, p2) => at_start(p1) && at_start(p2),
        PathEl::CurveTo(p1, p2, p3) => at_start(p1) && at_start(p2) && at_start(p3),
        PathEl::ClosePath => true,
        PathEl::MoveTo(_) => false, // not present mid-slice after canonicalization
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn canon(els: &[PathEl]) -> Vec<PathEl> {
        let mut out = BezPath::new();
        collect_canonical(els.iter().copied(), &mut out);
        out.into_elements()
    }

    #[test]
    fn inserts_moveto_after_close() {
        let els = canon(&[
            PathEl::MoveTo((1.0, 2.0).into()),
            PathEl::LineTo((3.0, 2.0).into()),
            PathEl::ClosePath,
            PathEl::LineTo((5.0, 5.0).into()),
        ]);
        assert_eq!(
            els,
            [
                PathEl::MoveTo((1.0, 2.0).into()),
                PathEl::LineTo((3.0, 2.0).into()),
                PathEl::ClosePath,
                PathEl::MoveTo((1.0, 2.0).into()),
                PathEl::LineTo((5.0, 5.0).into()),
            ]
        );
    }

    #[test]
    fn double_close_becomes_degenerate_subpath() {
        let els = canon(&[
            PathEl::MoveTo((1.0, 2.0).into()),
            PathEl::LineTo((3.0, 2.0).into()),
            PathEl::ClosePath,
            PathEl::ClosePath,
        ]);
        assert_eq!(
            els,
            [
                PathEl::MoveTo((1.0, 2.0).into()),
                PathEl::LineTo((3.0, 2.0).into()),
                PathEl::ClosePath,
                PathEl::MoveTo((1.0, 2.0).into()),
                PathEl::ClosePath,
            ]
        );
    }

    #[test]
    fn leading_lineto_becomes_moveto() {
        let els = canon(&[
            PathEl::LineTo((3.0, 2.0).into()),
            PathEl::LineTo((4.0, 2.0).into()),
        ]);
        assert_eq!(
            els,
            [
                PathEl::MoveTo((3.0, 2.0).into()),
                PathEl::LineTo((4.0, 2.0).into()),
            ]
        );
    }

    #[test]
    fn subpath_split_and_classify() {
        let els = canon(&[
            PathEl::MoveTo((0.0, 0.0).into()),
            PathEl::LineTo((1.0, 0.0).into()),
            PathEl::ClosePath,
            PathEl::MoveTo((5.0, 5.0).into()),
            PathEl::LineTo((6.0, 5.0).into()),
            PathEl::MoveTo((9.0, 9.0).into()),
            PathEl::LineTo((9.0, 9.0).into()),
        ]);
        let subs: Vec<&[PathEl]> = subpaths(&els).collect();
        assert_eq!(subs.len(), 3);
        assert!(is_closed(subs[0]) && !is_closed(subs[1]) && !is_closed(subs[2]));
        assert!(!is_zero_length(subs[0]) && !is_zero_length(subs[1]));
        assert!(is_zero_length(subs[2]));
        assert_eq!(end_point(subs[1]), (6.0, 5.0).into());
    }
}
