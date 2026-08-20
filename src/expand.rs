// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! One-sided/asymmetric stroke band expansion.
//!
//! This is a re-derivation of kurbo's private stroke machinery
//! (`stroke.rs`, `StrokeCtx`) generalized from the symmetric `±width/2`
//! offsets to independent per-side distances `(d_left, d_right)`:
//!
//! - the **left** boundary accumulates points offset by `−d_left·n̂`,
//! - the **right** boundary accumulates points offset by `+d_right·n̂`,
//!
//! where `n̂ = t̂.turn_90()` is the unit right normal (Y-down). With
//! `(w/2, w/2)` this reproduces centered stroking; with `(0, w)` or `(w, 0)`
//! one boundary is the source path itself, copied exactly.
//!
//! Differences from kurbo's stroker, on purpose:
//! - join/cap arcs honor the caller's tolerance (kurbo hardcodes `1e-3`,
//!   marked "TODO: scale" upstream);
//! - a boundary at distance `0` copies source segments verbatim instead of
//!   calling the offsetter, and skips join emission (its corner *is* the
//!   joint);
//! - round start caps are followed by an explicit `ClosePath`.
//!
//! The output is meaningful under the **nonzero winding rule** only.

use core::f64::consts::PI;

use kurbo::common::solve_quadratic;
use kurbo::offset::offset_cubic;
use kurbo::{Affine, Arc, BezPath, Cap, CubicBez, Join, PathEl, PathSeg, Point, QuadBez, Vec2};

use crate::math;

/// Per-run parameters of the band expansion.
#[derive(Copy, Clone)]
pub(crate) struct BandParams<'c> {
    pub d_left: f64,
    pub d_right: f64,
    pub join: Join,
    pub miter_limit: f64,
    pub start_cap: Cap,
    pub end_cap: Cap,
    pub tolerance: f64,
    /// Join-omission threshold, `2·tolerance / (d_left + d_right)`
    /// (kurbo's `join_thresh` with `width = d_left + d_right`).
    pub join_thresh: f64,
    /// Self-intersection vertices of the piece's source subpath. At these
    /// corners a one-sided band on the *outer* side of the turn suppresses
    /// its join fan and routes through the vertex, so sibling lobes tile the
    /// crossing without invading each other's fill (Figma parity).
    pub crossings: &'c [Point],
    /// Match radius for `crossings` (split accuracy scaled).
    pub crossing_eps: f64,
    /// Emit the *mathematically raw* offset: no inner-side corner clip and
    /// no crossing-tip handling, so reflex corners keep their overshoot
    /// loops. The region pruner (see `region`) removes them by distance,
    /// which is exact where the local clip is only an approximation.
    pub raw_offset: bool,
}

impl BandParams<'_> {
    pub(crate) fn width(&self) -> f64 {
        self.d_left + self.d_right
    }

    fn at_crossing(&self, p: Point) -> bool {
        let e2 = self.crossing_eps * self.crossing_eps;
        self.crossings.iter().any(|c| (*c - p).hypot2() <= e2)
    }
}

/// Working state of one expansion run. Buffers are borrowed from the caller's
/// context so repeated runs reuse allocations.
pub(crate) struct Band<'a> {
    pub(crate) p: BandParams<'a>,
    left: &'a mut BezPath,
    right: &'a mut BezPath,
    scratch: &'a mut BezPath,
    out: &'a mut BezPath,
    start_pt: Point,
    start_tan: Vec2,
    /// Unit right normal at the subpath start (for the start cap).
    start_norm: Vec2,
    last_pt: Point,
    last_tan: Vec2,
    /// When set, `finish_*` computes joins/reanchoring but emits nothing and
    /// leaves the boundary accumulators intact — used by the width-clamp
    /// probe to inspect a candidate offset boundary.
    pub(crate) suppress_finish: bool,
    /// Segment-index ranges of *outer-side* join geometry emitted in
    /// `raw_offset` mode, each with the fraction of `w` it may legitimately
    /// come within of its corner: `1` for round joins and miters (both stay
    /// at or beyond `w`), `cos(φ/2)` for a bevel chord, which cuts the
    /// corner. The region pruner relaxes the distance test by that factor —
    /// bounded, so inner-side overshoot at extreme widths stays prunable.
    pub(crate) left_join_segs: alloc::vec::Vec<(u32, u32, f64)>,
    pub(crate) right_join_segs: alloc::vec::Vec<(u32, u32, f64)>,
}

impl<'a> Band<'a> {
    pub(crate) fn new(
        p: BandParams<'a>,
        left: &'a mut BezPath,
        right: &'a mut BezPath,
        scratch: &'a mut BezPath,
        out: &'a mut BezPath,
    ) -> Self {
        left.truncate(0);
        right.truncate(0);
        Band {
            p,
            left,
            right,
            scratch,
            out,
            start_pt: Point::ORIGIN,
            start_tan: Vec2::ZERO,
            start_norm: Vec2::ZERO,
            last_pt: Point::ORIGIN,
            last_tan: Vec2::ZERO,
            suppress_finish: false,
            left_join_segs: alloc::vec::Vec::new(),
            right_join_segs: alloc::vec::Vec::new(),
        }
    }

    /// Expand a stream of path elements (one or more subpaths, all sharing
    /// this band's caps). Mirrors kurbo's `stroke_undashed` loop.
    pub(crate) fn run(&mut self, els: impl IntoIterator<Item = PathEl>) {
        for el in els {
            let p0 = self.last_pt;
            match el {
                PathEl::MoveTo(p) => {
                    self.finish_open();
                    self.start_pt = p;
                    self.last_pt = p;
                }
                PathEl::LineTo(p1) => {
                    if p1 != p0 {
                        let tangent = p1 - p0;
                        self.do_join(tangent);
                        self.last_tan = tangent;
                        self.do_line(tangent, p1);
                    }
                }
                PathEl::QuadTo(p1, p2) => {
                    if p1 != p0 || p2 != p0 {
                        let q = QuadBez::new(p0, p1, p2);
                        let (tan0, tan1) = tangents(&PathSeg::Quad(q));
                        self.do_join(tan0);
                        self.do_cubic(q.raise());
                        self.last_tan = tan1;
                    }
                }
                PathEl::CurveTo(p1, p2, p3) => {
                    if p1 != p0 || p2 != p0 || p3 != p0 {
                        let c = CubicBez::new(p0, p1, p2, p3);
                        let (tan0, tan1) = tangents(&PathSeg::Cubic(c));
                        self.do_join(tan0);
                        self.do_cubic(c);
                        self.last_tan = tan1;
                    }
                }
                PathEl::ClosePath => {
                    if p0 != self.start_pt {
                        let tangent = self.start_pt - p0;
                        self.do_join(tangent);
                        self.last_tan = tangent;
                        self.do_line(tangent, self.start_pt);
                    }
                    self.finish_closed();
                }
            }
        }
        self.finish_open();
    }

    /// Unit right normal of a (nonzero) tangent.
    fn unit_norm(tan: Vec2) -> Vec2 {
        tan.turn_90() * (1.0 / tan.hypot())
    }

    fn do_line(&mut self, tangent: Vec2, p1: Point) {
        let n = Self::unit_norm(tangent);
        self.left.line_to(p1 - self.p.d_left * n);
        self.right.line_to(p1 + self.p.d_right * n);
        self.last_pt = p1;
    }

    fn do_cubic(&mut self, c: CubicBez) {
        // Degenerate-linear detection, ported from kurbo: project the control
        // points onto a reference chord; if the polygon is monotone and flat
        // within tolerance, the cubic is treated as a (possibly cusped) line —
        // offset_cubic's documented precondition.
        let chord = c.p3 - c.p0;
        let mut chord_ref = chord;
        let mut chord_ref_hypot2 = chord_ref.hypot2();
        let d01 = c.p1 - c.p0;
        if d01.hypot2() > chord_ref_hypot2 {
            chord_ref = d01;
            chord_ref_hypot2 = chord_ref.hypot2();
        }
        let d23 = c.p3 - c.p2;
        if d23.hypot2() > chord_ref_hypot2 {
            chord_ref = d23;
            chord_ref_hypot2 = chord_ref.hypot2();
        }
        let p0 = c.p0.to_vec2().dot(chord_ref);
        let p1 = c.p1.to_vec2().dot(chord_ref);
        let p2 = c.p2.to_vec2().dot(chord_ref);
        let p3 = c.p3.to_vec2().dot(chord_ref);
        const ENDPOINT_D: f64 = 0.01;
        if p3 <= p0
            || p1 > p2
            || p1 < p0 + ENDPOINT_D * (p3 - p0)
            || p2 > p3 - ENDPOINT_D * (p3 - p0)
        {
            let x01 = d01.cross(chord_ref);
            let x23 = d23.cross(chord_ref);
            let x03 = chord.cross(chord_ref);
            let thresh = self.p.tolerance * self.p.tolerance * chord_ref_hypot2;
            if x01 * x01 < thresh && x23 * x23 < thresh && x03 * x03 < thresh {
                // Control points nearly collinear: handle as line(s).
                let midpoint = c.p0.midpoint(c.p3);
                let ref_vec = chord_ref * (1.0 / chord_ref_hypot2);
                let ref_pt = midpoint - 0.5 * (p0 + p3) * ref_vec;
                self.do_linear(c, [p0, p1, p2, p3], ref_pt, ref_vec);
                return;
            }
        }

        if self.p.d_left == 0.0 {
            self.left.curve_to(c.p1, c.p2, c.p3);
        } else {
            offset_cubic(c, -self.p.d_left, self.p.tolerance, self.scratch);
            heal_start(self.left, self.scratch);
            self.left.extend(self.scratch.iter().skip(1));
        }
        if self.p.d_right == 0.0 {
            self.right.curve_to(c.p1, c.p2, c.p3);
        } else {
            offset_cubic(c, self.p.d_right, self.p.tolerance, self.scratch);
            heal_start(self.right, self.scratch);
            self.right.extend(self.scratch.iter().skip(1));
        }
        self.last_pt = c.p3;
    }

    /// A cubic that is actually linear, with possible interior cusps.
    ///
    /// `p` holds the control points projected onto the reference chord;
    /// `ref_pt`/`ref_vec` map projections back to points. Cusps are modeled
    /// as the limit of finite curvature: forced round joins (Nehab).
    fn do_linear(&mut self, c: CubicBez, p: [f64; 4], ref_pt: Point, ref_vec: Vec2) {
        let saved_join = self.p.join;
        self.p.join = Join::Round;

        let (tan0, tan1) = tangents(&PathSeg::Cubic(c));
        self.last_tan = tan0;
        // dx/dt roots of the projected cubic = cusp candidates.
        let c0 = p[1] - p[0];
        let c1 = 2.0 * p[2] - 4.0 * p[1] + 2.0 * p[0];
        let c2 = p[3] - 3.0 * p[2] + 3.0 * p[1] - p[0];
        let roots = solve_quadratic(c0, c1, c2);
        const EPSILON: f64 = 1e-6;
        for t in roots {
            if t > EPSILON && t < 1.0 - EPSILON {
                let mt = 1.0 - t;
                let z = mt * (mt * mt * p[0] + 3.0 * t * (mt * p[1] + t * p[2])) + t * t * t * p[3];
                let p = ref_pt + z * ref_vec;
                if p != self.last_pt {
                    let tan = p - self.last_pt;
                    self.do_join(tan);
                    self.do_line(tan, p);
                    self.last_tan = tan;
                }
            }
        }
        if c.p3 != self.last_pt {
            let tan = c.p3 - self.last_pt;
            self.do_join(tan);
            self.do_line(tan, c.p3);
            self.last_tan = tan;
        }
        self.do_join(tan1);

        self.p.join = saved_join;
    }

    fn do_join(&mut self, tan0: Vec2) {
        let n = Self::unit_norm(tan0);
        let nl = self.p.d_left * n;
        let nr = self.p.d_right * n;
        let p0 = self.last_pt;
        if self.left.elements().is_empty() {
            self.left.move_to(p0 - nl);
            self.right.move_to(p0 + nr);
            self.start_tan = tan0;
            self.start_norm = n;
            return;
        }
        let ab = self.last_tan;
        let cd = tan0;
        let cross = ab.cross(cd);
        let dot = ab.dot(cd);
        let hypot = math::hypot(cross, dot);
        // Omit the join entirely for nearly straight continuations.
        if dot > 0.0 && cross.abs() < hypot * self.p.join_thresh {
            return;
        }
        // One-sided bands: when the offset side is the *inner* side of this
        // turn and the opposite boundary rides the source path (d == 0), the
        // strip-end points would poke past the adjacent segment — leaking
        // paint across the source boundary (an inside stroke escaping the
        // fill). Clip the boundary at the offset-line intersection instead.
        // The styled join belongs to the outer side, which has no geometry
        // here, so the clip replaces the whole join. When the clip point
        // overshoots the neighboring strips (segments short relative to the
        // width — tight polyline corners), fall back to kurbo's additive
        // routing through the joint center: it may overpaint, but overlaps
        // add winding instead of cancelling.
        let one_sided_outer_left =
            !self.p.raw_offset && cross > 0.0 && self.p.d_left > 0.0 && self.p.d_right == 0.0;
        let one_sided_outer_right =
            !self.p.raw_offset && cross < 0.0 && self.p.d_right > 0.0 && self.p.d_left == 0.0;
        if (one_sided_outer_left || one_sided_outer_right) && self.p.at_crossing(p0) {
            // Crossing tip: no join fan; route through the vertex so the
            // sibling lobe's band can tile the other side of the crossing.
            if one_sided_outer_left {
                self.left.line_to(p0);
                self.left.line_to(p0 - nl);
            } else {
                self.right.line_to(p0);
                self.right.line_to(p0 + nr);
            }
            return;
        }
        // Raw-offset mode: remember which segments the outer-side join is
        // about to emit, so the region pruner can exempt them from the
        // distance test (see the field docs).
        let tag_left = self.p.raw_offset && cross > 0.0 && self.p.d_left > 0.0;
        let tag_right = self.p.raw_offset && cross < 0.0 && self.p.d_right > 0.0;
        let (l0, r0) = (self.left.elements().len(), self.right.elements().len());
        // How close to its corner this join's geometry may legitimately come,
        // as a fraction of the band width. A bevel chord between two points
        // at distance `w` passes at `w·cos(φ/2)`; round fans and miter legs
        // never come inside `w`.
        let miter_ok = 2.0 * hypot < (hypot + dot) * self.p.miter_limit * self.p.miter_limit;
        let join_relax = match self.p.join {
            Join::Round => 1.0,
            Join::Miter if miter_ok => 1.0,
            _ => {
                let c = if hypot > 0.0 {
                    ((1.0 + dot / hypot) * 0.5).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                math::sqrt(c)
            }
        };
        if self.p.raw_offset {
            // Raw mode: fall through to the plain join emission below, which
            // bevels the inner side (the overshoot loop the pruner needs).
        } else if cross > 0.0 && self.p.d_left == 0.0 && self.p.d_right > 0.0 {
            // Right side is inner (right turn).
            if self.inner_clip(ab, cd, cross, p0, nr, false) {
                return;
            }
            self.right.line_to(p0);
        } else if cross < 0.0 && self.p.d_right == 0.0 && self.p.d_left > 0.0 {
            // Left side is inner (left turn).
            if self.inner_clip(ab, cd, cross, p0, nl, true) {
                return;
            }
            self.left.line_to(p0);
        }
        match self.p.join {
            Join::Bevel => {
                self.bevel_to(p0, nl, nr);
            }
            Join::Miter => {
                if miter_ok {
                    // Miter on the outer side; the inner side routes through
                    // the joint center (nonzero winding cancels the overlap).
                    if cross > 0.0 && self.p.d_left > 0.0 {
                        let last_n = Self::unit_norm(ab);
                        let fp_last = p0 - self.p.d_left * last_n;
                        let fp_this = p0 - nl;
                        let h = ab.cross(fp_this - fp_last) / cross;
                        let miter_pt = fp_this - cd * h;
                        self.left.line_to(miter_pt);
                        if self.p.d_right > 0.0 {
                            self.right.line_to(p0);
                        }
                    } else if cross < 0.0 && self.p.d_right > 0.0 {
                        let last_n = Self::unit_norm(ab);
                        let fp_last = p0 + self.p.d_right * last_n;
                        let fp_this = p0 + nr;
                        let h = ab.cross(fp_this - fp_last) / cross;
                        let miter_pt = fp_this - cd * h;
                        self.right.line_to(miter_pt);
                        if self.p.d_left > 0.0 {
                            self.left.line_to(p0);
                        }
                    }
                }
                self.bevel_to(p0, nl, nr);
            }
            Join::Round => {
                let angle = math::atan2(cross, dot);
                if angle > 0.0 {
                    // Left side is outer.
                    if self.p.d_right > 0.0 {
                        self.right.line_to(p0 + nr);
                    }
                    if self.p.d_left > 0.0 {
                        round_join(self.left, self.p.tolerance, p0, nl, angle);
                    }
                } else {
                    if self.p.d_left > 0.0 {
                        self.left.line_to(p0 - nl);
                    }
                    if self.p.d_right > 0.0 {
                        round_join_rev(self.right, self.p.tolerance, p0, -nr, -angle);
                    }
                }
            }
        }
        // Segment index = element index − 1 (the leading MoveTo).
        if tag_left && self.left.elements().len() > l0 {
            self.left_join_segs.push((
                l0 as u32 - 1,
                self.left.elements().len() as u32 - 1,
                join_relax,
            ));
        }
        if tag_right && self.right.elements().len() > r0 {
            self.right_join_segs.push((
                r0 as u32 - 1,
                self.right.elements().len() as u32 - 1,
                join_relax,
            ));
        }
    }

    /// Inner-side clip for one-sided bands (see `do_join`).
    ///
    /// Replaces the strip-end points around the corner with the intersection
    /// of the two offset lines: `X = fp_this − cd·h`, the same construction
    /// as the outer miter but on the inner side and unlimited. For a corner
    /// between two line segments this is exact — the incoming strip end
    /// (a `LineTo`) is rewritten to `X` and the outgoing offset line passes
    /// through `X` on its way to the next point. Curve neighbors re-connect
    /// via a healing line in `do_cubic`, keeping a bounded remnant of the
    /// poke (documented limitation).
    ///
    /// Returns `false` (caller falls back to the regular join) for
    /// near-reversals, where the intersection shoots off to infinity.
    fn inner_clip(
        &mut self,
        ab: Vec2,
        cd: Vec2,
        cross: f64,
        p0: Point,
        n_new: Vec2,
        left: bool,
    ) -> bool {
        let d = if left { self.p.d_left } else { self.p.d_right };
        let sign = if left { -1.0 } else { 1.0 };
        let last_n = Self::unit_norm(ab);
        let fp_last = p0 + sign * d * last_n;
        let fp_this = p0 + sign * n_new;
        let delta = fp_this - fp_last;
        // X = fp_last + ab·g = fp_this + cd·(−h). The clip is only valid when
        // it lands within both adjacent strips (g, h ∈ [−1, 0], measured in
        // tangent lengths — exact for line neighbors); overshoot means the
        // segments are short relative to the width and cutting would zigzag
        // the boundary backwards, creating winding-cancellation pockets.
        let h = ab.cross(delta) / cross;
        let g = delta.cross(cd) / cross;
        const SLACK: f64 = 1e-9;
        if !(-1.0 - SLACK..=SLACK).contains(&h) || !(-1.0 - SLACK..=SLACK).contains(&g) {
            return false;
        }
        let x = fp_this - cd * h;
        // Reversal guard: near 180° the intersection runs away; the poke
        // there is the U-turn band itself, handled by the regular join.
        const CLIP_LIMIT: f64 = 1e4;
        if !x.is_finite() || (x - p0).hypot2() > (CLIP_LIMIT * d) * (CLIP_LIMIT * d) {
            return false;
        }
        let side = if left {
            &mut *self.left
        } else {
            &mut *self.right
        };
        if let Some(PathEl::LineTo(p)) = side.elements_mut().last_mut() {
            *p = x;
        } else {
            side.line_to(x);
        }
        true
    }

    /// Bevel connection to the new segment's start offsets (both sides).
    fn bevel_to(&mut self, p0: Point, nl: Vec2, nr: Vec2) {
        if self.p.d_left > 0.0 {
            self.left.line_to(p0 - nl);
        }
        if self.p.d_right > 0.0 {
            self.right.line_to(p0 + nr);
        }
    }

    /// Close out an open subpath: left boundary, end cap, reversed right
    /// boundary, start cap.
    fn finish_open(&mut self) {
        if self.left.elements().is_empty() || self.suppress_finish {
            return;
        }
        let tol = self.p.tolerance;
        let half = 0.5 * self.p.width();
        self.out.extend(self.left.iter());

        let right_els = self.right.elements();
        let right_end = right_els[right_els.len() - 1].end_point().unwrap();
        // End cap across the band cross-section.
        let n_end = Self::unit_norm(self.last_tan);
        let mid_end = self.last_pt + 0.5 * (self.p.d_right - self.p.d_left) * n_end;
        let cap_n_end = -half * n_end; // current point (left end) minus midpoint
        match self.p.end_cap {
            Cap::Butt => self.out.line_to(right_end),
            Cap::Round => round_cap(self.out, tol, mid_end, cap_n_end),
            Cap::Square => square_cap(self.out, false, mid_end, cap_n_end),
        }
        extend_reversed(self.out, right_els);
        // Start cap; the current point is the right boundary's start.
        let n0 = self.start_norm;
        let mid_start = self.start_pt + 0.5 * (self.p.d_right - self.p.d_left) * n0;
        let cap_n_start = half * n0;
        match self.p.start_cap {
            Cap::Butt => self.out.close_path(),
            Cap::Round => {
                round_cap(self.out, tol, mid_start, cap_n_start);
                self.out.close_path();
            }
            Cap::Square => square_cap(self.out, true, mid_start, cap_n_start),
        }

        self.left.truncate(0);
        self.right.truncate(0);
    }

    /// Close out a closed subpath: two closed contours (left forward, right
    /// reversed) forming a ring under the nonzero rule.
    fn finish_closed(&mut self) {
        if self.left.elements().is_empty() {
            return;
        }
        self.do_join(self.start_tan);
        reanchor_clipped_seam(self.left);
        reanchor_clipped_seam(self.right);
        if self.suppress_finish {
            return;
        }
        self.out.extend(self.left.iter());
        self.out.close_path();
        let right_els = self.right.elements();
        let right_end = right_els[right_els.len() - 1].end_point().unwrap();
        self.out.move_to(right_end);
        extend_reversed(self.out, right_els);
        self.out.close_path();
        self.left.truncate(0);
        self.right.truncate(0);
    }
}

/// After an inner-clipped seam join, a ring accumulator ends at the clip
/// point instead of back at its `MoveTo` start; the implicit closing chord
/// would retrace a sliver along the first offset line (winding-neutral but
/// visible in wireframes). When the first drawn element is a line — the clip
/// point provably lies on it — start the ring at the clip point instead.
fn reanchor_clipped_seam(path: &mut BezPath) {
    let els = path.elements();
    if els.len() < 2 || !matches!(els[1], PathEl::LineTo(_)) {
        return;
    }
    let (PathEl::MoveTo(start), Some(last)) = (els[0], els.last().and_then(|el| el.end_point()))
    else {
        return;
    };
    if last != start {
        if let PathEl::MoveTo(p) = &mut path.elements_mut()[0] {
            *p = last;
        }
    }
}

/// Robust endpoint tangents of a path segment.
///
/// Port of kurbo's `pub(crate) PathSeg::tangents` (an upstream visibility PR
/// is planned); falls back across coincident control points.
pub(crate) fn tangents(seg: &PathSeg) -> (Vec2, Vec2) {
    const EPS: f64 = 1e-12;
    match seg {
        PathSeg::Line(l) => {
            let d = l.p1 - l.p0;
            (d, d)
        }
        PathSeg::Quad(q) => {
            let d01 = q.p1 - q.p0;
            let d0 = if d01.hypot2() > EPS { d01 } else { q.p2 - q.p0 };
            let d12 = q.p2 - q.p1;
            let d1 = if d12.hypot2() > EPS { d12 } else { q.p2 - q.p0 };
            (d0, d1)
        }
        PathSeg::Cubic(c) => {
            let d01 = c.p1 - c.p0;
            let d0 = if d01.hypot2() > EPS {
                d01
            } else {
                let d02 = c.p2 - c.p0;
                if d02.hypot2() > EPS { d02 } else { c.p3 - c.p0 }
            };
            let d23 = c.p3 - c.p2;
            let d1 = if d23.hypot2() > EPS {
                d23
            } else {
                let d13 = c.p3 - c.p1;
                if d13.hypot2() > EPS { d13 } else { c.p3 - c.p0 }
            };
            (d0, d1)
        }
    }
}

/// Connect `side`'s current endpoint to `offset`'s start point if they
/// differ (they coincide exactly after a regular join; an inner clip leaves
/// the boundary at the clip point instead, and curve offsets must start from
/// their true offset origin).
fn heal_start(side: &mut BezPath, offset: &BezPath) {
    if let (Some(PathEl::MoveTo(start)), Some(current)) = (
        offset.elements().first(),
        side.elements().last().and_then(|el| el.end_point()),
    ) {
        if current != *start {
            side.line_to(*start);
        }
    }
}

/// Append `elements` (an open `MoveTo`-led run) to `out`, reversed.
fn extend_reversed(out: &mut BezPath, elements: &[PathEl]) {
    for i in (1..elements.len()).rev() {
        let end = elements[i - 1].end_point().unwrap();
        match elements[i] {
            PathEl::LineTo(_) => out.line_to(end),
            PathEl::QuadTo(p1, _) => out.quad_to(p1, end),
            PathEl::CurveTo(p1, p2, _) => out.curve_to(p2, p1, end),
            _ => unreachable!("reversed run contains only drawing elements"),
        }
    }
}

/// Half-circle cap; `norm` points from the band midpoint to the arc's start
/// (the boundary the output currently sits on) and its length is the radius.
pub(crate) fn round_cap(out: &mut BezPath, tolerance: f64, center: Point, norm: Vec2) {
    round_join(out, tolerance, center, norm, PI);
}

/// Arc on the left boundary from the previous normal to `norm`, sweeping
/// `angle` around `center`. Ported from kurbo.
fn round_join(out: &mut BezPath, tolerance: f64, center: Point, norm: Vec2, angle: f64) {
    let a = Affine::new([norm.x, norm.y, -norm.y, norm.x, center.x, center.y]);
    let arc = Arc::new(Point::ORIGIN, (1.0, 1.0), PI - angle, angle, 0.0);
    arc.to_cubic_beziers(tolerance, |p1, p2, p3| out.curve_to(a * p1, a * p2, a * p3));
}

/// Mirror-image of [`round_join`] for the right boundary.
fn round_join_rev(out: &mut BezPath, tolerance: f64, center: Point, norm: Vec2, angle: f64) {
    let a = Affine::new([norm.x, norm.y, norm.y, -norm.x, center.x, center.y]);
    let arc = Arc::new(Point::ORIGIN, (1.0, 1.0), PI - angle, angle, 0.0);
    arc.to_cubic_beziers(tolerance, |p1, p2, p3| out.curve_to(a * p1, a * p2, a * p3));
}

/// Square cap; same `norm` convention as [`round_cap`].
pub(crate) fn square_cap(out: &mut BezPath, close: bool, center: Point, norm: Vec2) {
    let a = Affine::new([norm.x, norm.y, -norm.y, norm.x, center.x, center.y]);
    out.line_to(a * Point::new(1.0, 1.0));
    out.line_to(a * Point::new(-1.0, 1.0));
    if close {
        out.close_path();
    } else {
        out.line_to(a * Point::new(-1.0, 0.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band_run(els: &[PathEl], p: BandParams<'_>) -> BezPath {
        let mut left = BezPath::new();
        let mut right = BezPath::new();
        let mut scratch = BezPath::new();
        let mut out = BezPath::new();
        let mut band = Band::new(p, &mut left, &mut right, &mut scratch, &mut out);
        band.run(els.iter().copied());
        out
    }

    fn params(d_left: f64, d_right: f64) -> BandParams<'static> {
        let tolerance = 1e-3;
        BandParams {
            d_left,
            d_right,
            join: Join::Bevel,
            miter_limit: 4.0,
            start_cap: Cap::Butt,
            end_cap: Cap::Butt,
            tolerance,
            join_thresh: 2.0 * tolerance / (d_left + d_right),
            crossings: &[],
            crossing_eps: 0.0,
            raw_offset: false,
        }
    }

    /// A single horizontal line, right-side band, butt caps: an exact
    /// axis-aligned rectangle below the line (Y-down ⇒ right of east = +y).
    #[test]
    fn one_sided_line_is_a_rect() {
        let els = [
            PathEl::MoveTo((0.0, 0.0).into()),
            PathEl::LineTo((10.0, 0.0).into()),
        ];
        let out = band_run(&els, params(0.0, 4.0));
        let expected = [
            PathEl::MoveTo((0.0, 0.0).into()),
            PathEl::LineTo((10.0, 0.0).into()),
            PathEl::LineTo((10.0, 4.0).into()),
            PathEl::LineTo((0.0, 4.0).into()),
            PathEl::ClosePath,
        ];
        assert_eq!(out.elements(), &expected);
    }

    /// Centered band on the same line matches kurbo's geometry.
    #[test]
    fn centered_line_matches_kurbo_rect() {
        let els = [
            PathEl::MoveTo((0.0, 0.0).into()),
            PathEl::LineTo((10.0, 0.0).into()),
        ];
        let out = band_run(&els, params(2.0, 2.0));
        let expected = [
            PathEl::MoveTo((0.0, -2.0).into()),
            PathEl::LineTo((10.0, -2.0).into()),
            PathEl::LineTo((10.0, 2.0).into()),
            PathEl::LineTo((0.0, 2.0).into()),
            PathEl::ClosePath,
        ];
        assert_eq!(out.elements(), &expected);
    }

    /// Closed CW square, inside (right) band: the left contour is the exact
    /// source square; the ring is (outer area) − (inner area) under nonzero.
    #[test]
    fn closed_square_inside_band() {
        let els = [
            PathEl::MoveTo((0.0, 0.0).into()),
            PathEl::LineTo((10.0, 0.0).into()),
            PathEl::LineTo((10.0, 10.0).into()),
            PathEl::LineTo((0.0, 10.0).into()),
            PathEl::ClosePath,
        ];
        let mut p = params(0.0, 2.0);
        p.join = Join::Miter;
        let out = band_run(&els, p);
        // First contour = source square, exactly (closing segment explicit).
        let first: alloc::vec::Vec<PathEl> = crate::normalize::subpaths(out.elements())
            .next()
            .unwrap()
            .to_vec();
        let expected_first = [
            PathEl::MoveTo((0.0, 0.0).into()),
            PathEl::LineTo((10.0, 0.0).into()),
            PathEl::LineTo((10.0, 10.0).into()),
            PathEl::LineTo((0.0, 10.0).into()),
            PathEl::LineTo((0.0, 0.0).into()),
            PathEl::ClosePath,
        ];
        assert_eq!(&first, &expected_first);
        // Everything stays inside the source square.
        use kurbo::Shape;
        let bb = out.bounding_box();
        assert!(
            bb.x0 >= -1e-12 && bb.y0 >= -1e-12 && bb.x1 <= 10.0 + 1e-12 && bb.y1 <= 10.0 + 1e-12
        );
    }

    /// Right-angle polyline with a miter join, centered: kurbo's own
    /// `dash_miter_join` expected geometry (sanity for the join port).
    #[test]
    fn miter_join_matches_kurbo_expectation() {
        let els = [
            PathEl::MoveTo((70.0, 80.0).into()),
            PathEl::LineTo((0.0, 80.0).into()),
            PathEl::LineTo((0.0, 77.0).into()),
        ];
        let mut p = params(10.0, 10.0);
        p.join = Join::Miter;
        let out = band_run(&els, p);
        let expected = [
            PathEl::MoveTo((70.0, 90.0).into()),
            PathEl::LineTo((0.0, 90.0).into()),
            PathEl::LineTo((-10.0, 90.0).into()),
            PathEl::LineTo((-10.0, 80.0).into()),
            PathEl::LineTo((-10.0, 77.0).into()),
            PathEl::LineTo((10.0, 77.0).into()),
            PathEl::LineTo((10.0, 80.0).into()),
            PathEl::LineTo((0.0, 80.0).into()),
            PathEl::LineTo((0.0, 70.0).into()),
            PathEl::LineTo((70.0, 70.0).into()),
            PathEl::ClosePath,
        ];
        assert_eq!(out.elements(), &expected);
    }
}
