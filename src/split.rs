// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Splitting subpaths at their self-intersections.
//!
//! A one-sided band flips which geometric side faces the fill whenever the
//! path crosses itself: banding the raw subpath then produces windings that
//! *cancel* (unpainted pockets), and inside/outside stop matching the filled
//! region — Figma resolves both against the fill because its strokes are
//! masked by it. kurbo-se gets the same behavior by cutting each subpath at
//! its self-intersections into **lobes** (closed simple-ish loops) and
//! **strands** (leftover open runs), resolving the side per lobe with the
//! usual fill probe, and banding each piece independently — independent
//! contours add winding instead of cancelling.
//!
//! Intersections are found by bounding-box subdivision to a parametric
//! accuracy; single-segment loops (a cubic crossing itself) are handled by
//! pre-splitting at the curve's extrema, since an axis-monotone piece cannot
//! self-intersect. Interleaved (non-nested) crossing patterns — pretzels —
//! decompose only partially; the result is still artifact-reduced and never
//! panics. Tangential touches are deliberately ignored (they don't flip
//! sides).

use alloc::vec;
use alloc::vec::Vec;

use kurbo::{BezPath, ParamCurve, ParamCurveArclen, ParamCurveExtrema, PathEl, PathSeg, Point};

/// Parametric epsilon for treating a hit as an endpoint touch.
pub(crate) const T_EPS: f64 = 1e-6;

/// Parametric margin trimmed off shared endpoints of adjacent segments
/// before intersecting them. A smooth joint then rejects at the bounding-box
/// level instead of recursing to flatness; cuts this close to a joint are
/// redundant anyway (segment boundaries are already piece boundaries).
const ADJ_TRIM: f64 = 1e-3;
/// Cluster radius for deduplicating hits, in parameter space.
const DEDUP_EPS: f64 = 1e-4;
/// Arc-length accuracy for dash phase offsets.
const ARCLEN_ACC: f64 = 1e-6;

/// A renderable unit produced by splitting.
pub(crate) struct SplitPiece {
    pub els: BezPath,
    pub closed: bool,
}

/// A dash unit: a contiguous run between crossings, with its arc-length
/// start in the original subpath (for phase-continuous dashing).
pub(crate) struct SplitArc {
    pub els: BezPath,
    pub s0: f64,
    pub piece_ix: usize,
}

pub(crate) struct SplitResult {
    pub pieces: Vec<SplitPiece>,
    pub arcs: Vec<SplitArc>,
    /// The crossing vertices (both strands' cut points), for join
    /// suppression at crossing tips.
    pub crossing_points: Vec<Point>,
}

/// Split `subpath` at its self-intersections. Returns `None` when the
/// subpath is simple (the caller keeps the untouched fast path) or when
/// decomposition fails structurally.
pub(crate) fn split_self_intersections(
    subpath: &[PathEl],
    closed: bool,
    accuracy: f64,
) -> Option<SplitResult> {
    let segs: Vec<PathSeg> = kurbo::segments(subpath.iter().copied()).collect();
    let n = segs.len();
    if n == 0 {
        return None;
    }

    // ---- collect crossings ----
    struct Cut {
        seg: usize,
        t: f64,
        crossing: usize,
    }
    let mut cuts: Vec<Cut> = Vec::new();
    let mut n_crossings = 0usize;
    // Precomputed boxes: rejecting a pair here avoids both the intersection
    // search and its allocation, which dominates on many-segment paths.
    let bboxes: Vec<kurbo::Rect> = segs.iter().map(ParamCurveExtrema::bounding_box).collect();

    let mut push_crossing = |cuts: &mut Vec<Cut>, a: (usize, f64), b: (usize, f64)| {
        let id = n_crossings;
        cuts.push(Cut {
            seg: a.0,
            t: a.1,
            crossing: id,
        });
        cuts.push(Cut {
            seg: b.0,
            t: b.1,
            crossing: id,
        });
        n_crossings += 1;
    };

    for i in 0..n {
        // Single-segment self-intersection (cubic loops).
        for (t1, t2) in seg_self_intersections(&segs[i], accuracy) {
            push_crossing(&mut cuts, (i, t1), (i, t2));
        }
        for j in (i + 1)..n {
            let adjacent_next = j == i + 1;
            let adjacent_wrap = closed && i == 0 && j == n - 1;
            let (ba, bb) = (bboxes[i], bboxes[j]);
            if ba.x0 > bb.x1 + accuracy
                || bb.x0 > ba.x1 + accuracy
                || ba.y0 > bb.y1 + accuracy
                || bb.y0 > ba.y1 + accuracy
            {
                continue;
            }
            let hits =
                segment_pair_params(&segs[i], &segs[j], adjacent_next, adjacent_wrap, accuracy);
            for (ta, tb) in hits {
                // Mutual endpoint touches are handled by the duplicate-vertex
                // pass below (once, instead of once per incident pair).
                let a_end = !(T_EPS..=1.0 - T_EPS).contains(&ta);
                let b_end = !(T_EPS..=1.0 - T_EPS).contains(&tb);
                if a_end && b_end {
                    continue;
                }
                push_crossing(&mut cuts, (i, ta), (j, tb));
            }
        }
    }

    // Crossings *at vertices*: the path re-visits an anchor point (the
    // figure-eight crosses exactly at its center anchor). Detected by
    // duplicate end-of-segment positions; each duplicate pair is one
    // crossing with cuts at the segment ends. A revisit of the start point
    // of a closed subpath pairs with the seam (end of the last segment).
    const VTX_EPS2: f64 = 1e-12;
    let vertex_after: Vec<Point> = segs.iter().map(|s| s.end()).collect();
    for i in 0..n.saturating_sub(1) {
        for j in (i + 1)..n {
            if j == n - 1 && !closed {
                // End of an open path touching an interior vertex is a
                // T-touch, not a crossing.
                continue;
            }
            if (vertex_after[i] - vertex_after[j]).hypot2() <= VTX_EPS2 {
                push_crossing(&mut cuts, (i, 1.0), (j, 1.0));
            }
        }
    }

    if n_crossings == 0 {
        return None;
    }

    // ---- cut segments into arcs ----
    // Order cuts along the path.
    cuts.sort_by(|a, b| (a.seg, a.t).partial_cmp(&(b.seg, b.t)).unwrap());
    let crossing_points: Vec<Point> = cuts
        .iter()
        .map(|c| segs[c.seg].eval(c.t.clamp(0.0, 1.0)))
        .collect();

    struct Arc {
        els: BezPath,
        s0: f64,
        end_crossing: Option<usize>,
    }
    let mut arcs: Vec<Arc> = Vec::new();
    let mut cur = BezPath::new();
    let mut cur_s0 = 0.0;
    let mut s = 0.0;
    let mut cut_ix = 0;
    for (i, seg) in segs.iter().enumerate() {
        let seg_len = seg.arclen(ARCLEN_ACC);
        let mut t0 = 0.0;
        while cut_ix < cuts.len() && cuts[cut_ix].seg == i {
            let c = &cuts[cut_ix];
            let t = c.t.clamp(0.0, 1.0);
            if t > t0 + T_EPS {
                append_seg(&mut cur, seg.subsegment(t0..t));
            }
            // Close the current arc at this crossing.
            arcs.push(Arc {
                els: core::mem::take(&mut cur),
                s0: cur_s0,
                end_crossing: Some(c.crossing),
            });
            cur_s0 = s + seg_len * t; // arc-length approximation via parameter share
            cur = BezPath::new();
            cur.move_to(seg.eval(t));
            t0 = t;
            cut_ix += 1;
        }
        if t0 < 1.0 - T_EPS {
            if cur.elements().is_empty() {
                cur.move_to(seg.eval(t0));
            }
            append_seg(&mut cur, seg.subsegment(t0..1.0));
        }
        s += seg_len;
    }
    arcs.push(Arc {
        els: cur,
        s0: cur_s0,
        end_crossing: None,
    });

    // First arc may lack its MoveTo (starts at path start).
    if let Some(first) = arcs.first_mut() {
        if !matches!(first.els.elements().first(), Some(PathEl::MoveTo(_))) {
            let start = start_point(subpath);
            let mut with_move = BezPath::new();
            with_move.move_to(start);
            with_move.extend(first.els.iter());
            first.els = with_move;
        }
    }

    // For closed subpaths the seam is not a cut: merge the trailing arc into
    // the leading one (contiguous through the seam; phase continues past L).
    if closed && arcs.len() >= 2 {
        if arcs.last().unwrap().els.elements().len() < 2 {
            // A cut landed exactly on the seam; the stub carries nothing.
            arcs.pop();
        } else {
            let last = arcs.pop().unwrap();
            let first_s0 = last.s0;
            let first = &mut arcs[0];
            let mut merged = last.els;
            merged.extend(first.els.iter().skip(1)); // drop first's MoveTo
            first.els = merged;
            first.s0 = first_s0;
            // its end_crossing stays (the crossing that closed arc 0)
        }
    }

    // ---- assemble lobes (stack algorithm) ----
    let mut piece_of_arc: Vec<usize> = vec![usize::MAX; arcs.len()];
    let mut pieces: Vec<Vec<usize>> = Vec::new();
    {
        let mut chain: Vec<usize> = Vec::new();
        // crossing id -> position in chain where it was opened
        let mut open: Vec<Option<usize>> = vec![None; n_crossings];
        for (aix, arc) in arcs.iter().enumerate() {
            chain.push(aix);
            let Some(c) = arc.end_crossing else { continue };
            if let Some(mark) = open[c].take() {
                // Arcs since the mark form a closed lobe.
                let lobe: Vec<usize> = chain.drain(mark..).collect();
                // Crossings opened inside the popped run are gone with it
                // (interleaved patterns decompose only partially).
                for o in open.iter_mut() {
                    if o.is_some_and(|m| m >= chain.len()) {
                        *o = None;
                    }
                }
                pieces.push(lobe);
            } else {
                open[c] = Some(chain.len());
            }
        }
        if !chain.is_empty() {
            pieces.push(chain);
        }
    }

    // ---- materialize pieces ----
    let mut out = SplitResult {
        pieces: Vec::with_capacity(pieces.len()),
        arcs: Vec::with_capacity(arcs.len()),
        crossing_points,
    };
    for (pix, arc_ixs) in pieces.iter().enumerate() {
        let mut els = BezPath::new();
        for (k, &aix) in arc_ixs.iter().enumerate() {
            if k == 0 {
                els.extend(arcs[aix].els.iter());
            } else {
                els.extend(arcs[aix].els.iter().skip(1));
            }
            piece_of_arc[aix] = pix;
        }
        if els.elements().len() < 2 {
            // Degenerate sliver (e.g. cuts closer than T_EPS): give up on
            // splitting rather than emit broken pieces.
            return None;
        }
        // A piece is closed when its endpoints coincide: lobes always do;
        // the leftover chain does iff the subpath was closed.
        let is_leftover = pix == pieces.len() - 1 && arcs.last().unwrap().end_crossing.is_none();
        let piece_closed = if is_leftover { closed } else { true };
        if piece_closed {
            els.close_path();
        }
        out.pieces.push(SplitPiece {
            els,
            closed: piece_closed,
        });
    }
    for (aix, arc) in arcs.into_iter().enumerate() {
        if arc.els.elements().len() < 2 {
            continue; // empty arc (cut at a vertex)
        }
        out.arcs.push(SplitArc {
            els: arc.els,
            s0: arc.s0,
            piece_ix: piece_of_arc[aix],
        });
    }
    Some(out)
}

/// Self-intersection parameters of a single segment (cubic loops), flattened
/// to a plain cut list. Used by the offset-region pruner.
pub(crate) fn segment_self_intersection_params(seg: &PathSeg, accuracy: f64) -> Vec<f64> {
    let mut out = Vec::new();
    for (t1, t2) in seg_self_intersections(seg, accuracy) {
        out.push(t1);
        out.push(t2);
    }
    out
}

/// Intersection parameters of two segments.
///
/// `next` marks `end(a) = start(b)` adjacency, `wrap` marks
/// `start(a) = end(b)` (the seam pair of a closed contour); both can hold at
/// once for a two-segment contour. The shared endpoints are trimmed off the
/// search domain ([`ADJ_TRIM`]), so a smooth joint costs one bounding-box
/// rejection instead of a recursion to flatness, and the joint itself is
/// never reported as a crossing.
pub(crate) fn segment_pair_params(
    a: &PathSeg,
    b: &PathSeg,
    next: bool,
    wrap: bool,
    accuracy: f64,
) -> Vec<(f64, f64)> {
    let ar = (
        if wrap { ADJ_TRIM } else { 0.0 },
        if next { 1.0 - ADJ_TRIM } else { 1.0 },
    );
    let br = (
        if next { ADJ_TRIM } else { 0.0 },
        if wrap { 1.0 - ADJ_TRIM } else { 1.0 },
    );
    let mut hits = Vec::new();
    if next || wrap {
        let a_sub = a.subsegment(ar.0..ar.1);
        let b_sub = b.subsegment(br.0..br.1);
        recurse(&a_sub, ar, &b_sub, br, accuracy.max(1e-9), 0, &mut hits);
    } else {
        recurse(a, ar, b, br, accuracy.max(1e-9), 0, &mut hits);
    }
    dedupe(&mut hits);
    hits
}

/// Subdivide a segment at the given parameters (sorted and deduplicated in
/// place); interior cuts only.
pub(crate) fn subdivide_at(seg: &PathSeg, ts: &mut Vec<f64>) -> Vec<PathSeg> {
    ts.retain(|t| *t > T_EPS && *t < 1.0 - T_EPS);
    ts.sort_by(f64::total_cmp);
    ts.dedup_by(|a, b| (*a - *b).abs() < DEDUP_EPS);
    let mut out = Vec::with_capacity(ts.len() + 1);
    let mut t0 = 0.0;
    for &t in ts.iter() {
        out.push(seg.subsegment(t0..t));
        t0 = t;
    }
    out.push(seg.subsegment(t0..1.0));
    out
}

fn start_point(subpath: &[PathEl]) -> Point {
    match subpath.first() {
        Some(PathEl::MoveTo(p)) => *p,
        _ => Point::ORIGIN,
    }
}

fn append_seg(path: &mut BezPath, seg: PathSeg) {
    if path.elements().is_empty() {
        path.move_to(seg.start());
    }
    match seg {
        PathSeg::Line(l) => path.line_to(l.p1),
        PathSeg::Quad(q) => path.quad_to(q.p1, q.p2),
        PathSeg::Cubic(c) => path.curve_to(c.p1, c.p2, c.p3),
    }
}

/// Distance from a segment's control points to its chord (flatness).
fn flatness(seg: &PathSeg) -> f64 {
    match seg {
        PathSeg::Line(_) => 0.0,
        PathSeg::Quad(q) => point_line_dist(q.p1, q.p0, q.p2),
        PathSeg::Cubic(c) => {
            point_line_dist(c.p1, c.p0, c.p3).max(point_line_dist(c.p2, c.p0, c.p3))
        }
    }
}

fn point_line_dist(p: Point, a: Point, b: Point) -> f64 {
    let d = b - a;
    let len2 = d.hypot2();
    if len2 == 0.0 {
        return (p - a).hypot();
    }
    ((p - a).cross(d)).abs() / crate::math::sqrt(len2)
}

/// Self-intersections of one segment (cubic loops): split at the curve's
/// extrema — monotone pieces cannot self-intersect — and intersect the
/// pieces pairwise. Adjacent pieces get their shared endpoint trimmed off
/// the domain (same rationale as [`segment_pair_params`]).
fn seg_self_intersections(seg: &PathSeg, accuracy: f64) -> Vec<(f64, f64)> {
    if !matches!(seg, PathSeg::Cubic(_)) {
        return Vec::new();
    }
    let mut ts: Vec<f64> = seg.extrema().to_vec();
    ts.retain(|t| *t > T_EPS && *t < 1.0 - T_EPS);
    ts.sort_by(f64::total_cmp);
    if ts.is_empty() {
        return Vec::new();
    }
    let mut bounds = Vec::with_capacity(ts.len() + 2);
    bounds.push(0.0);
    bounds.extend(ts);
    bounds.push(1.0);
    let mut hits = Vec::new();
    for i in 0..bounds.len() - 1 {
        for j in (i + 1)..bounds.len() - 1 {
            let (a0, mut a1) = (bounds[i], bounds[i + 1]);
            let (mut b0, b1) = (bounds[j], bounds[j + 1]);
            if j == i + 1 {
                // Trim the shared endpoint out of the search domain.
                a1 -= ADJ_TRIM * (a1 - a0);
                b0 += ADJ_TRIM * (b1 - b0);
            }
            let pa = seg.subsegment(a0..a1);
            let pb = seg.subsegment(b0..b1);
            recurse(
                &pa,
                (a0, a1),
                &pb,
                (b0, b1),
                accuracy.max(1e-9),
                0,
                &mut hits,
            );
        }
    }
    dedupe(&mut hits);
    hits
}

fn dedupe(hits: &mut Vec<(f64, f64)>) {
    hits.sort_by(|x, y| x.partial_cmp(y).unwrap());
    hits.dedup_by(|a, b| (a.0 - b.0).abs() < DEDUP_EPS && (a.1 - b.1).abs() < DEDUP_EPS);
}

const MAX_DEPTH: u32 = 28;

fn recurse(
    a: &PathSeg,
    ar: (f64, f64),
    b: &PathSeg,
    br: (f64, f64),
    accuracy: f64,
    depth: u32,
    out: &mut Vec<(f64, f64)>,
) {
    let ba = a.bounding_box();
    let bb = b.bounding_box();
    if ba.x0 > bb.x1 + accuracy
        || bb.x0 > ba.x1 + accuracy
        || ba.y0 > bb.y1 + accuracy
        || bb.y0 > ba.y1 + accuracy
    {
        return;
    }
    let fa = flatness(a);
    let fb = flatness(b);
    if (fa <= accuracy && fb <= accuracy) || depth >= MAX_DEPTH {
        // Chord/chord intersection.
        let (a0, a1) = (a.start(), a.end());
        let (b0, b1) = (b.start(), b.end());
        let da = a1 - a0;
        let db = b1 - b0;
        let denom = da.cross(db);
        if denom == 0.0 {
            return;
        }
        let s = (b0 - a0).cross(db) / denom;
        let t = (b0 - a0).cross(da) / denom;
        if !(-1e-9..=1.0 + 1e-9).contains(&s) || !(-1e-9..=1.0 + 1e-9).contains(&t) {
            return;
        }
        let ta = ar.0 + s.clamp(0.0, 1.0) * (ar.1 - ar.0);
        let tb = br.0 + t.clamp(0.0, 1.0) * (br.1 - br.0);
        out.push((ta, tb));
        return;
    }
    // Subdivide the less flat one (or both when comparable).
    if fa > accuracy && (fa >= fb || fb <= accuracy) {
        let am = 0.5 * (ar.0 + ar.1);
        let a_lo = a.subsegment(0.0..0.5);
        let a_hi = a.subsegment(0.5..1.0);
        recurse(&a_lo, (ar.0, am), b, br, accuracy, depth + 1, out);
        recurse(&a_hi, (am, ar.1), b, br, accuracy, depth + 1, out);
    } else {
        let bm = 0.5 * (br.0 + br.1);
        let b_lo = b.subsegment(0.0..0.5);
        let b_hi = b.subsegment(0.5..1.0);
        recurse(a, ar, &b_lo, (br.0, bm), accuracy, depth + 1, out);
        recurse(a, ar, &b_hi, (bm, br.1), accuracy, depth + 1, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::Shape;

    fn bowtie() -> BezPath {
        let mut p = BezPath::new();
        p.move_to((20.0, 20.0));
        p.line_to((220.0, 180.0));
        p.line_to((220.0, 20.0));
        p.line_to((20.0, 180.0));
        p.close_path();
        p
    }

    #[test]
    fn simple_shapes_do_not_split() {
        let rect = kurbo::Rect::new(0.0, 0.0, 10.0, 10.0).to_path(1e-9);
        assert!(split_self_intersections(rect.elements(), true, 1e-3).is_none());
        let circle = kurbo::Circle::new((0.0, 0.0), 10.0).to_path(1e-6);
        assert!(split_self_intersections(circle.elements(), true, 1e-3).is_none());
    }

    #[test]
    fn bowtie_splits_into_two_lobes() {
        let p = bowtie();
        let res = split_self_intersections(p.elements(), true, 1e-3).expect("must split");
        assert_eq!(res.pieces.len(), 2, "bowtie has two lobes");
        for piece in &res.pieces {
            assert!(piece.closed);
            let a = piece.els.elements().area().abs();
            assert!(a > 1.0, "lobe area {a} too small");
        }
        // Lobe areas: the crossing splits the bowtie into two triangles.
        let mut areas: Vec<f64> = res
            .pieces
            .iter()
            .map(|p| p.els.elements().area().abs())
            .collect();
        areas.sort_by(f64::total_cmp);
        let total: f64 = areas.iter().sum();
        assert!(
            (total - 16000.0).abs() < 50.0,
            "lobes should cover both triangles, got {areas:?}"
        );
    }

    #[test]
    fn cubic_loop_splits() {
        // Open curve with a loop (the sandbox gallery shape).
        let mut p = BezPath::new();
        p.move_to((20.0, 150.0));
        p.curve_to((200.0, 20.0), (0.0, 20.0), (180.0, 150.0));
        let res = split_self_intersections(p.elements(), false, 1e-3).expect("must split");
        // One closed loop lobe + one open leftover strand.
        assert_eq!(res.pieces.len(), 2);
        let closed_count = res.pieces.iter().filter(|p| p.closed).count();
        assert_eq!(closed_count, 1, "exactly one closed loop");
        // Arc phases are ascending and start at 0.
        assert!(res.arcs[0].s0.abs() < 1e-6);
        for w in res.arcs.windows(2) {
            assert!(w[0].s0 <= w[1].s0 + 1e-6);
        }
    }

    #[test]
    fn figure_eight_two_lobes() {
        let mut p = BezPath::new();
        p.move_to((120.0, 100.0));
        p.curve_to((150.0, 55.0), (210.0, 55.0), (240.0, 100.0));
        p.curve_to((210.0, 145.0), (150.0, 145.0), (120.0, 100.0));
        p.curve_to((90.0, 55.0), (30.0, 55.0), (0.0, 100.0));
        p.curve_to((30.0, 145.0), (90.0, 145.0), (120.0, 100.0));
        p.close_path();
        let res = split_self_intersections(p.elements(), true, 1e-3).expect("must split");
        assert_eq!(res.pieces.len(), 2, "figure-eight = two lobes");
        // Lobes have opposite orientations (that's the point: each gets its
        // own side resolution).
        let a0 = res.pieces[0].els.elements().area();
        let a1 = res.pieces[1].els.elements().area();
        assert!(
            a0 * a1 < 0.0,
            "areas {a0} / {a1} should have opposite signs"
        );
    }
}
