// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Splitting subpaths at their self-intersections.
//!
//! A one-sided band flips which geometric side faces the fill wherever the
//! path crosses itself. Banding the raw subpath then produces windings that
//! cancel, leaving unpainted pockets, and inside/outside stop matching the
//! filled region. Figma resolves both against the fill, because its strokes
//! are masked by it. kurbo-se gets the same behavior by cutting each
//! subpath at its self-intersections into lobes (closed, roughly simple
//! loops) and strands (the leftover open runs), resolving the side per lobe
//! with the usual fill probe, and banding each piece independently.
//! Independent contours add winding instead of cancelling.
//!
//! Intersections are found by bounding-box subdivision to a parametric
//! accuracy. Single-segment loops, where a cubic crosses itself, are
//! handled by pre-splitting at the curve's extrema, since an axis-monotone
//! piece cannot self-intersect. Interleaved crossing patterns — pretzels,
//! whose crossings do not nest — decompose only partially; the result still
//! has fewer artifacts than the unsplit band and never panics. Tangential
//! touches are ignored on purpose, since they do not flip sides.

use alloc::vec;
use alloc::vec::Vec;

use kurbo::{BezPath, ParamCurve, ParamCurveExtrema, PathEl, PathSeg, Point, Rect, Vec2};

/// Parametric epsilon for treating a hit as an endpoint touch.
pub(crate) const T_EPS: f64 = 1e-6;

/// Parametric margin trimmed off shared endpoints of adjacent segments
/// before intersecting them. A smooth joint then rejects at the
/// bounding-box level instead of recursing to flatness. Cuts this close to
/// a joint are redundant anyway: segment boundaries are already piece
/// boundaries.
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

/// A dash unit: a contiguous run between crossings.
pub(crate) struct SplitArc {
    pub els: BezPath,
    /// The arc length at which the run starts in the original subpath, so
    /// that dashing keeps its phase. Only dashing reads it. It is measured
    /// only when [`split_self_intersections`] is asked to, and is `0`
    /// otherwise.
    pub s0: f64,
    pub piece_ix: usize,
    /// Set on the arc of a closed subpath that runs through its start
    /// point: the index into `els` of the first element after that point.
    /// kurbo restarts the dash pattern at the start point, so the elements
    /// from here on are dashed from arc length `0` rather than from `s0`.
    pub seam: Option<usize>,
    /// Whether another arc picks up the pattern where this one ends: at a
    /// crossing, or at the start point of a closed subpath. A zero-length
    /// dash that lands exactly there belongs to that other arc, as it would
    /// on an unsplit path, so this arc does not draw its dot.
    pub shares_end: bool,
}

pub(crate) struct SplitResult {
    pub pieces: Vec<SplitPiece>,
    pub arcs: Vec<SplitArc>,
    /// The crossing vertices, meaning both strands' cut points. Used to
    /// suppress joins at crossing tips.
    pub crossing_points: Vec<Point>,
}

/// The crossings of `segs`, each as the two `(segment, t)` cuts it makes,
/// in the order of a nested loop over segment `i`: its own loops first,
/// then its crossings with each later segment `j`, by `j`.
///
/// This order is part of the result. Crossings are numbered in it, and the
/// cuts are then sorted along the path by a stable sort, so cuts on the
/// same point keep it. The broad phase visits pairs in no set order. So
/// each crossing is stored with its place in the loop, `(i, j, k)` for the
/// `k`th hit of the pair, and sorted by it at the end. No two crossings
/// share a place, so the unstable sort has only one result. Only actual
/// crossings are stored, never the candidate pairs.
fn collect_crossings(
    segs: &[PathSeg],
    bboxes: &[Rect],
    closed: bool,
    accuracy: f64,
) -> impl Iterator<Item = ((usize, f64), (usize, f64))> {
    let n = segs.len();
    let mut found: Vec<((usize, usize, usize), f64, f64)> = Vec::new();
    for (i, seg) in segs.iter().enumerate() {
        // Single-segment self-intersection (cubic loops).
        for (k, (t1, t2)) in seg_self_intersections(seg, accuracy)
            .into_iter()
            .enumerate()
        {
            found.push(((i, i, k), t1, t2));
        }
    }
    crate::broad::for_each_overlapping_pair(
        n,
        |i| bboxes[i],
        accuracy,
        |i, j| {
            let adjacent_next = j == i + 1;
            let adjacent_wrap = closed && i == 0 && j == n - 1;
            let hits = segment_pair_params_boxed(
                (&segs[i], bboxes[i]),
                (&segs[j], bboxes[j]),
                adjacent_next,
                adjacent_wrap,
                accuracy,
            );
            for (k, (ta, tb)) in hits.into_iter().enumerate() {
                // A touch at an end of both segments is left to the revisit
                // pass, `collect_revisits`. It finds each touch once, not
                // once for every pair of segments that meet there.
                let a_end = !(T_EPS..=1.0 - T_EPS).contains(&ta);
                let b_end = !(T_EPS..=1.0 - T_EPS).contains(&tb);
                if a_end && b_end {
                    continue;
                }
                found.push(((i, j, k), ta, tb));
            }
        },
    );
    found.sort_unstable_by_key(|&(at, _, _)| at);
    found
        .into_iter()
        .map(|((i, j, _), ta, tb)| ((i, ta), (j, tb)))
}

/// The pairs of segments `(i, j)`, `i < j`, whose end points lie within
/// `sqrt(vtx_eps2)` of each other: the places where the path revisits an
/// anchor. They come in the order of a nested loop, like
/// [`collect_crossings`]. The end of an open path touching an interior
/// vertex is a T-touch, not a crossing, and is left out.
///
/// The broad phase finds the candidates, on boxes that are single points.
/// Its slack is twice the radius, so every pair within the radius is a
/// candidate despite rounding in `sqrt`. The exact distance test decides.
fn collect_revisits(segs: &[PathSeg], closed: bool, vtx_eps2: f64) -> Vec<(usize, usize)> {
    let n = segs.len();
    let mut revisits: Vec<(usize, usize)> = Vec::new();
    crate::broad::for_each_overlapping_pair(
        n,
        |i| Rect::from_points(segs[i].end(), segs[i].end()),
        2.0 * crate::math::sqrt(vtx_eps2),
        |i, j| {
            let t_touch = j == n - 1 && !closed;
            if !t_touch && (segs[i].end() - segs[j].end()).hypot2() <= vtx_eps2 {
                revisits.push((i, j));
            }
        },
    );
    revisits.sort_unstable();
    revisits
}

/// Split `subpath` at its self-intersections. Returns `None` when the
/// subpath is simple (the caller keeps the untouched fast path) or when
/// decomposition fails structurally.
///
/// `measure` asks for the arc length at which each arc starts,
/// [`SplitArc::s0`]. Only dashing reads it. Measuring every segment to
/// dash accuracy costs more than the rest of the split, so solid strokes
/// pass `false` and get `0` there.
pub(crate) fn split_self_intersections(
    subpath: &[PathEl],
    closed: bool,
    accuracy: f64,
    measure: bool,
) -> Option<SplitResult> {
    // Degenerate segments carry no geometry, but their endpoints coincide
    // with their neighbours'. The duplicate-vertex pass below would read a
    // run of them as the path revisiting a point and split there.
    let segs: Vec<PathSeg> = kurbo::segments(subpath.iter().copied())
        .filter(|s| !crate::math::is_degenerate(s))
        .collect();
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
    // Precomputed boxes. Rejecting a pair here avoids both the
    // intersection search and its allocation, which dominate on
    // many-segment paths.
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

    for (a, b) in collect_crossings(&segs, &bboxes, closed, accuracy) {
        push_crossing(&mut cuts, a, b);
    }

    // Crossings at vertices, where the path revisits an anchor point. A
    // figure-eight crosses exactly at its center anchor. These are found as
    // duplicate end-of-segment positions; each duplicate pair is one
    // crossing with cuts at the segment ends. A revisit of a closed
    // subpath's start point pairs with the seam, the end of the last
    // segment.
    // Coincidence radius for "the path revisits this anchor", relative to
    // the subpath's own extent rather than absolute in user coordinates.
    // An absolute epsilon means something different for a shape authored in
    // pixels than for the same shape in a 0..1 coordinate system: too loose
    // at small scales, where it merges genuinely distinct vertices, and too
    // tight at large ones. Exactly coincident anchors — what real path data
    // carries — match under any radius. The radius is the one
    // canonicalization drops segments under, so the two ends of a segment
    // the input carried never read as a revisit.
    let vtx_eps2 = {
        let e = crate::normalize::RESOLUTION * crate::normalize::extent(subpath);
        (e * e).max(f64::MIN_POSITIVE)
    };
    for (i, j) in collect_revisits(&segs, closed, vtx_eps2) {
        push_crossing(&mut cuts, (i, 1.0), (j, 1.0));
    }

    if n_crossings == 0 {
        return None;
    }

    // ---- cut segments into arcs ----
    // Order cuts along the path.
    cuts.sort_by(|a, b| a.seg.cmp(&b.seg).then(a.t.total_cmp(&b.t)));
    let crossing_points: Vec<Point> = cuts
        .iter()
        .map(|c| segs[c.seg].eval(c.t.clamp(0.0, 1.0)))
        .collect();

    struct Arc {
        els: BezPath,
        s0: f64,
        end_crossing: Option<usize>,
        seam: Option<usize>,
    }
    let mut arcs: Vec<Arc> = Vec::new();
    let mut cur = BezPath::new();
    let mut cur_s0 = 0.0;
    let mut s = 0.0;
    let mut cut_ix = 0;
    for (i, seg) in segs.iter().enumerate() {
        let seg_len = if measure {
            crate::math::arclen(seg, ARCLEN_ACC)
        } else {
            0.0
        };
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
                seam: None,
            });
            // Measured, not `seg_len * t`: arc length is proportional to
            // the parameter only on a line. On a looping cubic the
            // difference can shift the dash pattern by several units.
            cur_s0 = if measure {
                s + crate::math::arclen(&seg.subsegment(0.0..t), ARCLEN_ACC)
            } else {
                0.0
            };
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
        seam: None,
    });

    // First arc may lack its MoveTo (starts at path start).
    if let Some(first) = arcs.first_mut() {
        if !matches!(first.els.elements().first(), Some(PathEl::MoveTo(_))) {
            let start = crate::normalize::start_point(subpath);
            let mut with_move = BezPath::new();
            with_move.move_to(start);
            with_move.extend(first.els.iter());
            first.els = with_move;
        }
    }

    // For a closed subpath the seam is not a cut, so merge the trailing arc
    // into the leading one. They are contiguous through the seam. The merged
    // arc records where the seam falls, because kurbo restarts the dash
    // phase there, as `SplitArc::seam` explains.
    if closed && arcs.len() >= 2 {
        if arcs.last().unwrap().els.elements().len() < 2 {
            // A cut landed exactly on the seam; the stub carries nothing.
            arcs.pop();
        } else {
            let last = arcs.pop().unwrap();
            let first_s0 = last.s0;
            let first = &mut arcs[0];
            let seam = last.els.elements().len();
            let past_seam = first.els.elements().len() >= 2;
            let mut merged = last.els;
            merged.extend(first.els.iter().skip(1)); // drop first's MoveTo
            first.els = merged;
            first.s0 = first_s0;
            first.seam = past_seam.then_some(seam);
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
                // Crossings opened inside the popped run go with it, which
                // is why interleaved patterns decompose only partially.
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
            // A degenerate sliver, from cuts closer together than T_EPS.
            // Give up on splitting rather than emit broken pieces.
            return None;
        }
        // A piece is closed when its endpoints coincide. Lobes always do.
        // The leftover chain does exactly when the subpath was closed.
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
            seam: arc.seam,
            shares_end: closed || arc.end_crossing.is_some(),
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
/// `next` marks `end(a) = start(b)` adjacency; `wrap` marks
/// `start(a) = end(b)`, the seam pair of a closed contour. Both hold at
/// once for a two-segment contour. Shared endpoints are trimmed off the
/// search domain by [`ADJ_TRIM`], so a smooth joint costs one bounding-box
/// rejection instead of a recursion to flatness, and the joint itself is
/// never reported as a crossing.
pub(crate) fn segment_pair_params(
    a: &PathSeg,
    b: &PathSeg,
    next: bool,
    wrap: bool,
    accuracy: f64,
) -> Vec<(f64, f64)> {
    pair_params(a, b, None, next, wrap, accuracy)
}

/// [`segment_pair_params`] for a caller that already has the exact boxes of
/// both segments, from `ParamCurveExtrema::bounding_box`, as the broad
/// phase does.
///
/// The search starts from those boxes instead of solving for them again.
/// They are the boxes it would solve for, so the result is the same. Most
/// pairs the broad phase passes are settled by the first step, so those
/// two solves were most of the search's cost. An adjacent pair, `next` or
/// `wrap`, is searched over a trimmed domain that the boxes of the whole
/// segments do not describe, so it solves for its own.
pub(crate) fn segment_pair_params_boxed(
    (a, a_box): (&PathSeg, Rect),
    (b, b_box): (&PathSeg, Rect),
    next: bool,
    wrap: bool,
    accuracy: f64,
) -> Vec<(f64, f64)> {
    // Any other box would change where the search goes, and so its result.
    // The check compares bits, so a `NaN` box matches itself.
    let same = |r: Rect, s: Rect| {
        [r.x0, r.y0, r.x1, r.y1].map(f64::to_bits) == [s.x0, s.y0, s.x1, s.y1].map(f64::to_bits)
    };
    debug_assert!(same(a_box, a.bounding_box()) && same(b_box, b.bounding_box()));
    pair_params(a, b, Some((a_box, b_box)), next, wrap, accuracy)
}

fn pair_params(
    a: &PathSeg,
    b: &PathSeg,
    boxes: Option<(Rect, Rect)>,
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
    let mut hits: Vec<Hit> = Vec::new();
    let mut budget = SEARCH_BUDGET;
    let accuracy = accuracy.max(1e-9);
    if next || wrap {
        // The pieces are trimmed, so the boxes of the whole segments do not
        // apply.
        let a_sub = a.subsegment(ar.0..ar.1);
        let b_sub = b.subsegment(br.0..br.1);
        recurse(
            &Piece::new(&a_sub, ar),
            &Piece::new(&b_sub, br),
            accuracy,
            0,
            &mut hits,
            &mut budget,
        );
    } else {
        let (a_box, b_box) = boxes.unwrap_or_else(|| (a.bounding_box(), b.bounding_box()));
        let a = Piece {
            seg: a,
            range: ar,
            bbox: a_box,
        };
        let b = Piece {
            seg: b,
            range: br,
            bbox: b_box,
        };
        recurse(&a, &b, accuracy, 0, &mut hits, &mut budget);
    }
    if hits.is_empty() {
        return Vec::new();
    }
    let mut out = polish_hits(a, b, hits, false, next, wrap);
    dedupe(&mut out);
    out
}

/// Subdivide a segment at the given parameters, which are sorted and
/// deduplicated in place. Interior cuts only.
pub(crate) fn subdivide_at(seg: &PathSeg, ts: &mut Vec<f64>) -> Vec<PathSeg> {
    clean_cuts(ts);
    let mut out = Vec::with_capacity(ts.len() + 1);
    subdivide_into(seg, ts, &mut out);
    out
}

/// Keep the interior cuts of `ts`, sorted. A cut closer than [`DEDUP_EPS`]
/// to the last one kept is dropped.
pub(crate) fn clean_cuts(ts: &mut Vec<f64>) {
    ts.retain(|t| *t > T_EPS && *t < 1.0 - T_EPS);
    ts.sort_by(f64::total_cmp);
    ts.dedup_by(|a, b| (*a - *b).abs() < DEDUP_EPS);
}

/// Append to `out` the pieces of `seg` between consecutive cuts of `ts`.
/// The cuts must already be cleaned by [`clean_cuts`].
pub(crate) fn subdivide_into(seg: &PathSeg, ts: &[f64], out: &mut Vec<PathSeg>) {
    let mut t0 = 0.0;
    for &t in ts {
        out.push(seg.subsegment(t0..t));
        t0 = t;
    }
    out.push(seg.subsegment(t0..1.0));
}

/// Append `seg` to `path`, starting the path at the segment's start point if
/// it is empty.
pub(crate) fn append_seg(path: &mut BezPath, seg: PathSeg) {
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

/// Self-intersections of one segment, meaning cubic loops. Split at the
/// curve's extrema, since monotone pieces cannot self-intersect, then
/// intersect the pieces pairwise. Adjacent pieces get their shared endpoint
/// trimmed off the domain, for the reason [`segment_pair_params`] gives.
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
    let mut hits: Vec<Hit> = Vec::new();
    let mut budget = SEARCH_BUDGET;
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
                &Piece::new(&pa, (a0, a1)),
                &Piece::new(&pb, (b0, b1)),
                accuracy.max(1e-9),
                0,
                &mut hits,
                &mut budget,
            );
        }
    }
    if hits.is_empty() {
        return Vec::new();
    }
    let mut out = polish_hits(seg, seg, hits, true, false, false);
    dedupe(&mut out);
    out
}

fn dedupe(hits: &mut Vec<(f64, f64)>) {
    hits.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.total_cmp(&y.1)));
    hits.dedup_by(|a, b| (a.0 - b.0).abs() < DEDUP_EPS && (a.1 - b.1).abs() < DEDUP_EPS);
}

/// Crossings at a smaller angle than this, as a sine, are not polished.
const MIN_POLISH_SINE: f64 = 1e-3;

/// A crossing found by [`recurse`]: its parameter on each segment, and the
/// parameter ranges of the two flat pieces it was found between.
#[derive(Copy, Clone)]
struct Hit {
    ta: f64,
    tb: f64,
    ar: (f64, f64),
    br: (f64, f64),
}

/// Move each crossing that the search found between `a` and `b` onto the
/// true crossing.
///
/// The search stops once both pieces are flat to within its accuracy. It
/// then intersects their chords and reads the result back as a parameter
/// along each piece. When the parameter runs unevenly along a curve, as on
/// a cubic with a control point bunched at one end, that lands well away
/// from the crossing: 11 units on a notch of Impact's `w` at accuracy 0.25.
/// The pieces cut there then do not meet where the curves do, and anything
/// probed or stitched at the cut reads the wrong side.
///
/// A polished crossing is kept only near the flat pieces it was found
/// between, within one piece length on either side, since that is where
/// the search placed it. Near tangency the curves stay close over a long
/// stretch, and the exact crossing can lie far along it or be a different
/// crossing altogether. It is also dropped when it collapses onto a point
/// that is no crossing. With `same` that is a segment meeting itself at
/// one parameter. With `next` or `wrap` it is two adjacent segments
/// meeting at their shared end, as in [`segment_pair_params`]. Whenever
/// the polished crossing is dropped, the search's own estimate is kept.
fn polish_hits(
    a: &PathSeg,
    b: &PathSeg,
    hits: Vec<Hit>,
    same: bool,
    next: bool,
    wrap: bool,
) -> Vec<(f64, f64)> {
    let near = |t: f64, (lo, hi): (f64, f64)| {
        let m = hi - lo;
        (lo - m..=hi + m).contains(&t)
    };
    let at_end = |t: f64| !(T_EPS..=1.0 - T_EPS).contains(&t);
    hits.into_iter()
        .map(|h| {
            // Where both strands end, the touch is a joint. Nothing
            // downstream cuts there, and a smooth offset reports dozens
            // of them.
            if at_end(h.ta) && at_end(h.tb) {
                return (h.ta, h.tb);
            }
            let (pa, pb) = polish_crossing(a, h.ta, b, h.tb);
            let collapsed = (same && (pa - pb).abs() < ADJ_TRIM)
                || (next && pa > 1.0 - ADJ_TRIM && pb < ADJ_TRIM)
                || (wrap && pa < ADJ_TRIM && pb > 1.0 - ADJ_TRIM);
            if near(pa, h.ar) && near(pb, h.br) && !collapsed {
                (pa, pb)
            } else {
                (h.ta, h.tb)
            }
        })
        .collect()
}

/// Run up to eight Newton steps on `a(ta) = b(tb)` from the given
/// parameters. Stop before the first step that would not bring the two
/// points closer.
///
/// A crossing within a thousandth of a radian of tangency is left as it
/// is. There the curves run together over a stretch, every point of which
/// is as good a crossing as any other, and Newton would slide along it as
/// far as rounding takes it.
pub(crate) fn polish_crossing(a: &PathSeg, ta: f64, b: &PathSeg, tb: f64) -> (f64, f64) {
    let (da, db) = (deriv(a, ta), deriv(b, tb));
    if db.cross(da).abs() < MIN_POLISH_SINE * da.hypot() * db.hypot() {
        return (ta, tb);
    }
    let (mut ta, mut tb) = (ta, tb);
    let mut gap = (a.eval(ta) - b.eval(tb)).hypot2();
    for _ in 0..8 {
        if gap == 0.0 {
            break;
        }
        let (da, db) = (deriv(a, ta), deriv(b, tb));
        let det = db.cross(da);
        if det == 0.0 {
            break;
        }
        // Solve `da·δa − db·δb = b(tb) − a(ta)`.
        let r = b.eval(tb) - a.eval(ta);
        let na = (ta + db.cross(r) / det).clamp(0.0, 1.0);
        let nb = (tb + da.cross(r) / det).clamp(0.0, 1.0);
        let next = (a.eval(na) - b.eval(nb)).hypot2();
        if next.is_nan() || next >= gap {
            break;
        }
        (ta, tb, gap) = (na, nb, next);
    }
    (ta, tb)
}

/// The derivative of `seg` at `t`.
pub(crate) fn deriv(seg: &PathSeg, t: f64) -> Vec2 {
    use kurbo::ParamCurveDeriv;
    match seg {
        PathSeg::Line(l) => l.p1 - l.p0,
        PathSeg::Quad(q) => q.deriv().eval(t).to_vec2(),
        PathSeg::Cubic(c) => c.deriv().eval(t).to_vec2(),
    }
}

const MAX_DEPTH: u32 = 28;

/// Node budget for one intersection search.
///
/// Bounding-box rejection normally prunes the subdivision tree to a few
/// dozen nodes. Nothing prunes it when every box overlaps every other and
/// flatness never converges, which is what overflow-scale coordinates
/// produce: differences of `1e300` are not finite when squared, so no
/// subdivision ever looks flat and no pair ever separates. The tree then
/// runs to `MAX_DEPTH` down every branch, and 2^28 nodes is not a
/// computation that finishes — a four-segment path was enough to hang the
/// call indefinitely.
///
/// The budget bounds that. A search that exhausts it returns the hits it
/// found so far, which for geometry this degenerate is no worse than the
/// alternative, and the caller stays responsive. Well-behaved input never
/// comes close to the limit.
const SEARCH_BUDGET: u32 = 10_000;

/// One operand of the intersection search: a piece of a segment, the
/// parameter range it covers on that segment, and its exact box.
///
/// Each step splits one operand and passes the other down unchanged. So a
/// box is solved for once, when its piece is made, and not again at each
/// later step that gets the piece. It is the same box those steps solved
/// for, so the search goes the same way. The piece borrows its segment,
/// so making one does not copy the curve.
#[derive(Copy, Clone)]
struct Piece<'a> {
    seg: &'a PathSeg,
    range: (f64, f64),
    bbox: Rect,
}

impl<'a> Piece<'a> {
    fn new(seg: &'a PathSeg, range: (f64, f64)) -> Self {
        Piece {
            seg,
            range,
            bbox: seg.bounding_box(),
        }
    }
}

fn recurse(
    a: &Piece<'_>,
    b: &Piece<'_>,
    accuracy: f64,
    depth: u32,
    out: &mut Vec<Hit>,
    budget: &mut u32,
) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    if !crate::broad::overlaps(a.bbox, b.bbox, accuracy) {
        return;
    }
    let fa = flatness(a.seg);
    let fb = flatness(b.seg);
    if (fa <= accuracy && fb <= accuracy) || depth >= MAX_DEPTH {
        // Chord/chord intersection.
        let (a0, a1) = (a.seg.start(), a.seg.end());
        let (b0, b1) = (b.seg.start(), b.seg.end());
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
        let (ar, br) = (a.range, b.range);
        let ta = ar.0 + s.clamp(0.0, 1.0) * (ar.1 - ar.0);
        let tb = br.0 + t.clamp(0.0, 1.0) * (br.1 - br.0);
        out.push(Hit { ta, tb, ar, br });
        return;
    }
    // Split one operand: the one that is not flat yet, or the less flat one
    // when neither is.
    if fa > accuracy && (fa >= fb || fb <= accuracy) {
        let (t0, t1) = a.range;
        let mid = 0.5 * (t0 + t1);
        let lo = a.seg.subsegment(0.0..0.5);
        let hi = a.seg.subsegment(0.5..1.0);
        let a_lo = Piece::new(&lo, (t0, mid));
        let a_hi = Piece::new(&hi, (mid, t1));
        recurse(&a_lo, b, accuracy, depth + 1, out, budget);
        recurse(&a_hi, b, accuracy, depth + 1, out, budget);
    } else {
        let (t0, t1) = b.range;
        let mid = 0.5 * (t0 + t1);
        let lo = b.seg.subsegment(0.0..0.5);
        let hi = b.seg.subsegment(0.5..1.0);
        let b_lo = Piece::new(&lo, (t0, mid));
        let b_hi = Piece::new(&hi, (mid, t1));
        recurse(a, &b_lo, accuracy, depth + 1, out, budget);
        recurse(a, &b_hi, accuracy, depth + 1, out, budget);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::{CubicBez, ParamCurveArclen, Shape};

    /// The crossings and revisits that the nested loops found before the
    /// broad phase replaced them, in the loops' order.
    fn nested_loop_crossings(
        segs: &[PathSeg],
        closed: bool,
        accuracy: f64,
        vtx_eps2: f64,
    ) -> Vec<((usize, f64), (usize, f64))> {
        let n = segs.len();
        let bboxes: Vec<Rect> = segs.iter().map(ParamCurveExtrema::bounding_box).collect();
        let mut out = Vec::new();
        for i in 0..n {
            for (t1, t2) in seg_self_intersections(&segs[i], accuracy) {
                out.push(((i, t1), (i, t2)));
            }
            for j in (i + 1)..n {
                if !crate::broad::overlaps(bboxes[i], bboxes[j], accuracy) {
                    continue;
                }
                let next = j == i + 1;
                let wrap = closed && i == 0 && j == n - 1;
                for (ta, tb) in segment_pair_params(&segs[i], &segs[j], next, wrap, accuracy) {
                    let a_end = !(T_EPS..=1.0 - T_EPS).contains(&ta);
                    let b_end = !(T_EPS..=1.0 - T_EPS).contains(&tb);
                    if !(a_end && b_end) {
                        out.push(((i, ta), (j, tb)));
                    }
                }
            }
        }
        for i in 0..n.saturating_sub(1) {
            for j in (i + 1)..n {
                let t_touch = j == n - 1 && !closed;
                if !t_touch && (segs[i].end() - segs[j].end()).hypot2() <= vtx_eps2 {
                    out.push(((i, 1.0), (j, 1.0)));
                }
            }
        }
        out
    }

    /// The broad phase gives the crossings and revisits in the order of the
    /// nested loops. That order numbers the crossings and orders the cuts
    /// on one point. Each path has enough segments that the broad phase
    /// sorts and sweeps instead of scanning all pairs: a self-crossing star
    /// polygon, a self-crossing curve, a rosette that revisits its centre
    /// six times, and a path that revisits nine anchors, each a little off.
    #[test]
    fn crossings_come_out_in_nested_loop_order() {
        let mut star = BezPath::new();
        for k in 0..19 {
            let a = core::f64::consts::TAU * (7 * k) as f64 / 19.0;
            let p = (100.0 * a.cos(), 100.0 * a.sin());
            if k == 0 {
                star.move_to(p);
            } else {
                star.line_to(p);
            }
        }
        star.close_path();
        let mut curve = BezPath::new();
        curve.move_to((0.0, 0.0));
        for k in 0..20 {
            let x = 12.0 * k as f64;
            curve.curve_to(
                (x + 40.0, 60.0),
                (x - 30.0, 60.0),
                (x + 12.0, (k % 3) as f64),
            );
        }
        let mut rosette = BezPath::new();
        rosette.move_to((0.0, 0.0));
        for k in 0..6 {
            let a = core::f64::consts::TAU * k as f64 / 6.0;
            let at = |d: f64, off: f64| (d * (a + off).cos(), d * (a + off).sin());
            rosette.line_to(at(50.0, -0.2));
            rosette.line_to(at(60.0, 0.2));
            rosette.line_to((0.0, 0.0));
        }
        rosette.close_path();
        // Twice round a nonagon, taking every second vertex. Each of the
        // nine anchors is revisited once. The second visit is a little off
        // the first: within the revisit radius 1e-6, but not on the point.
        let mut twice = BezPath::new();
        for k in 0..18 {
            let a = core::f64::consts::TAU * ((2 * k) % 9) as f64 / 9.0;
            let off = if k < 9 { 0.0 } else { 0.7e-6 };
            let p = (80.0 * a.cos() + off, 80.0 * a.sin() + off);
            if k == 0 {
                twice.move_to(p);
            } else {
                twice.line_to(p);
            }
        }
        twice.close_path();
        for (path, closed) in [(star, true), (curve, false), (rosette, true), (twice, true)] {
            let segs: Vec<PathSeg> = path.segments().collect();
            assert!(segs.len() > crate::broad::SCAN_MAX);
            let bboxes: Vec<Rect> = segs.iter().map(ParamCurveExtrema::bounding_box).collect();
            let vtx_eps2 = 1e-12;
            let mut swept: Vec<_> = collect_crossings(&segs, &bboxes, closed, 1e-3).collect();
            let revisits = collect_revisits(&segs, closed, vtx_eps2);
            swept.extend(revisits.into_iter().map(|(i, j)| ((i, 1.0), (j, 1.0))));
            let nested = nested_loop_crossings(&segs, closed, 1e-3, vtx_eps2);
            assert!(nested.len() > 5, "{} crossings", nested.len());
            assert_eq!(swept, nested);
        }
    }

    /// Two nearly straight cubics cross at about 20°, each with a control
    /// point bunched at its start. At accuracy 4 they are already flat, so
    /// the search intersects their chords, and the chord fraction read back
    /// as a parameter lands far along the curve from the crossing. At every
    /// accuracy tested, the reported crossing must lie on both curves.
    #[test]
    fn crossings_land_on_both_curves() {
        let a = PathSeg::Cubic(CubicBez::new(
            (0.0, 0.0),
            (0.05, 0.5),
            (3.0, 30.0),
            (18.0, 100.0),
        ));
        let b = PathSeg::Cubic(CubicBez::new(
            (14.0, 0.0),
            (13.95, 0.5),
            (11.0, 30.0),
            (-4.0, 100.0),
        ));
        let mut raw = Vec::new();
        let mut budget = SEARCH_BUDGET;
        recurse(
            &Piece::new(&a, (0.0, 1.0)),
            &Piece::new(&b, (0.0, 1.0)),
            4.0,
            0,
            &mut raw,
            &mut budget,
        );
        let estimate = raw.iter().map(|h| (a.eval(h.ta) - b.eval(h.tb)).hypot());
        assert!(
            estimate.fold(0.0, f64::max) > 1.0,
            "the search's own estimate should miss here"
        );
        for accuracy in [4.0, 0.25, 1e-3] {
            let hits = segment_pair_params(&a, &b, false, false, accuracy);
            assert_eq!(hits.len(), 1, "accuracy {accuracy}: {hits:?}");
            let (ta, tb) = hits[0];
            let gap = (a.eval(ta) - b.eval(tb)).hypot();
            assert!(
                gap < 1e-9,
                "accuracy {accuracy}: cut {gap} off the crossing"
            );
        }
    }

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
        assert!(split_self_intersections(rect.elements(), true, 1e-3, false).is_none());
        let circle = kurbo::Circle::new((0.0, 0.0), 10.0).to_path(1e-6);
        assert!(split_self_intersections(circle.elements(), true, 1e-3, false).is_none());
    }

    #[test]
    fn bowtie_splits_into_two_lobes() {
        let p = bowtie();
        let res = split_self_intersections(p.elements(), true, 1e-3, false).expect("must split");
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
        let res = split_self_intersections(p.elements(), false, 1e-3, true).expect("must split");
        // One closed loop lobe + one open leftover strand.
        assert_eq!(res.pieces.len(), 2);
        let closed_count = res.pieces.iter().filter(|p| p.closed).count();
        assert_eq!(closed_count, 1, "exactly one closed loop");
        // Each arc starts at the arc length where the arcs before it end.
        let mut s = 0.0;
        for arc in &res.arcs {
            assert!((arc.s0 - s).abs() < 1e-6, "s0 {} against {s}", arc.s0);
            s += arc.els.segments().map(|seg| seg.arclen(1e-9)).sum::<f64>();
        }
        assert!(s > 100.0);
        // Without measuring, every arc reads 0.
        let res = split_self_intersections(p.elements(), false, 1e-3, false).expect("must split");
        assert!(res.arcs.iter().all(|arc| arc.s0 == 0.0));
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
        let res = split_self_intersections(p.elements(), true, 1e-3, false).expect("must split");
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
