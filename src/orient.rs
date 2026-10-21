// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Per-subpath side resolution: mapping user-facing alignment onto a
//! geometric left or right side.
//!
//! The crate docs give the conventions. Y-down, the unit normal
//! `n̂ = t̂.turn_90()` points right of travel, and positive signed area means
//! clockwise, so a positive-area subpath has its own interior — its "own
//! disk" — on the right of travel.
//!
//! For closed subpaths, `Inside` means the side of the filled region of the
//! whole compound path under the nonzero rule. The own-disk rule from the
//! area sign answers that for outer boundaries, but inverts it for holes,
//! so the actual fill gets probed instead: a point just inside the
//! subpath's own disk has nonzero whole-path winding exactly when the own
//! disk is filled there. Holes then stroke into the ring, and reversing
//! winding directions changes nothing.

use kurbo::{
    Affine, BezPath, ParamCurve, ParamCurveDeriv, ParamCurveExtrema, ParamCurveNearest, PathEl,
    PathSeg, Point, Shape, Vec2,
};

use crate::style::{StrokeAlignment, StrokeSide};

/// Number of topmost-candidate points to try before giving up on probing.
const MAX_PROBE_CANDIDATES: usize = 8;

/// Outcome of side resolution for one subpath or lobe.
#[derive(Copy, Clone)]
pub(crate) struct Resolved {
    /// The geometric side the band goes on.
    pub side: StrokeSide,
    /// The side facing the filled region (closed, one-sided cases only) —
    /// orients the region construction and the dash fill mask.
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
    // The fill probe is only needed for closed one-sided bands.
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
    // Own-disk side from the subpath's signed area. The closing chord
    // counts: the slice ends with ClosePath, so `segments` emits it.
    let own_disk_right = subpath.area() >= 0.0;
    let own_disk = if own_disk_right {
        StrokeSide::Right
    } else {
        StrokeSide::Left
    };

    let delta = 1e-6 * probe_scale;
    if delta > 0.0 {
        for delta in [delta, delta / 1024.0] {
            let probe = probe_point(subpath, delta)
                .map(|p| (p, Ray::Up))
                .or_else(|| probe_beside(subpath, delta, own_disk_right));
            if let Some((p_in, ray)) = probe {
                let filled = ray.winding(whole.elements(), p_in) != 0;
                return if filled {
                    own_disk
                } else {
                    own_disk.opposite()
                };
            }
        }
    }
    // Degenerate geometry — a zero-size path, or features smaller than the
    // probe delta — falls back to the own-disk rule. Deterministic.
    own_disk
}

/// Which way a probe's winding is counted.
///
/// Just below the highest point of a curve, a ray to the left grazes it.
/// Its two crossings nearly coincide, and rounding decides them. kurbo
/// solves each curve for the ray's height and can lose them there: the
/// figure-eight in `tests/degenerate.rs`, scaled by 1e-3, read the top of
/// its left loop as unfilled. [`winding`] finds crossings by bisection,
/// which cannot lose them. On top of that, a probe is counted along a ray
/// that crosses the curve it sits by: upwards below a top, and sideways
/// beside a steep curve. So no count has to resolve a graze.
#[derive(Copy, Clone)]
enum Ray {
    Left,
    Up,
}

impl Ray {
    /// The winding number of `els` at `p`, counted along this ray.
    fn winding(self, els: &[PathEl], p: Point) -> i32 {
        match self {
            Ray::Left => winding(els, p),
            Ray::Up => {
                // Swapping the axes turns the leftward ray upwards. The swap
                // is exact, and a mirror only flips the winding's sign.
                // Open subpaths are closed first, as filling closes them.
                // Left open, the count would depend on the ray's direction:
                // a leftward ray never reaches an open polyline lying above
                // the probe, but an upward ray can.
                let swap = Affine::new([0.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
                let mut swapped = BezPath::new();
                let mut open = false;
                for &el in els {
                    match el {
                        PathEl::MoveTo(_) if open => swapped.close_path(),
                        PathEl::ClosePath => open = false,
                        _ => open = true,
                    }
                    if matches!(el, PathEl::MoveTo(_)) {
                        open = false;
                    }
                    swapped.push(swap * el);
                }
                if open {
                    swapped.close_path();
                }
                winding(swapped.elements(), swap * p)
            }
        }
    }
}

/// A point verified to lie inside the subpath's own disk.
///
/// The candidates are the topmost points of the subpath: curve extrema and
/// segment endpoints, ascending in y. At a topmost point the interior lies
/// locally below, so `candidate + (0, δ)` is inside. Each candidate is
/// checked against the subpath's own winding, which is what makes this
/// survive corners, flat tops, and spikes thinner than δ. The point must
/// also keep clear of the subpath, as [`is_clear`] checks.
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
        // Only the extrema in `y`. At an extremum in `x` the curve is
        // vertical, so the point below it lies on the curve to within
        // rounding, and any winding taken there is noise.
        for t in seg.extrema() {
            let d = crate::split::deriv(&seg, t);
            if d.y.abs() <= d.x.abs() {
                consider(seg.eval(t));
            }
        }
    }
    let candidates = &mut candidates[..n];
    candidates.sort_by(|a, b| a.y.total_cmp(&b.y));
    for c in candidates.iter() {
        let p_in = *c + Vec2::new(0.0, delta);
        if Ray::Up.winding(subpath, p_in) != 0 && is_clear(subpath, p_in, delta) {
            return Some(p_in);
        }
    }
    None
}

/// A point verified to lie inside the subpath's own disk, `delta` off the
/// middle of one of its segments, on the side the own disk lies.
///
/// The fallback for a subpath whose topmost points all fail [`probe_point`]:
/// an apex that leans, so its interior lies beside it rather than below, and
/// a vertical edge running down from the next one, so the point below lies on
/// the subpath itself. A segment's middle is no corner, so the own disk lies
/// just off it to one side. Without this, such a hole fell back to the
/// own-disk rule and was stroked as if it were filled.
fn probe_beside(subpath: &[PathEl], delta: f64, own_disk_right: bool) -> Option<(Point, Ray)> {
    for seg in kurbo::segments(subpath.iter().copied()) {
        let tangent = match seg {
            PathSeg::Line(line) => line.p1 - line.p0,
            PathSeg::Quad(quad) => quad.deriv().eval(0.5).to_vec2(),
            PathSeg::Cubic(cubic) => cubic.deriv().eval(0.5).to_vec2(),
        };
        let length = tangent.hypot();
        if length == 0.0 || !length.is_finite() {
            continue;
        }
        let right = tangent.turn_90() / length;
        let toward_disk = if own_disk_right { right } else { -right };
        let p_in = seg.eval(0.5) + toward_disk * delta;
        let ray = if tangent.y.abs() < tangent.x.abs() {
            Ray::Up
        } else {
            Ray::Left
        };
        if ray.winding(subpath, p_in) != 0 && is_clear(subpath, p_in, delta) {
            return Some((p_in, ray));
        }
    }
    None
}

/// Whether `p_in`, a probe `delta` off the subpath, stays more than a
/// sixteenth of that distance away from all of it.
///
/// A probe δ below a point where the curve runs vertically lies only about
/// δ² from the curve. At the finer δ that is rounding, so the subpath's
/// winding at the probe is noise. A lobe is a subsegment of the path it
/// was cut from, so the two outlines differ by the same rounding, and the
/// whole path can read the probe the other way. In the loop of
/// `M20,150 C200,20 0,20 180,150`, the probe below the lobe's leftmost
/// point read inside the lobe but outside the path, and the lobe got the
/// wrong fill side.
fn is_clear(subpath: &[PathEl], p_in: Point, delta: f64) -> bool {
    let margin = delta / 16.0;
    kurbo::segments(subpath.iter().copied()).all(|seg| {
        !crate::clip::control_box(&seg)
            .inflate(margin, margin)
            .contains(p_in)
            || seg.nearest(p_in, 1e-9).distance_sq > margin * margin
    })
}

/// The winding number of `els` around `p`. This is kurbo's
/// `Shape::winding`, except that curve crossings are found by bisection.
///
/// Both count crossings of a ray pointing left, one monotonic piece of a
/// segment at a time. kurbo 0.13.1 finds where a cubic piece meets the ray
/// by solving the cubic in closed form. On either side of a loop's apex the
/// pieces are nearly quadratic, so their leading coefficient is rounding
/// noise and the solver loses the real root. A point 2e-4 below the top of
/// the loop in `M20,150 C200,20 0,20 180,150` read as outside it. A piece
/// is monotonic, so bisecting its parameter always finds the crossing.
pub(crate) fn winding(els: &[PathEl], p: Point) -> i32 {
    kurbo::segments(els.iter().copied())
        .map(|seg| segment_winding(&seg, p))
        .sum()
}

fn segment_winding(seg: &PathSeg, p: Point) -> i32 {
    let bounds = crate::clip::control_box(seg);
    if p.y < bounds.y0 || p.y > bounds.y1 || p.x < bounds.x0 {
        return 0;
    }
    let mut winding = 0;
    let mut piece_start = (0.0, seg.start());
    for t in seg.extrema().into_iter().chain([1.0]) {
        let piece_end = (t, if t == 1.0 { seg.end() } else { seg.eval(t) });
        winding += piece_winding(seg, piece_start, piece_end, p);
        piece_start = piece_end;
    }
    winding
}

/// How one monotonic piece of `seg` crosses the leftward ray from `p`:
/// `1` or `-1` by direction, or `0` if it does not. The piece runs from
/// parameter `t0` at `start` to `t1` at `end`. As in kurbo, a piece
/// includes its end with the smaller `y` but not the other, so a vertex
/// shared by two pieces counts once.
fn piece_winding(
    seg: &PathSeg,
    (t0, start): (f64, Point),
    (t1, end): (f64, Point),
    p: Point,
) -> i32 {
    let sign = if end.y > start.y {
        if p.y < start.y || p.y >= end.y {
            return 0;
        }
        -1
    } else if end.y < start.y {
        if p.y < end.y || p.y >= start.y {
            return 0;
        }
        1
    } else {
        return 0;
    };
    // `extrema` cuts at x extrema too, so the piece lies between its ends in x.
    if p.x < start.x.min(end.x) {
        return 0;
    }
    if p.x >= start.x.max(end.x) {
        return sign;
    }
    if let PathSeg::Line(_) = seg {
        // kurbo's side-of-line test, which needs no solve.
        let a = end.y - start.y;
        let b = start.x - end.x;
        let c = a * start.x + b * start.y;
        return if (a * p.x + b * p.y - c) * f64::from(sign) <= 0.0 {
            sign
        } else {
            0
        };
    }
    // The crossing stays between `lo` and `hi`. The piece is monotonic in x,
    // so the answer is known once both their x values lie on one side of `p`.
    let below_at_start = start.y <= p.y;
    let (mut lo, mut hi) = ((t0, start), (t1, end));
    loop {
        if p.x < lo.1.x.min(hi.1.x) {
            return 0;
        }
        if p.x >= lo.1.x.max(hi.1.x) {
            return sign;
        }
        let t = 0.5 * (lo.0 + hi.0);
        if t <= lo.0 || t >= hi.0 {
            return if p.x >= lo.1.x { sign } else { 0 };
        }
        let mid = (t, seg.eval(t));
        if (mid.1.y <= p.y) == below_at_start {
            lo = mid;
        } else {
            hi = mid;
        }
    }
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

    /// The loop of `M20,150 C200,20 0,20 180,150`, cut off as a lobe, is a
    /// counter-clockwise teardrop. The whole path fills its disk, so the
    /// lobe's fill side must face the disk. kurbo's winding misses the
    /// crossing 2e-4 below the apex, so the probe fell through to a point
    /// below the lobe's leftmost point. That point is within rounding of the
    /// curve, where the lobe and the whole path disagree.
    #[test]
    fn loop_lobe_faces_its_filled_disk() {
        let mut whole = BezPath::new();
        whole.move_to((20.0, 150.0));
        whole.curve_to((200.0, 20.0), (0.0, 20.0), (180.0, 150.0));
        let scale = whole.control_box().size().max_side();
        for accuracy in [0.25, 1e-3] {
            let split =
                crate::split::split_self_intersections(whole.elements(), false, accuracy).unwrap();
            let lobe = split.pieces.iter().find(|p| p.closed).unwrap();
            let lobe = lobe.els.elements();
            assert!(lobe.area() < 0.0, "disk on the left");
            assert_eq!(winding(lobe, Point::new(100.0, 52.5 + 2e-4)), -1);
            assert_eq!(fill_side(lobe, &whole, scale), StrokeSide::Left);
        }
    }
}
