// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Distance-pruned offset regions for one-sided bands on closed contours.
//!
//! The band of a closed contour is defined set-theoretically, matching
//! Figma, whose inside and outside strokes are a doubled centered stroke
//! masked by the fill:
//!
//! ```text
//! Inside(w)  = { p ∈ F : dist(p, path) ≤ w } = F \ E
//! Outside(w) = { p ∉ F : dist(p, path) ≤ w } = D \ F
//! ```
//!
//! `F` is the filled region of the whole path under nonzero winding, `E` is
//! its erosion by `w`, and `D` is its dilation. The outline is then the
//! source contours plus the boundary of the eroded or dilated region, wound
//! the other way, so the whole problem reduces to computing `∂E` and `∂D`.
//!
//! Those are not the naive offset curves: past the local thickness the
//! offset self-intersects and inverts. They are computed the classical way.
//! Take the raw offset of every contour, cut it at every intersection with
//! itself and with the others, then discard each piece that is nearer than
//! `w` to the path, on the wrong side of the fill, or inside another loop's
//! region, and stitch what survives. That last test is a winding test
//! against the raw loops themselves, which miter and bevel joins require —
//! their loops deviate from the distance-`w` set at corners. Cutting across
//! contours is what makes overlapping bands merge into one boundary instead
//! of two overlapping ones, which nonzero winding could not express.
//!
//! When nothing survives, the erosion is empty and the stroke saturates:
//! the shape fills completely, as Figma renders a wedge, a thin star, or a
//! rectangle narrower than `2w`. Dropped pieces whose neighbours survive
//! are rejoined with a straight line, which reproduces bevel-join chords
//! exactly.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use kurbo::{BezPath, ParamCurve, PathEl, PathSeg, Point, Rect, Shape};

use crate::expand::{Band, BandParams};
use crate::math;
use crate::split;
use crate::style::StrokeSide;

/// Relative slack on the `dist ≥ w` test, absorbing offset approximation
/// error so legitimate boundary pieces are never pruned.
const PRUNE_SLACK: f64 = 2e-3;

/// Which region boundary to build.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum RegionKind {
    /// `∂E`: offsets run into the fill; the region is `F` shrunk by `w`.
    Erosion,
    /// `∂D`: offsets run away from the fill; the region is `F` grown by `w`.
    Dilation,
}

/// One closed contour and the side its fill lies on.
pub(crate) struct ContourSpec<'a> {
    pub els: &'a [PathEl],
    pub fill_side: StrokeSide,
}

/// Y-bucketed flattened edges, shared by [`SourceIndex`] and [`RawIndex`].
///
/// An edge is `(a, b, tag, real)`: the flattened segment, the ordinal of
/// the contour or raw loop it belongs to, and whether it is part of the
/// path for distance purposes. `real == false` marks an implicit closing
/// chord, which counts for winding only. The predicates take a per-edge
/// filter, so each wrapper can scope a query to one tag or away from it.
struct EdgeIndex {
    edges: Vec<(Point, Point, u32, bool)>,
    buckets: Vec<Vec<u32>>,
    y0: f64,
    inv_h: f64,
}

impl EdgeIndex {
    fn from_edges(edges: Vec<(Point, Point, u32, bool)>) -> EdgeIndex {
        let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
        for (a, b, _, _) in &edges {
            ymin = ymin.min(a.y).min(b.y);
            ymax = ymax.max(a.y).max(b.y);
        }
        let n_buckets = (edges.len() / 4).clamp(1, 256);
        let mut buckets: Vec<Vec<u32>> = alloc::vec![Vec::new(); n_buckets];
        if edges.is_empty() {
            return EdgeIndex {
                edges,
                buckets,
                y0: 0.0,
                inv_h: 0.0,
            };
        }
        let inv_h = n_buckets as f64 / (ymax - ymin).max(1e-12);
        for (i, (a, b, _, _)) in edges.iter().enumerate() {
            let lo = (((a.y.min(b.y) - ymin) * inv_h) as usize).min(n_buckets - 1);
            let hi = (((a.y.max(b.y) - ymin) * inv_h) as usize).min(n_buckets - 1);
            for bucket in &mut buckets[lo..=hi] {
                bucket.push(i as u32);
            }
        }
        EdgeIndex {
            edges,
            buckets,
            y0: ymin,
            inv_h,
        }
    }

    /// The bucket containing `y` (there is always at least one bucket).
    fn bucket_ix(&self, y: f64) -> usize {
        let n = self.buckets.len();
        crate::math::floor((y - self.y0) * self.inv_h).clamp(0.0, (n - 1) as f64) as usize
    }

    /// Nonzero winding at `p` of the edges whose tag passes `keep`.
    ///
    /// Exactly one bucket is scanned. An edge crossing `y = p.y` always
    /// spans that bucket, and visiting a neighbour would double-count edges
    /// registered in both, breaking the cancellation that makes the winding
    /// zero outside the shape.
    fn winding_where(&self, p: Point, keep: impl Fn(u32) -> bool) -> i32 {
        let mut w = 0;
        for &ix in &self.buckets[self.bucket_ix(p.y)] {
            let (a, b, tag, _) = self.edges[ix as usize];
            if !keep(tag) {
                continue;
            }
            if a.y <= p.y {
                if b.y > p.y && (b - a).cross(p - a) > 0.0 {
                    w += 1;
                }
            } else if b.y <= p.y && (b - a).cross(p - a) < 0.0 {
                w -= 1;
            }
        }
        w
    }

    /// Whether any edge passing `keep` lies within `sqrt(r_sq)` of `p`.
    ///
    /// An early-exit scan: an edge whose box is already beyond the radius
    /// cannot qualify, and the first hit ends the scan. Callers
    /// special-case non-positive `r_sq`.
    fn within_where(&self, p: Point, r_sq: f64, keep: impl Fn(u32, bool) -> bool) -> bool {
        let n = self.buckets.len();
        let r = crate::math::sqrt(r_sq);
        let lo = crate::math::floor((p.y - r - self.y0) * self.inv_h).clamp(0.0, (n - 1) as f64)
            as usize;
        let hi =
            crate::math::ceil((p.y + r - self.y0) * self.inv_h).clamp(0.0, (n - 1) as f64) as usize;
        for bi in lo..=hi {
            for &ix in &self.buckets[bi] {
                let (a, b, tag, real) = self.edges[ix as usize];
                if !keep(tag, real) {
                    continue;
                }
                let (x0, x1) = if a.x <= b.x { (a.x, b.x) } else { (b.x, a.x) };
                let dx = (x0 - p.x).max(p.x - x1).max(0.0);
                if dx * dx >= r_sq {
                    continue;
                }
                let ab = b - a;
                let len2 = ab.hypot2();
                let t = if len2 > 0.0 {
                    ((p - a).dot(ab) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                if ((p - a) - t * ab).hypot2() < r_sq {
                    return true;
                }
            }
        }
        false
    }
}

/// Source geometry prepared for the two hot predicates: is this point
/// inside the region, and is it far enough from the path.
///
/// Both would otherwise be a per-curve solve — [`kurbo::Shape::winding`]
/// and [`kurbo::ParamCurveNearest::nearest`] — run once per candidate
/// piece, which dominates the pipeline. The path is flattened once instead
/// and its edges bucketed by `y`, so a query touches only the edges that
/// can matter. Flattening error is folded into the distance threshold.
///
/// Edge tags are subpath ordinals, so the distance test can be restricted
/// to a piece's own contour. Cross-contour validity is a winding question,
/// not a distance one; see `region_loops`.
pub(crate) struct SourceIndex {
    index: EdgeIndex,
    flat_tol: f64,
}

/// Lower bound on a flattening tolerance, as a fraction of the geometry's
/// own extent.
///
/// The number of line segments [`kurbo::flatten`] produces grows as the
/// curve's size over the tolerance, and nothing upstream bounds it: a path
/// spanning `1e300` flattened at `1e-3` asks for more segments than a `Vec`
/// can describe, and the allocation aborts the process rather than
/// returning. Resolving a shape to better than a billionth of itself is not
/// meaningful in `f64` regardless, so tolerances finer than that are raised.
/// At ordinary coordinate magnitudes this floor never binds.
const MIN_RELATIVE_FLAT_TOL: f64 = 1e-9;

impl SourceIndex {
    pub(crate) fn new(path: &BezPath, flat_tol: f64) -> Self {
        let extent = path.control_box().size().max_side();
        let flat_tol = flat_tol.max(extent * MIN_RELATIVE_FLAT_TOL).max(1e-9);
        let mut edges: Vec<(Point, Point, u32, bool)> = Vec::new();
        let mut last: Option<Point> = None;
        let mut start: Option<Point> = None;
        let mut sub: u32 = 0;
        let mut seen_move = false;
        let close = |edges: &mut Vec<(Point, Point, u32, bool)>,
                     last: Option<Point>,
                     s: Option<Point>,
                     sub: u32| {
            if let (Some(prev), Some(s)) = (last, s) {
                if (s - prev).hypot2() > 0.0 {
                    edges.push((prev, s, sub, false));
                }
            }
        };
        kurbo::flatten(path.iter(), flat_tol, |el| match el {
            PathEl::MoveTo(p) => {
                close(&mut edges, last, start, sub);
                if seen_move {
                    sub += 1;
                } else {
                    seen_move = true;
                }
                last = Some(p);
                start = Some(p);
            }
            PathEl::LineTo(p) => {
                if let Some(prev) = last {
                    if (p - prev).hypot2() > 0.0 {
                        edges.push((prev, p, sub, true));
                    }
                }
                last = Some(p);
            }
            PathEl::ClosePath => {
                if let (Some(prev), Some(s)) = (last, start) {
                    if (s - prev).hypot2() > 0.0 {
                        edges.push((prev, s, sub, true));
                    }
                }
                last = start;
            }
            _ => {}
        });
        close(&mut edges, last, start, sub);

        SourceIndex {
            index: EdgeIndex::from_edges(edges),
            flat_tol,
        }
    }

    /// Whether `p` is at least `sqrt(thresh_sq)` away from contour
    /// `contour` of the path (`None`: away from the whole path).
    ///
    /// Implicit closing chords are not part of the path for distance.
    pub(crate) fn is_clear_of(&self, p: Point, thresh_sq: f64, contour: Option<usize>) -> bool {
        if thresh_sq <= 0.0 {
            return true;
        }
        !self.index.within_where(p, thresh_sq, |tag, real| {
            real && contour.is_none_or(|c| c as u32 == tag)
        })
    }

    /// Nonzero winding number of the flattened path at `p` (implicit closing
    /// chords included).
    pub(crate) fn winding(&self, p: Point) -> i32 {
        self.index.winding_where(p, |_| true)
    }

    /// Flattening tolerance, subtracted from distance thresholds.
    fn slack(&self) -> f64 {
        self.flat_tol
    }
}

/// Flattened raw offset loops with per-edge owner tags.
///
/// Backs the cross-contour membership test: a piece of loop `i` belongs to
/// the region boundary only where the other loops' winding matches the
/// expected value. Distance to the source path cannot express that for
/// miter and bevel joins, whose loops deviate from the distance-`w` set at
/// corners. A miter wedge reaches past `w`, and an arc of another contour
/// hiding inside that wedge still has to be pruned.
struct RawIndex {
    index: EdgeIndex,
}

impl RawIndex {
    fn new(raws: &[BezPath], flat_tol: f64) -> RawIndex {
        let extent = raws
            .iter()
            .map(|r| r.control_box().size().max_side())
            .fold(0.0, f64::max);
        let flat_tol = flat_tol.max(extent * MIN_RELATIVE_FLAT_TOL);
        let mut edges: Vec<(Point, Point, u32, bool)> = Vec::new();
        for (ix, raw) in raws.iter().enumerate() {
            let mut last: Option<Point> = None;
            let mut start: Option<Point> = None;
            kurbo::flatten(raw.iter(), flat_tol.max(1e-9), |el| match el {
                PathEl::MoveTo(p) => {
                    last = Some(p);
                    start = Some(p);
                }
                PathEl::LineTo(p) => {
                    if let Some(prev) = last {
                        if (p - prev).hypot2() > 0.0 {
                            edges.push((prev, p, ix as u32, true));
                        }
                    }
                    last = Some(p);
                }
                PathEl::ClosePath => {
                    if let (Some(prev), Some(s)) = (last, start) {
                        if (s - prev).hypot2() > 0.0 {
                            edges.push((prev, s, ix as u32, true));
                        }
                    }
                    last = start;
                }
                _ => {}
            });
            // The raw loops are explicitly closed, but guard anyway.
            if let (Some(prev), Some(s)) = (last, start) {
                if (s - prev).hypot2() > 0.0 {
                    edges.push((prev, s, ix as u32, true));
                }
            }
        }
        RawIndex {
            index: EdgeIndex::from_edges(edges),
        }
    }

    /// Nonzero winding at `p` of every loop except `owner`'s.
    fn winding_excluding(&self, p: Point, owner: usize) -> i32 {
        self.index.winding_where(p, |tag| tag as usize != owner)
    }

    /// Whether `p` lies within `sqrt(r_sq)` of any loop except `owner`'s.
    ///
    /// A piece that close to another loop cannot be classified by winding,
    /// because the loops locally coincide — a donut at exactly half its
    /// ring thickness does this. Pruning it collapses sub-resolution
    /// regions into clean saturation.
    fn is_near_other(&self, p: Point, r_sq: f64, owner: usize) -> bool {
        if r_sq <= 0.0 {
            return false;
        }
        self.index
            .within_where(p, r_sq, |tag, _| tag as usize != owner)
    }
}

/// Build the boundary loops of the eroded or dilated region.
///
/// An empty result for [`RegionKind::Erosion`] means the erosion vanished:
/// the caller emits the source contours alone and the stroke saturates.
#[allow(clippy::too_many_arguments)]
pub(crate) fn region_loops(
    contours: &[ContourSpec<'_>],
    kind: RegionKind,
    index: &SourceIndex,
    width: f64,
    base: &BandParams<'_>,
    accuracy: f64,
    left: &mut BezPath,
    right: &mut BezPath,
    scratch: &mut BezPath,
    sink: &mut BezPath,
) -> Vec<BezPath> {
    // Resolution of the pruning predicate. Offset approximation, tolerance
    // and flattening error stack up to about this length; features smaller
    // than it fall inside the documented boundary band.
    let fuzz = PRUNE_SLACK * width + 2.0 * base.tolerance + index.slack();

    // --- raw offsets, one per contour ---
    let mut raws: Vec<BezPath> = Vec::with_capacity(contours.len());
    // Per-raw, per-segment relaxation of the distance prune: `1` for plain
    // offset geometry, `cos(φ/2)` where a bevel chord legitimately cuts
    // its corner. See `keep_at`.
    let mut raws_relax: Vec<Vec<f64>> = Vec::with_capacity(contours.len());
    for c in contours {
        let side = match kind {
            RegionKind::Erosion => c.fill_side,
            RegionKind::Dilation => c.fill_side.opposite(),
        };
        let (d_left, d_right) = crate::orient::side_distances(side, width);
        let params = BandParams {
            d_left,
            d_right,
            join_thresh: 2.0 * base.tolerance / width.max(1e-12),
            raw_offset: true,
            ..*base
        };
        let join_spans = {
            let mut band = Band::new(params, left, right, scratch, sink);
            band.suppress_finish = true;
            band.run(c.els.iter().copied());
            if side == StrokeSide::Left {
                core::mem::take(&mut band.left_join_segs)
            } else {
                core::mem::take(&mut band.right_join_segs)
            }
        };
        let raw: &BezPath = if side == StrokeSide::Left {
            left
        } else {
            right
        };
        if raw.elements().len() < 2 {
            raws.push(BezPath::new());
            raws_relax.push(Vec::new());
            continue;
        }
        let mut closed = raw.clone();
        closed.close_path();
        // Collapsed offset, where the width reaches the local thickness:
        // the loop degenerates to a tolerance-scale cluster, which
        // `offset_cubic`'s cusp handling can shatter into thousands of
        // segments, and the pairwise cut of that cluster is quadratic in
        // them. A loop smaller than the pruning fuzz cannot bound a
        // resolvable region feature, so drop it before it reaches the
        // cutter.
        if closed.control_box().size().max_side() <= fuzz {
            raws.push(BezPath::new());
            raws_relax.push(Vec::new());
            continue;
        }
        let n_segs = closed.segments().count();
        let mut relax = alloc::vec![1.0; n_segs];
        for (s0, s1, factor) in join_spans {
            for r in relax
                .iter_mut()
                .take((s1 as usize).min(n_segs))
                .skip(s0 as usize)
            {
                *r = f64::min(*r, factor);
            }
        }
        raws.push(closed);
        raws_relax.push(relax);
    }

    // --- find every mutual and self intersection of the offsets ---
    let cut = CutSegs::collect(&raws, &raws_relax, accuracy);

    // Approximation error is subtracted from the `dist >= w` test so that a
    // legitimate boundary piece is never pruned. Below roughly twice the
    // tolerance that subtraction consumes the whole width, and since
    // `is_clear_of` treats a non-positive threshold as "always clear", the
    // fold-over guard would switch off entirely — silently, and precisely
    // for hairline strokes. Keep a fraction of the width instead: a
    // boundary piece sits at `w` and still passes, while a fold-over piece
    // at a fraction of `w` is still rejected. At any width where the
    // subtraction leaves something meaningful this floor never binds.
    let thresh = (width * (1.0 - PRUNE_SLACK) - 2.0 * base.tolerance - index.slack())
        .max(0.25 * width)
        .max(0.0);
    let want_filled = kind == RegionKind::Erosion;
    // Cross-contour membership needs the other raw loops' winding. With
    // miter and bevel joins a loop deviates from the distance-`w` set at
    // corners, so distance alone cannot tell whether a piece hides inside
    // another contour's region. The expected winding follows the owning
    // loop's orientation: 0 for outer-like (CW) loops, +1 for hole-like
    // (CCW) loops, whose enclosing outer loop contributes one turn. This is
    // only needed when two or more loops are live.
    //
    // Orientation comes from the source contour, whose travel direction the
    // loop inherits. The raw loop's own net area is unreliable when the
    // offset self-intersects, as in the overshoot star of a sharp
    // triangle's inward offset.
    let cw: Vec<bool> = contours.iter().map(|c| c.els.area() >= 0.0).collect();
    let live = raws.iter().filter(|r| r.elements().len() >= 2).count();
    let raw_index = (live >= 2).then(|| RawIndex::new(&raws, index.slack()));
    // The join relaxation applies to the dilation only. There, cutting a
    // corner is the style the caller asked for, and the strict test tears
    // the union apart; the dilation never folds inward, so nothing needs
    // the slack as a guard. For the erosion the distance test is the
    // saturation guard — a spurious survivor becomes a hole in a shape that
    // should be solid — and dropping a bevel chord costs nothing there,
    // because `stitch` bridges the gap with the same chord.
    let relax_joins = kind == RegionKind::Dilation;
    let keep_at = |p: Point, owner: usize, relax: f64| {
        // Self-validity: far enough from the piece's own contour, which
        // prunes fold-over past the local thickness. Only the own contour.
        // Proximity to another contour says nothing under miter and bevel
        // joins, whose loops deviate from the distance-`w` set at corners;
        // cross-contour validity is the winding test below. A bevel chord
        // legitimately cuts its corner, so its own threshold is scaled by
        // `cos(φ/2)`. That bound matters: a blanket exemption let huge join
        // fans survive inside the shape at extreme widths.
        let thr = if relax_joins { thresh * relax } else { thresh };
        if !index.is_clear_of(p, thr * thr, Some(owner)) {
            return false;
        }
        // The right side of the fill.
        if (index.winding(p) != 0) != want_filled {
            return false;
        }
        // Not swallowed by the other loops' region, not escaped from it,
        // and not so close to another loop that the winding answer is fuzz.
        match &raw_index {
            None => true,
            Some(ri) => {
                let expected = if cw[owner] { 0 } else { 1 };
                ri.winding_excluding(p, owner) == expected
                    && !ri.is_near_other(p, fuzz * fuzz, owner)
            }
        }
    };

    // Three samples, majority. Classifying a piece by its midpoint alone
    // assumes validity is constant along it, which holds only while the cut
    // set is complete — and it deliberately is not. Tangential touches are
    // ignored, hits closer than `DEDUP_EPS` are merged, and the subdivision
    // search stops at `MAX_DEPTH` and falls back to a chord solve. Any of
    // those can leave a piece whose validity genuinely changes inside it,
    // and one sample then picks a side arbitrarily: a hole in a region that
    // should be solid, or a spur outside one.
    // The first two samples settle the majority whenever they agree, which
    // is the overwhelmingly common case, so the third is only paid for on a
    // piece that actually straddles a decision.
    let keep_piece = |seg: &PathSeg, owner: usize, relax: f64| {
        let first = keep_at(seg.eval(0.5), owner, relax);
        if keep_at(seg.eval(0.25), owner, relax) == first {
            return first;
        }
        keep_at(seg.eval(0.75), owner, relax)
    };

    // --- fast path: nothing cuts ---
    // Validity flips only across an intersection, since segment joints are
    // already piece boundaries. With no interior cuts every loop is
    // uniformly valid or invalid, so classify each by a few midpoint
    // samples and skip subdivision, per-piece pruning, and stitching.
    // Samples can only disagree within the boundary band; fall back to the
    // full pipeline when they do.
    if !cut.any_interior {
        let mut keep = alloc::vec![false; raws.len()];
        let mut mixed = false;
        for (ri, raw) in raws.iter().enumerate() {
            let n = raw.segments().count();
            if n == 0 {
                continue;
            }
            let sample_ixs = [0, n / 2, n - 1];
            let (mut valid, mut invalid) = (0usize, 0usize);
            for (ix, seg) in raw.segments().enumerate() {
                if sample_ixs.contains(&ix) {
                    let relax = raws_relax[ri].get(ix).copied().unwrap_or(1.0);
                    if keep_at(seg.eval(0.5), ri, relax) {
                        valid += 1;
                    } else {
                        invalid += 1;
                    }
                }
            }
            if invalid == 0 {
                keep[ri] = substantial_loop(raw, accuracy);
            } else if valid > 0 {
                mixed = true;
                break;
            }
        }
        if !mixed {
            let loops = raws
                .into_iter()
                .zip(keep)
                .filter_map(|(raw, keep)| keep.then_some(raw))
                .collect();
            return drop_unresolvable(loops, fuzz);
        }
    }

    // --- cut, prune, stitch ---
    let kept: Vec<Option<(usize, PathSeg)>> = cut
        .materialize()
        .into_iter()
        .map(|(owner, seg, relax)| keep_piece(&seg, owner, relax).then_some((owner, seg)))
        .collect();

    drop_unresolvable(stitch(&kept, accuracy, width), fuzz)
}

/// Empty out a region thinner than the pipeline's resolution.
///
/// Two different things make a region paint nothing. A single loop can be
/// thin relative to its own perimeter, which is a per-loop test. Or a pair
/// of nearly coincident loops can cancel under the nonzero rule, which is
/// what a donut stroked at exactly half its ring thickness produces: each
/// loop bounds real area, but together they enclose an annulus thinner than
/// the pipeline can resolve.
///
/// Only the second needs the loops' *net* signed area, and that quantity
/// means what it should only while every loop carries the orientation the
/// nonzero rule expects. `stitch` can hand the walk across contours, which
/// is exactly where that bookkeeping is easiest to lose, and a single
/// reversed component would cancel a legitimate one and discard the whole
/// region — a wide stroke silently filling solid, indistinguishable from
/// intended saturation. So the net test applies only once the loops are
/// confirmed to run within `fuzz` of one another, which is the geometry a
/// real cancel pair has. Components that merely happen to sum near zero
/// keep their loops.
fn drop_unresolvable(loops: Vec<BezPath>, fuzz: f64) -> Vec<BezPath> {
    let perimeter_of = |l: &BezPath| {
        let mut per = 0.0;
        for seg in l.segments() {
            per += (seg.end() - seg.start()).hypot();
        }
        per
    };
    let loops: Vec<BezPath> = loops
        .into_iter()
        .filter(|l| l.elements().area().abs() > 0.5 * fuzz * perimeter_of(l).max(1.0))
        .collect();
    if loops.len() < 2 {
        return loops;
    }
    let net: f64 = loops.iter().map(|l| l.elements().area()).sum();
    let perimeter: f64 = loops.iter().map(&perimeter_of).sum();
    if net.abs() > 0.5 * fuzz * perimeter.max(1.0) {
        return loops;
    }
    let index = RawIndex::new(&loops, fuzz * 0.25);
    let coincident = loops.iter().enumerate().all(|(i, l)| {
        l.segments()
            .all(|seg| index.is_near_other(seg.eval(0.5), fuzz * fuzz, i))
    });
    if coincident { Vec::new() } else { loops }
}

/// The segments of a family of closed offset curves, with every mutual and
/// self intersection collected as cut parameters.
struct CutSegs {
    segs: Vec<PathSeg>,
    owner: Vec<usize>,
    /// Distance-prune relaxation per segment (see `region_loops`).
    relax: Vec<f64>,
    cuts: Vec<Vec<f64>>,
    /// Whether any cut lands in a segment's interior. `subdivide_at` drops
    /// endpoint touches anyway, so without interior cuts the subdivision is
    /// a no-op and the fast path applies.
    any_interior: bool,
}

impl CutSegs {
    fn collect(raws: &[BezPath], raws_relax: &[Vec<f64>], accuracy: f64) -> CutSegs {
        // Flatten to a segment list, remembering the owner and the index
        // range of each contour so adjacency can be recognised.
        let mut segs: Vec<PathSeg> = Vec::new();
        let mut owner: Vec<usize> = Vec::new();
        let mut relax: Vec<f64> = Vec::new();
        let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(raws.len());
        for (ix, raw) in raws.iter().enumerate() {
            let start = segs.len();
            for (k, seg) in raw.segments().enumerate() {
                segs.push(seg);
                owner.push(ix);
                relax.push(raws_relax[ix].get(k).copied().unwrap_or(1.0));
            }
            ranges.push((start, segs.len()));
        }
        let n = segs.len();
        let bboxes: Vec<Rect> = segs
            .iter()
            .map(kurbo::ParamCurveExtrema::bounding_box)
            .collect();
        let mut cuts: Vec<Vec<f64>> = alloc::vec![Vec::new(); n];
        let mut any_interior = false;
        let add = |cuts: &mut Vec<Vec<f64>>, any: &mut bool, ix: usize, t: f64| {
            *any |= t > split::T_EPS && t < 1.0 - split::T_EPS;
            cuts[ix].push(t);
        };

        for i in 0..n {
            for t in split::segment_self_intersection_params(&segs[i], accuracy) {
                add(&mut cuts, &mut any_interior, i, t);
            }
            for j in (i + 1)..n {
                // Cheap box rejection before the subdivision search.
                if !split::overlaps(bboxes[i], bboxes[j], accuracy) {
                    continue;
                }
                let same = owner[i] == owner[j];
                let (lo, hi) = ranges[owner[i]];
                let next = same && j == i + 1;
                let wrap = same && i == lo && j == hi - 1 && hi - lo > 1;
                for (ta, tb) in split::segment_pair_params(&segs[i], &segs[j], next, wrap, accuracy)
                {
                    add(&mut cuts, &mut any_interior, i, ta);
                    add(&mut cuts, &mut any_interior, j, tb);
                }
            }
        }
        CutSegs {
            segs,
            owner,
            relax,
            cuts,
            any_interior,
        }
    }

    /// Subdivide every segment at its cuts. Returns `(owner, piece, relax)`
    /// in owner-major path order.
    fn materialize(mut self) -> Vec<(usize, PathSeg, f64)> {
        let mut out = Vec::with_capacity(self.segs.len());
        for i in 0..self.segs.len() {
            for piece in split::subdivide_at(&self.segs[i], &mut self.cuts[i]) {
                out.push((self.owner[i], piece, self.relax[i]));
            }
        }
        out
    }
}

/// Whether a closed loop bounds meaningful area: its absolute signed area
/// must exceed a perimeter-proportional sliver threshold. This catches both
/// tiny loops, where the erosion collapses at the saturation width, and
/// long near-degenerate rings, the fuzz stitched from two nearly coincident
/// offsets when a donut's inward offsets meet in the middle.
fn substantial_loop(path: &BezPath, accuracy: f64) -> bool {
    if path.elements().len() < 4 {
        return false;
    }
    let mut perimeter = 0.0;
    for seg in path.segments() {
        perimeter += (seg.end() - seg.start()).hypot();
    }
    path.elements().area().abs() > 2.0 * accuracy * perimeter.max(1.0)
}

/// Reassemble surviving pieces into closed loops.
///
/// At an intersection the surviving strand continues from the same point,
/// so the walk hands over to whichever unused piece starts there. That
/// includes a piece belonging to another contour, which is how overlapping
/// bands merge into a single boundary. Elsewhere — at a dropped bevel
/// chord or a trimmed spike — it falls through to the next survivor of the
/// same contour and bridges with a line. For a bevel join that line is the
/// chord itself.
/// Bridge chords longer than this multiple of the band width are refused.
///
/// A legitimate bridge replaces dropped join geometry, so it is bounded by
/// the join: a bevel chord spans at most `2w`, and a miter leg at the miter
/// limit somewhat more. A gap far beyond that means pruning removed a whole
/// run of pieces, and connecting across it would draw a confident straight
/// line through geometry that was never part of the boundary. Ending the
/// loop there yields a visibly missing piece instead of a wrong one.
const MAX_BRIDGE: f64 = 8.0;

fn stitch(kept: &[Option<(usize, PathSeg)>], accuracy: f64, width: f64) -> Vec<BezPath> {
    let n = kept.len();
    let eps = (accuracy * 16.0).max(1e-9);
    let max_bridge = MAX_BRIDGE * width;
    let mut used = alloc::vec![false; n];
    let mut loops = Vec::new();

    // Both lookups below were linear scans over every piece, which made the
    // walk quadratic in surviving pieces: a detailed contour stroked wider
    // than its own feature spacing cuts into tens of thousands of them, and
    // that dominated everything else in the pipeline.
    //
    // Continuations are found through a grid of start points keyed at the
    // match radius, so a query touches only the 3x3 cells that can hold a
    // match. Bridges use a per-contour set of still-unused pieces, ordered,
    // so "the next survivor of this contour after `cur`" is a range query.
    // Both reproduce the old scans exactly, including their preference for
    // the lowest index among equally valid candidates.
    let cell = eps;
    let key = |p: Point| -> (i64, i64) {
        (
            math::floor(p.x / cell) as i64,
            math::floor(p.y / cell) as i64,
        )
    };
    let mut starts: BTreeMap<(i64, i64), Vec<u32>> = BTreeMap::new();
    let n_owners = kept.iter().flatten().map(|(o, _)| o + 1).max().unwrap_or(0);
    let mut free: Vec<BTreeSet<usize>> = alloc::vec![BTreeSet::new(); n_owners];
    for (i, k) in kept.iter().enumerate() {
        if let Some((owner, seg)) = k {
            starts.entry(key(seg.start())).or_default().push(i as u32);
            free[*owner].insert(i);
        }
    }

    let mut scan = 0usize;
    loop {
        // `used` only ever goes from false to true, so the search for the
        // next unwalked piece never has to look back.
        while scan < n && (kept[scan].is_none() || used[scan]) {
            scan += 1;
        }
        if scan >= n {
            break;
        }
        let start = scan;
        let first_pt = kept[start].unwrap().1.start();
        let mut path = BezPath::new();
        path.move_to(first_pt);
        let mut cur = start;
        loop {
            used[cur] = true;
            let (owner, seg) = kept[cur].unwrap();
            free[owner].remove(&cur);
            split::append_seg(&mut path, seg);
            let end = seg.end();
            if (end - first_pt).hypot() <= eps {
                break;
            }
            // A survivor continuing from this exact point (any contour).
            let (kx, ky) = key(end);
            let mut next: Option<usize> = None;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    // Saturating: a coordinate far enough out of range that
                    // its cell index pins to the end of `i64` would
                    // otherwise overflow while stepping to the neighbour.
                    let Some(bucket) = starts.get(&(kx.saturating_add(dx), ky.saturating_add(dy)))
                    else {
                        continue;
                    };
                    for &k in bucket {
                        let k = k as usize;
                        if k == cur || used[k] || next.is_some_and(|b| k > b) {
                            continue;
                        }
                        if kept[k].is_some_and(|(_, s)| (s.start() - end).hypot() <= eps) {
                            next = Some(k);
                        }
                    }
                }
            }
            if next.is_none() {
                // Otherwise the next survivor of the same contour, bridged.
                next = free[owner]
                    .range(cur + 1..)
                    .next()
                    .or_else(|| free[owner].iter().next())
                    .copied();
            }
            let Some(k) = next else { break };
            let s = kept[k].unwrap().1.start();
            let gap = (s - end).hypot();
            if gap > eps {
                if gap > max_bridge {
                    // Too far to be dropped join geometry; leave the hole.
                    break;
                }
                path.line_to(s);
            }
            cur = k;
        }
        path.close_path();
        // Drop slivers: at exactly the saturation width the erosion collapses
        // to a near-zero-area curve, which paints nothing but costs segments.
        if substantial_loop(&path, accuracy) {
            loops.push(path);
        }
    }
    loops
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::{Cap, Join, ParamCurveNearest, Rect};

    fn base_params(tolerance: f64) -> BandParams<'static> {
        BandParams {
            d_left: 0.0,
            d_right: 0.0,
            join: Join::Miter,
            miter_limit: 4.0,
            start_cap: Cap::Butt,
            end_cap: Cap::Butt,
            tolerance,
            join_thresh: 0.0,
            crossings: &[],
            crossing_eps: 0.0,
            raw_offset: false,
        }
    }

    fn loops_for(path: &BezPath, fill_side: StrokeSide, kind: RegionKind, w: f64) -> Vec<BezPath> {
        let index = SourceIndex::new(path, 1e-3);
        let (mut l, mut r, mut s, mut sink) = (
            BezPath::new(),
            BezPath::new(),
            BezPath::new(),
            BezPath::new(),
        );
        let specs = [ContourSpec {
            els: path.elements(),
            fill_side,
        }];
        region_loops(
            &specs,
            kind,
            &index,
            w,
            &base_params(1e-3),
            1e-3,
            &mut l,
            &mut r,
            &mut s,
            &mut sink,
        )
    }

    /// The index must agree with kurbo's exact winding away from the outline
    /// (points within the flattening error may classify either way).
    #[test]
    fn index_winding_matches_kurbo() {
        let mut donut = kurbo::Circle::new((130.0, 130.0), 100.0).to_path(1e-7);
        donut.extend(
            kurbo::Circle::new((130.0, 130.0), 45.0)
                .to_path(1e-7)
                .reverse_subpaths()
                .iter(),
        );
        let index = SourceIndex::new(&donut, 0.012);
        let mut bad = 0;
        for iy in 0..40 {
            for ix in 0..40 {
                let p = Point::new(ix as f64 * 7.0, iy as f64 * 7.0);
                if (donut.winding(p) != 0) == (index.winding(p) != 0) {
                    continue;
                }
                let d = donut
                    .segments()
                    .map(|s| s.nearest(p, 1e-9).distance_sq)
                    .fold(f64::INFINITY, f64::min)
                    .sqrt();
                if d > 0.05 {
                    bad += 1;
                }
            }
        }
        assert_eq!(bad, 0, "{bad} winding mismatches away from the outline");
    }

    /// A rect wider than 2w keeps its erosion; narrower than 2w saturates.
    #[test]
    fn rect_erosion_then_saturation() {
        let rect = Rect::new(0.0, 0.0, 80.0, 60.0).to_path(1e-9);
        let loops = loops_for(&rect, StrokeSide::Right, RegionKind::Erosion, 10.0);
        assert_eq!(loops.len(), 1, "erosion is a single rect");
        let area = loops[0].elements().area().abs();
        assert!(
            (area - 60.0 * 40.0).abs() < 1.0,
            "eroded rect should be 60x40, area {area}"
        );

        // 2w >= 60: the erosion is empty and the stroke saturates.
        assert!(
            loops_for(&rect, StrokeSide::Right, RegionKind::Erosion, 35.0).is_empty(),
            "over-wide inside band must saturate"
        );
        assert!(
            loops_for(&rect, StrokeSide::Right, RegionKind::Erosion, 30.0).is_empty(),
            "exactly-half-width band must saturate"
        );
    }

    /// A wedge's erosion vanishes long before its bounding box does.
    #[test]
    fn thin_wedge_saturates() {
        let mut wedge = BezPath::new();
        wedge.move_to((20.0, 180.0));
        wedge.line_to((230.0, 160.0));
        wedge.line_to((20.0, 140.0));
        wedge.close_path();
        assert!(loops_for(&wedge, StrokeSide::Left, RegionKind::Erosion, 60.0).is_empty());
    }

    /// The dilation always exists and encloses the source.
    #[test]
    fn dilation_encloses_source() {
        let rect = Rect::new(0.0, 0.0, 80.0, 60.0).to_path(1e-9);
        let loops = loops_for(&rect, StrokeSide::Right, RegionKind::Dilation, 25.0);
        assert_eq!(loops.len(), 1);
        let bb = loops[0].bounding_box();
        assert!(bb.x0 <= -24.0 && bb.y0 <= -24.0 && bb.x1 >= 104.0 && bb.y1 >= 84.0);
    }

    /// Smooth offsets have no intersections: the fast path must return the
    /// analytically correct erosion, and the collapse at w = r must yield
    /// the empty region (saturation), not a shredded point cluster.
    #[test]
    fn circle_erosion_fast_path_and_collapse() {
        let c = kurbo::Circle::new((0.0, 0.0), 50.0).to_path(1e-7);
        let loops = loops_for(&c, StrokeSide::Right, RegionKind::Erosion, 10.0);
        assert_eq!(loops.len(), 1, "circle erosion is a single loop");
        let area = loops[0].elements().area().abs();
        let expected = core::f64::consts::PI * 40.0 * 40.0;
        assert!(
            (area - expected).abs() < expected * 0.01,
            "eroded circle area {area:.1} vs analytic {expected:.1}"
        );
        assert!(
            loops_for(&c, StrokeSide::Right, RegionKind::Erosion, 50.0).is_empty(),
            "collapsed offset at w = r must saturate"
        );
    }
}
