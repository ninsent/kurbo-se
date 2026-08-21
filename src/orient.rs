// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Per-subpath side resolution: mapping user-facing alignment onto a
//! geometric left/right side.
//!
//! Conventions (see crate docs): Y-down, unit normal `n̂ = t̂.turn_90()` points
//! right of travel, positive signed area = clockwise, so a positive-area
//! subpath has its own interior ("own disk") on the right of travel.
//!
//! For closed subpaths, `Inside` means the side of the *filled region* of the
//! whole compound path under the nonzero rule. The own-disk rule (area sign)
//! answers that for outer boundaries but inverts it for holes, so we probe
//! the actual fill: a point just inside the subpath's own disk has nonzero
//! whole-path winding iff the own disk is filled there. Holes stroke into the
//! ring, and reversing winding directions changes nothing.

use kurbo::{BezPath, ParamCurve, ParamCurveExtrema, PathEl, Point, Shape, Vec2};

use crate::style::{StrokeAlignment, StrokeSide};

/// Number of topmost-candidate points to try before giving up on probing.
const MAX_PROBE_CANDIDATES: usize = 8;

/// Outcome of side resolution for one subpath or lobe.
#[derive(Copy, Clone)]
pub(crate) struct Resolved {
    /// The geometric side the band goes on.
    pub side: StrokeSide,
    /// The side facing the filled region (closed, one-sided cases only) —
    /// used by the extreme-width pair clamp.
    pub fill_side: Option<StrokeSide>,
}

/// Resolve the geometric side for one subpath.
pub(crate) fn resolve(
    alignment: StrokeAlignment,
    side_override: Option<StrokeSide>,
    subpath: &[PathEl],
    closed: bool,
    whole: &BezPath,
    probe_scale: f64,
) -> Resolved {
    // The fill probe is needed both for alignment resolution and for the
    // hole-aware width clamp, but only for closed one-sided bands.
    let wants_one_sided = matches!(side_override, Some(StrokeSide::Left | StrokeSide::Right))
        || (side_override.is_none()
            && matches!(
                alignment,
                StrokeAlignment::Inside | StrokeAlignment::Outside
            ));
    let fill = if closed && wants_one_sided {
        Some(fill_side(subpath, whole, probe_scale))
    } else {
        None
    };
    let side = if let Some(side) = side_override {
        side
    } else {
        match alignment {
            StrokeAlignment::Center => StrokeSide::Center,
            StrokeAlignment::Inside if !closed => StrokeSide::Right,
            StrokeAlignment::Outside if !closed => StrokeSide::Left,
            inside_or_outside => {
                let inside = fill.unwrap_or(StrokeSide::Right);
                if inside_or_outside == StrokeAlignment::Inside {
                    inside
                } else {
                    inside.opposite()
                }
            }
        }
    };
    Resolved {
        side,
        fill_side: fill,
    }
}

/// Convenience wrapper returning just the side.
pub(crate) fn resolve_side(
    alignment: StrokeAlignment,
    side_override: Option<StrokeSide>,
    subpath: &[PathEl],
    closed: bool,
    whole: &BezPath,
    probe_scale: f64,
) -> StrokeSide {
    resolve(
        alignment,
        side_override,
        subpath,
        closed,
        whole,
        probe_scale,
    )
    .side
}

impl StrokeSide {
    pub(crate) fn opposite(self) -> StrokeSide {
        match self {
            StrokeSide::Left => StrokeSide::Right,
            StrokeSide::Right => StrokeSide::Left,
            StrokeSide::Center => StrokeSide::Center,
        }
    }
}

/// Which side of a closed subpath faces the filled region of `whole`.
pub(crate) fn fill_side(subpath: &[PathEl], whole: &BezPath, probe_scale: f64) -> StrokeSide {
    // Own-disk side from the subpath's signed area (closing chord included:
    // the slice ends with ClosePath, so `segments` emits it).
    let own_disk_right = subpath.area() >= 0.0;
    let own_disk = if own_disk_right {
        StrokeSide::Right
    } else {
        StrokeSide::Left
    };

    let delta = 1e-6 * probe_scale;
    if delta > 0.0 {
        for delta in [delta, delta / 1024.0] {
            if let Some(p_in) = probe_point(subpath, delta) {
                let filled = whole.winding(p_in) != 0;
                return if filled {
                    own_disk
                } else {
                    own_disk.opposite()
                };
            }
        }
    }
    // Degenerate geometry (zero-size path, sub-delta features): fall back to
    // the own-disk rule. Deterministic, documented.
    own_disk
}

/// A point verified to lie inside the subpath's own disk.
///
/// Candidates are the topmost points of the subpath (curve extrema and
/// segment endpoints, ascending in y). At a topmost point the subpath's
/// interior lies locally below, so `candidate + (0, δ)` is inside — verified
/// against the subpath's own winding to survive corners, flat tops, and
/// spikes thinner than δ.
fn probe_point(subpath: &[PathEl], delta: f64) -> Option<Point> {
    let mut candidates: [Point; MAX_PROBE_CANDIDATES] = [Point::ORIGIN; MAX_PROBE_CANDIDATES];
    let mut n = 0;
    let mut consider = |p: Point| {
        if !p.is_finite() {
            return;
        }
        if n < MAX_PROBE_CANDIDATES {
            candidates[n] = p;
            n += 1;
        } else if let Some(worst) = candidates[..n]
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.y.total_cmp(&b.1.y))
            .map(|(i, _)| i)
        {
            if p.y < candidates[worst].y {
                candidates[worst] = p;
            }
        }
    };
    for seg in kurbo::segments(subpath.iter().copied()) {
        consider(seg.eval(0.0));
        consider(seg.eval(1.0));
        for t in seg.extrema() {
            consider(seg.eval(t));
        }
    }
    let candidates = &mut candidates[..n];
    candidates.sort_by(|a, b| a.y.total_cmp(&b.y));
    for c in candidates.iter() {
        let p_in = *c + Vec2::new(0.0, delta);
        if subpath.winding(p_in) != 0 {
            return Some(p_in);
        }
    }
    None
}

/// Split a width across the two band sides according to the resolved side.
pub(crate) fn side_distances(side: StrokeSide, width: f64) -> (f64, f64) {
    match side {
        StrokeSide::Center => (0.5 * width, 0.5 * width),
        StrokeSide::Right => (0.0, width),
        StrokeSide::Left => (width, 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize;
    use kurbo::Rect;

    fn closed_rect_cw() -> BezPath {
        // CW in Y-down: positive area.
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((10.0, 0.0));
        p.line_to((10.0, 10.0));
        p.line_to((0.0, 10.0));
        p.close_path();
        p
    }

    fn resolve_first(alignment: StrokeAlignment, path: &BezPath) -> StrokeSide {
        let scale = path.bounding_box().size().max_side();
        let els = path.elements();
        let sub = normalize::subpaths(els).next().unwrap();
        resolve_side(alignment, None, sub, normalize::is_closed(sub), path, scale)
    }

    #[test]
    fn cw_rect_inside_is_right() {
        let p = closed_rect_cw();
        assert!(p.elements().area() >= 0.0, "CW rect must be positive area");
        assert_eq!(
            resolve_first(StrokeAlignment::Inside, &p),
            StrokeSide::Right
        );
        assert_eq!(
            resolve_first(StrokeAlignment::Outside, &p),
            StrokeSide::Left
        );
    }

    #[test]
    fn ccw_rect_inside_is_left() {
        let p = closed_rect_cw().reverse_subpaths();
        assert_eq!(resolve_first(StrokeAlignment::Inside, &p), StrokeSide::Left);
        assert_eq!(
            resolve_first(StrokeAlignment::Outside, &p),
            StrokeSide::Right
        );
    }

    /// Donut: the hole contour must stroke into the ring (fill side), not
    /// into the void — for both hole winding directions.
    #[test]
    fn hole_inside_faces_the_ring() {
        for reverse_hole in [false, true] {
            let mut p = closed_rect_cw(); // outer, CW, area +100
            let hole = Rect::new(3.0, 3.0, 7.0, 7.0);
            let mut hole_path: BezPath = hole.to_path(1e-9);
            if !reverse_hole {
                // Make the hole CCW (opposite of outer) unless testing the
                // ill-formed same-winding case below.
                hole_path = hole_path.reverse_subpaths();
            }
            let hole_ccw = hole_path.elements().area() < 0.0;
            p.extend(hole_path.iter());

            let scale = p.bounding_box().size().max_side();
            let els = p.elements();
            let hole_sub = normalize::subpaths(els).nth(1).unwrap();
            let side = resolve_side(StrokeAlignment::Inside, None, hole_sub, true, &p, scale);
            if hole_ccw {
                // CCW hole: own disk (void) on the left ⇒ ring on the right.
                assert_eq!(
                    side,
                    StrokeSide::Right,
                    "CCW hole strokes right, into the ring"
                );
            } else {
                // CW "hole" inside a CW outer isn't a hole under nonzero —
                // its disk has winding 2 ⇒ filled ⇒ own-disk side (right).
                assert_eq!(side, StrokeSide::Right);
            }
        }
    }

    /// Two disjoint contours with opposite winding directions: each resolves
    /// independently (the `expand_path` global-sign failure mode is avoided).
    #[test]
    fn disjoint_opposite_winding() {
        let mut p = closed_rect_cw();
        let far = Rect::new(100.0, 100.0, 110.0, 110.0).to_path(1e-9);
        // kurbo Rect paths are CW (positive); reverse the second.
        p.extend(far.reverse_subpaths().iter());
        let scale = p.bounding_box().size().max_side();
        let els = p.elements();
        let subs: alloc::vec::Vec<&[PathEl]> = normalize::subpaths(els).collect();
        let s0 = resolve_side(StrokeAlignment::Inside, None, subs[0], true, &p, scale);
        let s1 = resolve_side(StrokeAlignment::Inside, None, subs[1], true, &p, scale);
        assert_eq!(s0, StrokeSide::Right, "CW contour: inside on the right");
        assert_eq!(s1, StrokeSide::Left, "CCW contour: inside on the left");
    }

    #[test]
    fn open_subpath_convention_and_override() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((10.0, 0.0));
        let scale = 10.0;
        let els = p.elements();
        let sub = normalize::subpaths(els).next().unwrap();
        assert_eq!(
            resolve_side(StrokeAlignment::Inside, None, sub, false, &p, scale),
            StrokeSide::Right
        );
        assert_eq!(
            resolve_side(StrokeAlignment::Outside, None, sub, false, &p, scale),
            StrokeSide::Left
        );
        assert_eq!(
            resolve_side(
                StrokeAlignment::Inside,
                Some(StrokeSide::Left),
                sub,
                false,
                &p,
                scale
            ),
            StrokeSide::Left
        );
    }
}
