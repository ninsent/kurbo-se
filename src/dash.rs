// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dashing: pattern sanitization, per-subpath orchestration of
//! `kurbo::dash`, cap bookkeeping, and dot synthesis for zero-length
//! dashes.
//!
//! The ordering is load-bearing. The caller resolves the side from the
//! undashed subpath. Dashes are cut from the original centerline, so their
//! lengths never distort with alignment. Each dash is then expanded on its
//! own. Interior dash edges wear the dash cap, while the true endpoints of
//! an open subpath keep the style's start and end caps. Closed contours use
//! kurbo's default seam-merge, so a dash spanning the seam stays one piece
//! and animating the offset never pops.

use alloc::vec::Vec;

use kurbo::{
    BezPath, Cap, ParamCurve, ParamCurveArclen, ParamCurveDeriv, PathEl, Point, Shape as _, Vec2,
};

use crate::clip::{ClipSource, ClipWork};
use crate::expand::{Band, BandParams};
use crate::math;
use crate::style::DashStyle;

/// How a dash band is masked by the fill (see [`crate::clip`]).
#[derive(Copy, Clone)]
pub(crate) struct DashClip<'a> {
    pub source: &'a ClipSource,
    /// `true` for inside alignment (keep the part in the fill), `false` for
    /// outside (keep the part outside it).
    pub keep_inside: bool,
    pub accuracy: f64,
}

/// Working storage for dashing. `AlignedStrokeCtx` keeps it, so every dash
/// of every stroke reuses the same buffers: the dash being collected, its
/// band before masking, and the mask's own storage.
#[derive(Default, Debug)]
pub(crate) struct DashWork {
    group: Vec<PathEl>,
    band: BezPath,
    clip: ClipWork,
}

/// Where a piece sits in its subpath's dash pattern.
///
/// An unsplit subpath uses `Phase::default()`. A piece split at
/// self-intersections copies the fields of its
/// [`SplitArc`](crate::split::SplitArc).
#[derive(Copy, Clone, Default)]
pub(crate) struct Phase {
    /// Arc length where the piece starts in the original subpath. It keeps
    /// the pattern continuous across pieces.
    pub s0: f64,
    /// Index of the piece's first element after the start point of its
    /// closed subpath, where the pattern restarts. See
    /// [`SplitArc::seam`](crate::split::SplitArc::seam).
    pub seam: Option<usize>,
    /// Whether another piece picks up the pattern where this one ends. See
    /// [`SplitArc::shares_end`](crate::split::SplitArc::shares_end).
    pub shares_end: bool,
}

/// Arc-length accuracy, matching kurbo's internal `DASH_ACCURACY`.
const DASH_ACCURACY: f64 = 1e-6;

/// A validated dash configuration.
///
/// kurbo's raw `dash()` panics on an empty pattern and can loop forever on
/// patterns with negative sums. Anything unusable maps to `None`, which
/// renders solid, per the crate's documented contract.
pub(crate) struct SanitizedDash<'a> {
    pub pattern: &'a [f64],
    pub offset: f64,
    pub cap: Cap,
    /// Whether the pattern has zero-length "on" entries, which become dots.
    pub has_zero_dashes: bool,
}

pub(crate) fn sanitize(dash: &DashStyle) -> Option<SanitizedDash<'_>> {
    let pattern = dash.pattern.as_slice();
    if pattern.is_empty() {
        return None;
    }
    let mut sum = 0.0;
    for &d in pattern {
        if !d.is_finite() || d < 0.0 {
            return None;
        }
        sum += d;
    }
    if sum <= 0.0 || !sum.is_finite() {
        return None;
    }
    let offset = if dash.offset.is_finite() {
        dash.offset
    } else {
        debug_assert!(false, "dash offset must be finite, got {}", dash.offset);
        0.0
    };
    // "On" entries sit at even indices. With an odd-length pattern the
    // repetition flips parity, so every entry takes both roles.
    let has_zero_dashes = pattern
        .iter()
        .enumerate()
        .any(|(i, &d)| d == 0.0 && (pattern.len() % 2 == 1 || i % 2 == 0));
    Some(SanitizedDash {
        pattern,
        offset,
        cap: dash.cap,
        has_zero_dashes,
    })
}

/// Dash one subpath, or one split piece, and expand every dash with the
/// given band parameters.
///
/// `params.start_cap` and `end_cap` are the style's end caps. They apply
/// only to the leading or trailing edge of the dashes that touch
/// `true_start` and `true_end`, the original subpath's endpoints. Those are
/// `None` when this piece has no true endpoint, as for an arc cut at a
/// crossing. Every other dash edge gets `dash.cap`.
///
/// `phase` places a split piece in its original subpath's pattern.
#[allow(clippy::too_many_arguments)]
pub(crate) fn expand_dashed_subpath(
    subpath: &[PathEl],
    closed: bool,
    dash: &SanitizedDash<'_>,
    params: BandParams<'_>,
    phase: Phase,
    true_start: Option<Point>,
    true_end: Option<Point>,
    clip: Option<DashClip<'_>>,
    work: &mut DashWork,
    left: &mut BezPath,
    right: &mut BezPath,
    scratch: &mut BezPath,
    out: &mut BezPath,
) {
    let DashWork {
        group,
        band: band_buf,
        clip: clip_work,
    } = work;
    // kurbo reads the offset like SVG's `stroke-dashoffset`: at arc length
    // `s` the pattern is at `offset + s`. So a piece that starts `s0` into
    // the original subpath starts `offset + s0` into the pattern.
    let offset = dash.offset + phase.s0;

    // kurbo restarts the pattern at the start point of a closed subpath. A
    // piece that runs through that point is dashed in two parts: the part
    // before it continues from `s0`, the part after starts again at arc
    // length 0. The dash that crosses the point is joined back into one
    // further down, as kurbo's seam-merge does.
    let (before, after) = match phase.seam {
        Some(k) if k >= 2 && k < subpath.len() => {
            let seam_pt = subpath[k - 1].end_point().unwrap_or(Point::ORIGIN);
            let mut after = Vec::with_capacity(subpath.len() - k + 1);
            after.push(PathEl::MoveTo(seam_pt));
            after.extend_from_slice(&subpath[k..]);
            (&subpath[..k], Some((after, seam_pt)))
        }
        _ => (subpath, None),
    };

    // Zero-length dashes become dots, synthesized separately: the dash
    // stream represents them as degenerate pairs with no tangent.
    // Arc-length cutting noise can fatten an exact-zero dash into a sliver
    // around 1e-9 long, whose caps would render a duplicate dot. So when
    // the pattern contains zeros, negligibly short dash groups are dropped
    // in favour of the synthesized dot at that position.
    let mut tiny_group_threshold = 0.0;
    if dash.has_zero_dashes && dash.cap != Cap::Butt {
        let length = |els: &[PathEl]| -> f64 {
            kurbo::segments(non_degenerate(els))
                .map(|seg| math::arclen(&seg, DASH_ACCURACY))
                .sum()
        };
        let before_len = length(before);
        let after_len = after.as_ref().map_or(0.0, |(els, _)| length(els));
        // A dot exactly at the end of a part belongs to whatever starts
        // there: the start of an unsplit closed contour, the part after the
        // start point, or the next piece at a crossing. Only the true end of
        // an open path draws one.
        let dots = |buf: &mut BezPath| {
            let before_shares_end = closed || after.is_some() || phase.shares_end;
            synthesize_dots(
                before,
                before_shares_end,
                dash,
                &params,
                offset,
                before_len,
                buf,
            );
            if let Some((els, _)) = &after {
                synthesize_dots(
                    els,
                    phase.shares_end,
                    dash,
                    &params,
                    dash.offset,
                    after_len,
                    buf,
                );
            }
        };
        match clip {
            Some(c) => {
                band_buf.truncate(0);
                dots(band_buf);
                crate::clip::clip_band(
                    band_buf,
                    c.source,
                    c.keep_inside,
                    c.accuracy,
                    clip_work,
                    out,
                );
                band_buf.truncate(0);
            }
            None => dots(out),
        }
        tiny_group_threshold = 1e-6 * (before_len + after_len).max(1.0);
    }

    // The pipeline's resolution along this subpath. A dash shorter than
    // this is rounding noise: when the phase lands a hair past a pattern
    // boundary at a segment end, kurbo starts a dash about 1e-14 long, and
    // its caps would draw a stray dot. Also, kurbo ends a dash on a line by
    // evaluating the segment at `t = 1`, which can land an ulp short of the
    // subpath's end point. A dash edge this close to a true endpoint counts
    // as that endpoint and keeps its cap.
    let noise = crate::normalize::RESOLUTION * crate::normalize::extent(subpath);
    let is_true_end = |pt: Option<Point>, end: Option<Point>| match (pt, end) {
        (Some(pt), Some(end)) => (pt - end).hypot() <= noise,
        _ => false,
    };

    // Stream the dashed elements, grouping per dash; each dash starts with
    // a MoveTo. kurbo may emit the zero-length dash last, out of its
    // seam-merge stash, so caps are assigned geometrically rather than by
    // position in the stream.
    let mut flush = |group: &mut Vec<PathEl>| {
        drop_unplaced(group);
        if group.is_empty() {
            return;
        }
        let len = group_len(group);
        if len <= noise || (tiny_group_threshold > 0.0 && len < tiny_group_threshold) {
            group.clear();
            return;
        }
        let mut p = params;
        if matches!(group.last(), Some(PathEl::ClosePath)) {
            // Gap-free closed contour: expand as a ring, no caps involved.
        } else if closed {
            p.start_cap = dash.cap;
            p.end_cap = dash.cap;
        } else {
            let leading = match group.first() {
                Some(PathEl::MoveTo(pt)) => Some(*pt),
                _ => None,
            };
            let trailing = group.last().and_then(|el| el.end_point());
            p.start_cap = if is_true_end(leading, true_start) {
                params.start_cap
            } else {
                dash.cap
            };
            p.end_cap = if is_true_end(trailing, true_end) {
                params.end_cap
            } else {
                dash.cap
            };
        }
        match clip {
            // Expand into a scratch buffer so the mask sees one dash at a
            // time, then intersect it with the fill, or subtract it.
            Some(c) => {
                band_buf.truncate(0);
                {
                    let mut band = Band::new(p, left, right, scratch, band_buf);
                    band.run(group.iter().copied());
                }
                crate::clip::clip_band(
                    band_buf,
                    c.source,
                    c.keep_inside,
                    c.accuracy,
                    clip_work,
                    out,
                );
                band_buf.truncate(0);
            }
            None => {
                let mut band = Band::new(p, left, right, scratch, out);
                band.run(group.iter().copied());
            }
        }
        group.clear();
    };

    group.clear();
    let Some((after, seam_pt)) = after else {
        for el in kurbo::dash(non_degenerate(subpath), offset, dash.pattern) {
            if matches!(el, PathEl::MoveTo(_)) {
                flush(group);
            }
            group.push(el);
        }
        flush(group);
        return;
    };

    // Only pieces of closed self-intersecting subpaths get here, so
    // collecting the dashes of both parts first costs little.
    let mut before_dashes = dash_groups(before, offset, dash.pattern);
    let mut after_dashes = dash_groups(&after, dash.offset, dash.pattern);
    // The dash running into the start point can end an ulp off it, as
    // above, so it is matched within `noise`. The dash running out of it
    // starts exactly there, with that point as its `MoveTo`.
    let into = before_dashes.iter().position(|g| {
        g.last()
            .and_then(|el| el.end_point())
            .is_some_and(|p| (p - seam_pt).hypot() <= noise)
    });
    // kurbo joins the dash running into the start point only to the dash it
    // stashed there. That dash exists only when the pattern is on at the
    // start, and on an open run kurbo emits it last. Another dash can also
    // start there, when the offset sits exactly on the end of a gap or
    // after a gap of length zero. kurbo keeps that one separate.
    let out_of = starts_on(dash)
        .then(|| {
            after_dashes
                .iter()
                .rposition(|g| g.first() == Some(&PathEl::MoveTo(seam_pt)))
        })
        .flatten();
    if let (Some(i), Some(j)) = (into, out_of) {
        let continued = after_dashes.remove(j);
        before_dashes[i].extend(continued.into_iter().skip(1));
    }
    for mut g in before_dashes.into_iter().chain(after_dashes) {
        flush(&mut g);
    }
}

/// The dashes of `els` as separate element runs, each led by its `MoveTo`.
fn dash_groups(els: &[PathEl], offset: f64, pattern: &[f64]) -> Vec<Vec<PathEl>> {
    let mut groups: Vec<Vec<PathEl>> = Vec::new();
    for el in kurbo::dash(non_degenerate(els), offset, pattern) {
        match (el, groups.last_mut()) {
            (PathEl::MoveTo(_), _) | (_, None) => groups.push(alloc::vec![el]),
            (_, Some(g)) => g.push(el),
        }
    }
    for g in &mut groups {
        drop_unplaced(g);
    }
    groups.retain(|g| !g.is_empty());
    groups
}

/// Whether the pattern is on at the start of the path. If it is,
/// `kurbo::dash` begins with a dash it stashes for the seam. This copies
/// kurbo's phase search: a dash whose remaining length comes out exactly
/// zero still counts as on.
fn starts_on(dash: &SanitizedDash<'_>) -> bool {
    let pattern = dash.pattern;
    let period: f64 = pattern.iter().sum::<f64>() * if pattern.len() % 2 == 1 { 2.0 } else { 1.0 };
    let mut ix = 0;
    let mut remaining = pattern[0] - math::rem_euclid(dash.offset, period);
    let mut on = true;
    while remaining < 0.0 {
        ix = (ix + 1) % pattern.len();
        remaining += pattern[ix];
        on = !on;
    }
    on
}

/// Drop the elements `kurbo::dash` could not place.
///
/// When a zero-length pattern entry lands exactly on a segment end,
/// rounding can leave a sliver of the segment after its parameter has
/// already reached 1. kurbo then measures the empty rest as `0 / 0`, and
/// the element comes out `NaN`. It covers no distance, so it is dropped. A
/// dash whose `MoveTo` is `NaN` is dropped whole, so it does not join the
/// dash before it.
fn drop_unplaced(group: &mut Vec<PathEl>) {
    group.retain(|el| el.is_finite());
    if !matches!(group.first(), Some(PathEl::MoveTo(_))) {
        group.clear();
    }
}

/// Drop drawing elements that cover no distance.
///
/// [`kurbo::dash`] walks the element stream by arc length. A zero-length
/// quad or cubic carries no direction and its arc-length inverse is
/// degenerate, which stalls the pattern phase: everything past such an
/// element comes back as one uninterrupted dash. Filtering them out first
/// is free, since the band expansion skips them anyway, and it keeps the
/// dash lengths and phase exactly as they would be on the cleaned path.
fn non_degenerate(els: &[PathEl]) -> impl Iterator<Item = PathEl> + '_ {
    use kurbo::{CubicBez, Line, PathSeg, QuadBez};
    let mut cur: Option<Point> = None;
    els.iter().copied().filter(move |el| {
        let keep = match (cur, el) {
            (Some(p0), PathEl::LineTo(p)) => {
                !crate::math::is_degenerate(&PathSeg::Line(Line::new(p0, *p)))
            }
            (Some(p0), PathEl::QuadTo(p1, p2)) => {
                !crate::math::is_degenerate(&PathSeg::Quad(QuadBez::new(p0, *p1, *p2)))
            }
            (Some(p0), PathEl::CurveTo(p1, p2, p3)) => {
                !crate::math::is_degenerate(&PathSeg::Cubic(CubicBez::new(p0, *p1, *p2, *p3)))
            }
            _ => true,
        };
        if let Some(p) = el.end_point() {
            cur = Some(p);
        }
        keep
    })
}

/// Length of a dash group's control polygon. It is never less than the arc
/// length, and zero only for a dash that covers no distance. Chords between
/// end points are not enough: a dash that runs once around a loop cut off
/// at a crossing can be a single curve that ends where it starts, so its
/// chord is zero.
fn group_len(group: &[PathEl]) -> f64 {
    let mut len = 0.0;
    let mut last: Option<Point> = None;
    for el in group {
        let points: &[Point] = match el {
            PathEl::MoveTo(p) => {
                last = Some(*p);
                continue;
            }
            PathEl::LineTo(p) => &[*p],
            PathEl::QuadTo(p1, p2) => &[*p1, *p2],
            PathEl::CurveTo(p1, p2, p3) => &[*p1, *p2, *p3],
            PathEl::ClosePath => continue,
        };
        for &p in points {
            if let Some(prev) = last {
                len += (p - prev).hypot();
            }
            last = Some(p);
        }
    }
    len
}

/// Emit dots for zero-length dashes.
///
/// kurbo's dash stream represents a zero-length dash as a degenerate
/// `MoveTo p, LineTo p` pair. It carries no tangent, and the band expansion
/// rightly skips it. Positions are recomputed here by pattern arithmetic
/// and then located on the source segments, which also yields the tangent
/// needed to offset the band midpoint and orient square dots.
fn synthesize_dots(
    subpath: &[PathEl],
    shares_end: bool,
    dash: &SanitizedDash<'_>,
    params: &BandParams<'_>,
    effective_offset: f64,
    total: f64,
    out: &mut BezPath,
) {
    // Effective even-length pattern (odd patterns repeat with flipped parity).
    let n = dash.pattern.len();
    let doubled = n % 2 == 1;
    let eff_len = if doubled { 2 * n } else { n };
    let entry = |i: usize| dash.pattern[i % n];
    let period: f64 = dash.pattern.iter().sum::<f64>() * if doubled { 2.0 } else { 1.0 };
    let offset = math::rem_euclid(effective_offset, period);

    if total <= 0.0 || !total.is_finite() {
        return;
    }

    // Cumulative start positions of "on" entries with zero length, phased by
    // the offset, repeated over the path length.
    // `offset ∈ [0, period)`, so the cursor starts within one period of 0.
    let mut cursor = -offset;
    let mut i = 0usize;
    let mut positions: Vec<f64> = Vec::new();
    let mut guard = 0usize;
    let max_iters = eff_len * (math::ceil(total / period) as usize + 2) + eff_len;
    while cursor <= total && guard < max_iters {
        let len = entry(i);
        let is_on = i % 2 == 0;
        if is_on && len == 0.0 && cursor >= 0.0 {
            // A dot at position `total` belongs to whatever starts there: on
            // a closed contour, position 0. The test allows for rounding.
            // The cursor and `total` sum different lengths, so a dot a hair
            // short of the end is still the dot at the shared point.
            if !(shares_end && cursor >= total - 1e-9 * total.max(1.0)) {
                positions.push(cursor);
            }
        }
        cursor += len;
        i = (i + 1) % eff_len;
        guard += 1;
    }

    if positions.is_empty() {
        return;
    }

    // Locate each position on the source segments.
    let half = 0.5 * params.width();
    let mid_shift = 0.5 * (params.d_right - params.d_left);
    let mut pos_ix = 0;
    let mut acc = 0.0;
    for seg in kurbo::segments(non_degenerate(subpath)) {
        if pos_ix >= positions.len() {
            break;
        }
        let len = math::arclen(&seg, DASH_ACCURACY);
        while pos_ix < positions.len() {
            let s = positions[pos_ix];
            if s > acc + len && !(s >= total && acc + len >= total) {
                break;
            }
            let local = (s - acc).clamp(0.0, len);
            let t = seg.inv_arclen(local, DASH_ACCURACY);
            let pt = seg.eval(t);
            let tan = seg_tangent(&seg, t);
            emit_dot(out, pt, tan, half, mid_shift, dash.cap, params.tolerance);
            pos_ix += 1;
        }
        acc += len;
    }
}

/// Tangent of a segment at parameter `t`, with degenerate fallbacks.
fn seg_tangent(seg: &kurbo::PathSeg, t: f64) -> Vec2 {
    let d = match seg {
        kurbo::PathSeg::Line(l) => l.p1 - l.p0,
        kurbo::PathSeg::Quad(q) => q.deriv().eval(t).to_vec2(),
        kurbo::PathSeg::Cubic(c) => c.deriv().eval(t).to_vec2(),
    };
    if d.hypot2() > 1e-24 {
        d
    } else {
        let (t0, t1) = crate::expand::tangents(seg);
        if t < 0.5 { t0 } else { t1 }
    }
}

/// One dot: a circle (round cap) or an oriented square (square cap) of
/// diameter `2·half`, centered on the band midpoint.
fn emit_dot(
    out: &mut BezPath,
    pt: Point,
    tan: Vec2,
    half: f64,
    mid_shift: f64,
    cap: Cap,
    tolerance: f64,
) {
    let h2 = tan.hypot2();
    if h2 <= 0.0 || !h2.is_finite() {
        // No orientation: still emit a centered round dot; skip squares.
        if cap == Cap::Round {
            out.extend(kurbo::Circle::new(pt, half).path_elements(tolerance));
        }
        return;
    }
    let t_unit = tan * (1.0 / math::sqrt(h2));
    let n_unit = t_unit.turn_90();
    let mid = pt + mid_shift * n_unit;
    match cap {
        Cap::Round => {
            out.extend(kurbo::Circle::new(mid, half).path_elements(tolerance));
        }
        Cap::Square => {
            let ht = half * t_unit;
            let hn = half * n_unit;
            out.move_to(mid - ht - hn);
            out.line_to(mid + ht - hn);
            out.line_to(mid + ht + hn);
            out.line_to(mid - ht + hn);
            out.close_path();
        }
        Cap::Butt => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::DashStyle;

    #[test]
    fn sanitize_rejects_bad_patterns() {
        assert!(sanitize(&DashStyle::from_pattern([])).is_none());
        assert!(sanitize(&DashStyle::from_pattern([0.0, 0.0])).is_none());
        assert!(sanitize(&DashStyle::from_pattern([4.0, -2.0])).is_none());
        assert!(sanitize(&DashStyle::from_pattern([4.0, f64::NAN])).is_none());
        assert!(sanitize(&DashStyle::from_pattern([4.0, f64::INFINITY])).is_none());
        assert!(sanitize(&DashStyle::from_pattern([4.0, 2.0])).is_some());
    }

    #[test]
    fn sanitize_normalizes_offset() {
        let mut d = DashStyle::new(4.0, 2.0);
        d.offset = f64::NAN;
        // debug builds assert; release builds fall back to 0. Test the
        // release behavior only when debug_assertions are off.
        if !cfg!(debug_assertions) {
            let s = sanitize(&d).unwrap();
            assert_eq!(s.offset, 0.0);
        }
    }

    #[test]
    fn detects_zero_dashes() {
        assert!(
            sanitize(&DashStyle::from_pattern([0.0, 5.0]))
                .unwrap()
                .has_zero_dashes
        );
        assert!(
            !sanitize(&DashStyle::from_pattern([5.0, 0.0]))
                .unwrap()
                .has_zero_dashes
        );
        // Odd-length: every entry takes both roles.
        assert!(
            sanitize(&DashStyle::from_pattern([5.0, 5.0, 0.0]))
                .unwrap()
                .has_zero_dashes
        );
    }
}
