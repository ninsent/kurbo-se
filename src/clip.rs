// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Masking one-sided dash bands by the filled region.
//!
//! Solid one-sided strokes on closed contours are built set-theoretically
//! by [`crate::region`]. Dashed ones cannot use that construction, because
//! the band is cut into dashes first. Each dash is expanded directly and
//! then masked by the fill here, which is what Figma does and what the
//! crate documents:
//!
//! ```text
//! Inside(w)  = { p ∈ F : dist(p, dashes) ≤ w }
//! Outside(w) = { p ∉ F : dist(p, dashes) ≤ w }
//! ```
//!
//! Without the mask, a band that does not fit the local geometry escapes. A
//! band of width `w` starting at a star's tip sticks far outside the shape,
//! since the wedge there is thinner than `w`, and a band near a
//! self-intersection crosses into the neighbouring lobe's fill.
//!
//! The mask is a Boolean intersection for inside alignment, or a difference
//! for outside. One edge of a one-sided band is the source path exactly —
//! the crate's shared-boundary guarantee, and the classic degenerate case
//! for a Boolean. Those pieces are detected geometrically and treated as
//! boundary: never cut, always kept, and excluded from the spliced fill
//! boundary, so the coincidence never reaches the intersection search.

use alloc::vec::Vec;

use kurbo::{
    BezPath, CubicBez, Line, ParamCurve, ParamCurveNearest, PathEl, PathSeg, Point, QuadBez, Rect,
    Shape,
};

use crate::region::SourceIndex;
use crate::split::{self, append_seg, overlaps};

/// The filled region a dashed band is masked by.
pub(crate) struct ClipSource {
    /// Exact source segments, oriented so the fill winds `+1`. Pieces of
    /// them are spliced into clipped loops, which keeps the shared boundary
    /// exactly on the source path.
    segs: Vec<PathSeg>,
    bboxes: Vec<Rect>,
    index: SourceIndex,
    /// Radius within which a point counts as lying *on* the fill boundary.
    eps: f64,
}

impl ClipSource {
    /// `normalized` must be the closed contours with the fill wound `+1`
    /// (as produced by `ctx::normalize_orientation`).
    pub(crate) fn new(normalized: &BezPath, flat_tol: f64) -> Self {
        let flat_tol = flat_tol.max(1e-9);
        let segs: Vec<PathSeg> = normalized.segments().collect();
        // Control-point boxes are looser than the true extrema box but
        // strictly conservative, and this is only a candidate filter.
        let bboxes = segs.iter().map(control_box).collect();
        ClipSource {
            index: SourceIndex::new(normalized, flat_tol),
            segs,
            bboxes,
            // The index is flattened, so a point exactly on the source curve
            // can read up to one flattening tolerance away from it.
            eps: flat_tol * 8.0,
        }
    }

    fn filled(&self, p: Point) -> bool {
        self.index.winding(p) != 0
    }

    fn near_boundary(&self, p: Point) -> bool {
        !self.index.is_clear_of(p, self.eps * self.eps, None)
    }
}

/// Mask the expansion of one dash (or dot) by the fill, appending to `out`.
pub(crate) fn clip_band(
    band: &BezPath,
    src: &ClipSource,
    keep_inside: bool,
    accuracy: f64,
    out: &mut BezPath,
) {
    let loops: Vec<&[PathEl]> = crate::normalize::subpaths(band.elements()).collect();
    // Nested loops are a winding pair that only makes sense together: a
    // gap-free dash expands to a ring. Masking them independently would
    // break the pair, so such output passes through untouched.
    if loops.len() > 1 {
        let boxes: Vec<Rect> = loops.iter().map(|l| l.bounding_box()).collect();
        let nested = (0..boxes.len())
            .any(|i| (0..boxes.len()).any(|j| i != j && boxes[i].intersect(boxes[j]).area() > 0.0));
        if nested {
            out.extend(band.iter());
            return;
        }
    }
    for els in loops {
        clip_loop(els, src, keep_inside, accuracy, out);
    }
}

fn clip_loop(
    els: &[PathEl],
    src: &ClipSource,
    keep_inside: bool,
    accuracy: f64,
    out: &mut BezPath,
) {
    let mut l = BezPath::from_vec(els.to_vec());
    if !matches!(l.elements().last(), Some(PathEl::ClosePath)) {
        l.close_path();
    }
    // The Boolean rules below assume the band and the fill wind the same
    // way. The fill is normalised to `+1`, so orient the band clockwise
    // too: positive area in Y-down.
    if l.elements().area() < 0.0 {
        l = l.reverse_subpaths();
    }
    let lsegs: Vec<PathSeg> = l.segments().collect();
    if lsegs.is_empty() {
        return;
    }
    let lbb = l.bounding_box();

    // Band pieces lying on the fill boundary: the shared edge of a
    // one-sided band. They are never intersected, since the coincidence
    // would flood the search with tangential hits. They are judged instead
    // by the side the band occupies next to them, which is not always the
    // masked side — a cap running along the source path past a
    // self-intersection sits on the boundary while the band beside it has
    // crossed into a wedge the fill does not cover.
    //
    // Test the midpoint first: it rejects the offset edge and the caps in
    // one query, so only a genuine boundary run pays for the endpoint
    // tests.
    let on_bnd: Vec<bool> = lsegs
        .iter()
        .map(|s| {
            src.near_boundary(s.eval(0.5))
                && src.near_boundary(s.eval(0.0))
                && src.near_boundary(s.eval(1.0))
        })
        .collect();
    // The band is wound clockwise, so its interior lies to the right of
    // travel (Y-down). Probe just inside it, far enough out to clear the
    // index's flattening error.
    let interior_masked = |s: &PathSeg| {
        const H: f64 = 0.05;
        let tan = s.eval(0.5 + H) - s.eval(0.5 - H);
        let len = tan.hypot();
        if len <= 0.0 {
            return true;
        }
        let probe = s.eval(0.5) + tan.turn_90() * (src.eps / len);
        src.filled(probe) == keep_inside
    };
    let on_bnd_ok: Vec<bool> = lsegs
        .iter()
        .zip(&on_bnd)
        .map(|(s, b)| !*b || interior_masked(s))
        .collect();

    // Sub-tolerance excursions are not worth a Boolean. Region membership
    // is already documented as approximate within a tolerance of the
    // boundary, and on any convex curve every cap pokes out by a fraction
    // of the local sagitta; trimming that costs far more than it shows.
    // Clip only when the band leaves the mask by more than the accuracy
    // asked for. That keeps the animated-dash path fast at display
    // tolerances and still tightens up as the caller asks for precision.
    let significant = |s: &PathSeg| {
        const N: usize = 4;
        (0..=N).any(|i| {
            let p = s.eval(i as f64 / N as f64);
            src.filled(p) != keep_inside && src.index.is_clear_of(p, accuracy * accuracy, None)
        })
    };
    let needs_clip = !on_bnd_ok.iter().all(|ok| *ok)
        || lsegs
            .iter()
            .zip(&on_bnd)
            .any(|(s, b)| !*b && significant(s));
    if !needs_clip {
        out.extend(l.iter());
        return;
    }

    let cand: Vec<usize> = (0..src.segs.len())
        .filter(|&i| overlaps(src.bboxes[i], lbb, accuracy))
        .collect();

    let mut lcuts: Vec<Vec<f64>> = alloc::vec![Vec::new(); lsegs.len()];
    let mut scuts: Vec<Vec<f64>> = alloc::vec![Vec::new(); cand.len()];
    let mut crossed = false;
    for (li, ls) in lsegs.iter().enumerate() {
        if on_bnd[li] {
            continue;
        }
        let lb = control_box(ls);
        for (ci, &si) in cand.iter().enumerate() {
            if !overlaps(src.bboxes[si], lb, accuracy) {
                continue;
            }
            for (ta, tb) in split::segment_pair_params(ls, &src.segs[si], false, false, accuracy) {
                lcuts[li].push(ta);
                scuts[ci].push(tb);
                crossed = true;
            }
        }
    }

    if !crossed && on_bnd_ok.iter().all(|ok| *ok) {
        // The band meets the fill boundary only along its shared edge and
        // sits on the masked side of it, so it lies wholly inside the mask:
        // keep or drop it whole. A band swallowed by another contour's fill
        // has no crossings either, which is why this tests the side rather
        // than keeping unconditionally.
        let side_ok = lsegs
            .iter()
            .zip(&on_bnd)
            .find(|(_, b)| !**b)
            .is_none_or(|(s, _)| src.filled(s.eval(0.5)) == keep_inside);
        if side_ok {
            out.extend(l.iter());
        }
        return;
    }

    // Cut the fill boundary at the ends of the band's shared edge too, so
    // every source piece is either wholly coincident with that edge or
    // wholly free of it. A piece straddling the transition would be dropped
    // as coincident and take the free stretch with it, stranding the shared
    // edge as its own loop: a sliver where a clipped dash should be.
    for (ls, _) in lsegs.iter().zip(&on_bnd).filter(|(_, b)| **b) {
        for t in [0.0, 1.0] {
            let p = ls.eval(t);
            for (ci, &si) in cand.iter().enumerate() {
                let near = src.segs[si].nearest(p, 1e-9);
                if near.distance_sq <= src.eps * src.eps {
                    scuts[ci].push(near.t);
                }
            }
        }
    }

    // --- classify ---
    let mut pieces: Vec<PathSeg> = Vec::new();
    for (li, ls) in lsegs.iter().enumerate() {
        if on_bnd[li] {
            if on_bnd_ok[li] {
                pieces.push(*ls);
            }
            continue;
        }
        for piece in split::subdivide_at(ls, &mut lcuts[li]) {
            if src.filled(piece.eval(0.5)) == keep_inside {
                pieces.push(piece);
            }
        }
    }
    // The fill boundary running through the band closes the clipped loop:
    // forwards for the intersection, backwards for the difference.
    for (ci, &si) in cand.iter().enumerate() {
        for piece in split::subdivide_at(&src.segs[si], &mut scuts[ci]) {
            let m = piece.eval(0.5);
            if l.winding(m) == 0 {
                continue;
            }
            // Skip the stretch the band already carries as its shared edge.
            if lsegs
                .iter()
                .zip(&on_bnd)
                .any(|(s, b)| *b && s.nearest(m, 1e-9).distance_sq <= src.eps * src.eps)
            {
                continue;
            }
            pieces.push(if keep_inside { piece } else { reverse(piece) });
        }
    }

    stitch(&pieces, (accuracy * 16.0).max(1e-9), out);
}

/// Conservative box of a segment's control points.
fn control_box(seg: &PathSeg) -> Rect {
    let pts: [Point; 4] = match seg {
        PathSeg::Line(l) => [l.p0, l.p1, l.p0, l.p1],
        PathSeg::Quad(q) => [q.p0, q.p1, q.p2, q.p2],
        PathSeg::Cubic(c) => [c.p0, c.p1, c.p2, c.p3],
    };
    let (mut x0, mut y0, mut x1, mut y1) = (pts[0].x, pts[0].y, pts[0].x, pts[0].y);
    for p in &pts[1..] {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    Rect::new(x0, y0, x1, y1)
}

fn reverse(seg: PathSeg) -> PathSeg {
    match seg {
        PathSeg::Line(l) => PathSeg::Line(Line::new(l.p1, l.p0)),
        PathSeg::Quad(q) => PathSeg::Quad(QuadBez::new(q.p2, q.p1, q.p0)),
        PathSeg::Cubic(c) => PathSeg::Cubic(CubicBez::new(c.p3, c.p2, c.p1, c.p0)),
    }
}

/// Chain surviving pieces into closed loops by endpoint matching.
fn stitch(pieces: &[PathSeg], eps: f64, out: &mut BezPath) {
    let n = pieces.len();
    let mut used = alloc::vec![false; n];
    let mut loop_path = BezPath::new();
    while let Some(start) = (0..n).find(|&i| !used[i]) {
        loop_path.truncate(0);
        let first = pieces[start].start();
        loop_path.move_to(first);
        let mut cur = start;
        loop {
            used[cur] = true;
            append_seg(&mut loop_path, pieces[cur]);
            let end = pieces[cur].end();
            if (end - first).hypot() <= eps {
                break;
            }
            let Some(next) = (0..n).find(|&k| !used[k] && (pieces[k].start() - end).hypot() <= eps)
            else {
                break;
            };
            cur = next;
        }
        loop_path.close_path();
        // Sub-resolution slivers paint nothing but cost segments.
        if loop_path.elements().len() >= 4 && loop_path.elements().area().abs() > eps * eps {
            out.extend(loop_path.iter());
        }
    }
}
