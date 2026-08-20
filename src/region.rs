// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Distance-pruned offset regions for one-sided bands on closed contours.
//!
//! The band of a closed contour is defined *set-theoretically*, matching
//! Figma (whose inside/outside strokes are a doubled centered stroke masked
//! by the fill):
//!
//! ```text
//! Inside(w)  = { p ∈ F : dist(p, path) ≤ w } = F \ E
//! Outside(w) = { p ∉ F : dist(p, path) ≤ w } = D \ F
//! ```
//!
//! with `F` the filled region of the whole path (nonzero), `E` its erosion by
//! `w` and `D` its dilation. So the outline is the source contours plus the
//! boundary of the eroded (or dilated) region, wound the other way — and the
//! whole problem reduces to computing `∂E` / `∂D`.
//!
//! Those are *not* the naive offset curves: past the local thickness the
//! offset self-intersects and inverts. They are computed the classical way —
//! raw offsets for every contour, cut at every intersection (with themselves
//! **and with each other**), then discard each piece that is nearer than `w`
//! to the path, on the wrong side of the fill, or inside another loop's
//! region (a winding test against the raw loops themselves — required for
//! miter/bevel joins, whose loops deviate from the distance-`w` set at
//! corners), and stitch what survives. Cutting across contours matters: it
//! is what makes overlapping bands merge into one boundary instead of two
//! overlapping ones, which nonzero winding could not express.
//!
//! When nothing survives, the erosion is empty and the stroke **saturates**:
//! the shape is filled completely (a wedge, a thin star, a rectangle narrower
//! than `2w`) — exactly as Figma renders it. Dropped pieces whose neighbours
//! survive are rejoined with a straight line, which reproduces bevel-join
//! chords exactly.

use alloc::vec::Vec;

use kurbo::{BezPath, ParamCurve, PathEl, PathSeg, Point, Rect, Shape};

use crate::expand::{Band, BandParams};
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

/// Source geometry prepared for the two hot predicates: "is this point inside
/// the region?" and "is it far enough from the path?".
///
/// Both would otherwise be a per-curve solve — [`kurbo::Shape::winding`],
/// [`kurbo::ParamCurveNearest::nearest`] — evaluated once per candidate
/// piece, which dominates the pipeline. The path is flattened once instead
/// and its edges bucketed by `y`, so a query touches only the edges that can
/// matter. Flattening error is folded into the distance threshold.
pub(crate) struct SourceIndex {
    /// `(a, b, real, contour)`; `real == false` marks an implicit closing
    /// edge, which counts for winding but is not part of the path for
    /// distance. `contour` is the subpath ordinal, so the distance test can
    /// be restricted to a piece's own contour (cross-contour validity is a
    /// winding question, not a distance one — see `region_loops`).
    edges: Vec<(Point, Point, bool, u32)>,
    buckets: Vec<Vec<u32>>,
    y0: f64,
    inv_h: f64,
    flat_tol: f64,
}

impl SourceIndex {
    pub(crate) fn new(path: &BezPath, flat_tol: f64) -> Self {
        let flat_tol = flat_tol.max(1e-9);
        let mut edges: Vec<(Point, Point, bool, u32)> = Vec::new();
        let mut last: Option<Point> = None;
        let mut start: Option<Point> = None;
        let mut sub: u32 = 0;
        let mut seen_move = false;
        let close = |edges: &mut Vec<(Point, Point, bool, u32)>,
                     last: Option<Point>,
                     s: Option<Point>,
                     sub: u32| {
            if let (Some(prev), Some(s)) = (last, s) {
                if (s - prev).hypot2() > 0.0 {
                    edges.push((prev, s, false, sub));
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
                        edges.push((prev, p, true, sub));
                    }
                }
                last = Some(p);
            }
            PathEl::ClosePath => {
                if let (Some(prev), Some(s)) = (last, start) {
                    if (s - prev).hypot2() > 0.0 {
                        edges.push((prev, s, true, sub));
                    }
                }
                last = start;
            }
            _ => {}
        });
        close(&mut edges, last, start, sub);

        let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
        for (a, b, _, _) in &edges {
            ymin = ymin.min(a.y).min(b.y);
            ymax = ymax.max(a.y).max(b.y);
        }
        let n_buckets = (edges.len() / 4).clamp(1, 256);
        let mut buckets: Vec<Vec<u32>> = alloc::vec![Vec::new(); n_buckets];
        if edges.is_empty() {
            return SourceIndex {
                edges,
                buckets,
                y0: 0.0,
                inv_h: 0.0,
                flat_tol,
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
        SourceIndex {
            edges,
            buckets,
            y0: ymin,
            inv_h,
            flat_tol,
        }
    }

    /// The single bucket containing `y`, or `None` when there is no index.
    fn bucket_of(&self, y: f64) -> Option<&Vec<u32>> {
        let n = self.buckets.len();
        if n == 0 {
            return None;
        }
        let ix = crate::math::floor((y - self.y0) * self.inv_h).clamp(0.0, (n - 1) as f64) as usize;
        Some(&self.buckets[ix])
    }

    fn bucket_range(&self, lo_y: f64, hi_y: f64) -> (usize, usize) {
        let n = self.buckets.len();
        if n == 0 {
            return (0, 0);
        }
        let lo =
            crate::math::floor((lo_y - self.y0) * self.inv_h).clamp(0.0, (n - 1) as f64) as usize;
        let hi =
            crate::math::ceil((hi_y - self.y0) * self.inv_h).clamp(0.0, (n - 1) as f64) as usize;
        (lo, hi)
    }

    /// Whether `p` is at least `sqrt(thresh_sq)` away from contour
    /// `contour` of the path (`None`: away from the whole path).
    ///
    /// Phrased as a predicate rather than a distance: a segment whose box is
    /// already beyond the threshold cannot disqualify the point, and the
    /// first violation ends the scan.
    pub(crate) fn is_clear_of(&self, p: Point, thresh_sq: f64, contour: Option<usize>) -> bool {
        if thresh_sq <= 0.0 {
            return true;
        }
        let r = crate::math::sqrt(thresh_sq);
        let (lo, hi) = self.bucket_range(p.y - r, p.y + r);
        for bi in lo..=hi {
            for &ix in &self.buckets[bi] {
                let (a, b, real, sub) = self.edges[ix as usize];
                if !real || contour.is_some_and(|c| c as u32 != sub) {
                    continue;
                }
                let (x0, x1) = if a.x <= b.x { (a.x, b.x) } else { (b.x, a.x) };
                let dx = (x0 - p.x).max(p.x - x1).max(0.0);
                if dx * dx >= thresh_sq {
                    continue;
                }
                let ab = b - a;
                let len2 = ab.hypot2();
                let t = if len2 > 0.0 {
                    ((p - a).dot(ab) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                if ((p - a) - t * ab).hypot2() < thresh_sq {
                    return false;
                }
            }
        }
        true
    }

    /// Nonzero winding number of the flattened path at `p`.
    ///
    /// Exactly one bucket is scanned: an edge crossing `y = p.y` always
    /// spans that bucket, and visiting a neighbour would double-count edges
    /// registered in both — which breaks the cancellation that makes the
    /// winding zero outside the shape.
    pub(crate) fn winding(&self, p: Point) -> i32 {
        let Some(bucket) = self.bucket_of(p.y) else {
            return 0;
        };
        let mut w = 0;
        for &ix in bucket {
            let (a, b, _, _) = self.edges[ix as usize];
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

    /// Flattening tolerance, subtracted from distance thresholds.
    fn slack(&self) -> f64 {
        self.flat_tol
    }
}

/// Flattened raw offset loops with per-edge owner tags.
///
/// Backs the cross-contour membership test: a piece of loop `i` belongs to
/// the region boundary only where the *other* loops' winding matches the
/// expected value. Distance to the source path cannot express this for
/// miter/bevel joins, whose loops deviate from the distance-`w` set at
/// corners (a miter wedge reaches past `w`, and an arc of another contour
/// hiding inside that wedge must still be pruned).
struct RawIndex {
    /// `(a, b, owner)` flattened edges.
    edges: Vec<(Point, Point, u32)>,
    buckets: Vec<Vec<u32>>,
    y0: f64,
    inv_h: f64,
}

impl RawIndex {
    fn new(raws: &[BezPath], flat_tol: f64) -> RawIndex {
        let mut edges: Vec<(Point, Point, u32)> = Vec::new();
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
                            edges.push((prev, p, ix as u32));
                        }
                    }
                    last = Some(p);
                }
                PathEl::ClosePath => {
                    if let (Some(prev), Some(s)) = (last, start) {
                        if (s - prev).hypot2() > 0.0 {
                            edges.push((prev, s, ix as u32));
                        }
                    }
                    last = start;
                }
                _ => {}
            });
            // The raw loops are explicitly closed, but guard anyway.
            if let (Some(prev), Some(s)) = (last, start) {
                if (s - prev).hypot2() > 0.0 {
                    edges.push((prev, s, ix as u32));
                }
            }
        }

        let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
        for (a, b, _) in &edges {
            ymin = ymin.min(a.y).min(b.y);
            ymax = ymax.max(a.y).max(b.y);
        }
        let n_buckets = (edges.len() / 4).clamp(1, 256);
        let mut buckets: Vec<Vec<u32>> = alloc::vec![Vec::new(); n_buckets];
        if edges.is_empty() {
            return RawIndex {
                edges,
                buckets,
                y0: 0.0,
                inv_h: 0.0,
            };
        }
        let inv_h = n_buckets as f64 / (ymax - ymin).max(1e-12);
        for (i, (a, b, _)) in edges.iter().enumerate() {
            let lo = (((a.y.min(b.y) - ymin) * inv_h) as usize).min(n_buckets - 1);
            let hi = (((a.y.max(b.y) - ymin) * inv_h) as usize).min(n_buckets - 1);
            for bucket in &mut buckets[lo..=hi] {
                bucket.push(i as u32);
            }
        }
        RawIndex {
            edges,
            buckets,
            y0: ymin,
            inv_h,
        }
    }

    /// Nonzero winding at `p` of every loop except `owner`'s.
    ///
    /// Single-bucket scan, same reasoning as [`SourceIndex::winding`].
    fn winding_excluding(&self, p: Point, owner: usize) -> i32 {
        let n = self.buckets.len();
        if n == 0 {
            return 0;
        }
        let ix =
            crate::math::floor((p.y - self.y0) * self.inv_h).clamp(0.0, (n - 1) as f64) as usize;
        let mut w = 0;
        for &ei in &self.buckets[ix] {
            let (a, b, o) = self.edges[ei as usize];
            if o as usize == owner {
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

    /// Whether `p` lies within `sqrt(r_sq)` of any loop except `owner`'s.
    ///
    /// A piece that close to another loop cannot be classified by winding —
    /// the loops locally coincide (a donut at exactly half its ring
    /// thickness) — and is pruned instead, collapsing sub-resolution
    /// regions to clean saturation.
    fn is_near_other(&self, p: Point, r_sq: f64, owner: usize) -> bool {
        let n = self.buckets.len();
        if n == 0 || r_sq <= 0.0 {
            return false;
        }
        let r = crate::math::sqrt(r_sq);
        let lo = crate::math::floor((p.y - r - self.y0) * self.inv_h).clamp(0.0, (n - 1) as f64)
            as usize;
        let hi =
            crate::math::ceil((p.y + r - self.y0) * self.inv_h).clamp(0.0, (n - 1) as f64) as usize;
        for bi in lo..=hi {
            for &ei in &self.buckets[bi] {
                let (a, b, o) = self.edges[ei as usize];
                if o as usize == owner {
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
    // Resolution of the pruning predicate: offset approximation, tolerance
    // and flattening error stack up to about this length. Features smaller
    // than it fall inside the documented boundary band.
    let fuzz = PRUNE_SLACK * width + 2.0 * base.tolerance + index.slack();

    // --- raw offsets, one per contour ---
    let mut raws: Vec<BezPath> = Vec::with_capacity(contours.len());
    // Per-raw, per-segment relaxation of the distance prune: `1` for plain
    // offset geometry, `cos(φ/2)` where a bevel chord legitimately cuts its
    // corner (see `keep_at`).
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
        // Collapsed offset (width at the local thickness): the loop
        // degenerates to a tolerance-scale cluster, which `offset_cubic`'s
        // cusp handling can shatter into thousands of segments — and the
        // pairwise cut of that cluster is quadratic in them. A loop smaller
        // than the pruning fuzz cannot bound a resolvable region feature;
        // drop it before it reaches the cutter.
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

    let thresh = (width * (1.0 - PRUNE_SLACK) - 2.0 * base.tolerance - index.slack()).max(0.0);
    let want_filled = kind == RegionKind::Erosion;
    // Cross-contour membership needs the *other* raw loops' winding: with
    // miter/bevel joins a loop deviates from the distance-`w` set at
    // corners, so distance alone cannot tell whether a piece hides inside
    // another contour's region. Expected winding follows the owning loop's
    // orientation: 0 for outer-like (CW) loops, +1 for hole-like (CCW)
    // loops, whose enclosing outer loop contributes one turn. Only needed
    // when two or more loops are live.
    // Loop orientation, taken from the *source* contour (the loop inherits
    // its travel direction; the raw loop's own net area is unreliable when
    // the offset self-intersects, e.g. the overshoot star of a sharp
    // triangle's inward offset).
    let cw: Vec<bool> = contours.iter().map(|c| c.els.area() >= 0.0).collect();
    let live = raws.iter().filter(|r| r.elements().len() >= 2).count();
    let raw_index = (live >= 2).then(|| RawIndex::new(&raws, index.slack()));
    // The join relaxation applies to the dilation only. There, cutting a
    // corner is the style the caller asked for and the strict test tears the
    // union apart; the dilation never folds inward, so nothing needs the
    // slack as a guard. For the erosion the distance test *is* the
    // saturation guard — a spurious survivor becomes a hole in a shape that
    // should be solid — and dropping a bevel chord costs nothing, because
    // `stitch` bridges the gap with the very same chord.
    let relax_joins = kind == RegionKind::Dilation;
    let keep_at = |p: Point, owner: usize, relax: f64| {
        // Self-validity: far enough from the piece's *own* contour (prunes
        // fold-over past the local thickness). Only the own contour —
        // proximity to another contour says nothing under miter/bevel
        // joins, whose loops deviate from the distance-`w` set at corners;
        // cross-contour validity is the winding test below. A bevel chord
        // legitimately cuts its corner, so its own threshold is scaled by
        // `cos(φ/2)` — bounded, unlike a blanket exemption, which at
        // extreme widths let huge join fans survive inside the shape.
        let thr = if relax_joins { thresh * relax } else { thresh };
        if !index.is_clear_of(p, thr * thr, Some(owner)) {
            return false;
        }
        // The right side of the fill.
        if (index.winding(p) != 0) != want_filled {
            return false;
        }
        // Not swallowed by (or escaped from) the other loops' region, and
        // not so close to another loop that the winding answer is fuzz.
        match &raw_index {
            None => true,
            Some(ri) => {
                let expected = if cw[owner] { 0 } else { 1 };
                ri.winding_excluding(p, owner) == expected
                    && !ri.is_near_other(p, fuzz * fuzz, owner)
            }
        }
    };

    // --- fast path: nothing cuts ---
    // Validity flips only across an intersection (segment joints are piece
    // boundaries already), so with no interior cuts every loop is uniformly
    // valid or invalid: classify each by a few midpoint samples and skip
    // subdivision, per-piece pruning and stitching. Disagreeing samples are
    // possible only within the boundary band; fall back to the full
    // pipeline for those.
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
        .map(|(owner, seg, is_join)| keep_at(seg.eval(0.5), owner, is_join).then_some((owner, seg)))
        .collect();

    drop_unresolvable(stitch(&kept, accuracy), fuzz)
}

/// Empty out a region that is thinner than the pipeline's resolution.
///
/// The loops describe the region by nonzero winding with consistent
/// orientations, so the net signed area *is* the region's area, and
/// `net / perimeter` bounds its mean thickness. A region thinner than the
/// fuzz everywhere sits inside the documented boundary band — most
/// importantly the cancel pair stitched from two nearly coincident offsets
/// (a donut at exactly half its ring thickness), which otherwise ships
/// hundreds of fuzz segments that paint nothing.
fn drop_unresolvable(loops: Vec<BezPath>, fuzz: f64) -> Vec<BezPath> {
    let net: f64 = loops.iter().map(|l| l.elements().area()).sum();
    let mut perimeter = 0.0;
    for l in &loops {
        for seg in l.segments() {
            perimeter += (seg.end() - seg.start()).hypot();
        }
    }
    if net.abs() <= 0.5 * fuzz * perimeter.max(1.0) {
        Vec::new()
    } else {
        loops
    }
}

/// The segments of a family of closed offset curves, with every mutual and
/// self intersection collected as cut parameters.
struct CutSegs {
    segs: Vec<PathSeg>,
    owner: Vec<usize>,
    /// Distance-prune relaxation per segment (see `region_loops`).
    relax: Vec<f64>,
    cuts: Vec<Vec<f64>>,
    /// Whether any cut lands in a segment's interior. Endpoint touches are
    /// dropped by `subdivide_at` anyway, so without interior cuts the
    /// subdivision is a no-op and the fast path applies.
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
                let (a, b) = (bboxes[i], bboxes[j]);
                if a.x0 > b.x1 + accuracy
                    || b.x0 > a.x1 + accuracy
                    || a.y0 > b.y1 + accuracy
                    || b.y0 > a.y1 + accuracy
                {
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

/// Whether a closed loop bounds meaningful area: its |signed area| must
/// exceed a perimeter-proportional sliver threshold. This catches both tiny
/// loops (the erosion collapsing at the saturation width) and long
/// near-degenerate rings (the fuzz stitched from two nearly coincident
/// offsets, e.g. a donut whose inward offsets meet in the middle).
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
/// At an intersection the surviving strand continues from the *same point*,
/// so the walk hands over to whichever unused piece starts there — including
/// a piece belonging to another contour, which is how overlapping bands merge
/// into a single boundary. Elsewhere (a dropped bevel chord, a trimmed spike)
/// it falls through to the next survivor of the same contour and bridges with
/// a line; for a bevel join that line is the chord itself.
fn stitch(kept: &[Option<(usize, PathSeg)>], accuracy: f64) -> Vec<BezPath> {
    let n = kept.len();
    let eps = (accuracy * 16.0).max(1e-9);
    let mut used = alloc::vec![false; n];
    let mut loops = Vec::new();

    while let Some(start) = (0..n).find(|&i| kept[i].is_some() && !used[i]) {
        let first_pt = kept[start].unwrap().1.start();
        let mut path = BezPath::new();
        path.move_to(first_pt);
        let mut cur = start;
        loop {
            used[cur] = true;
            let (owner, seg) = kept[cur].unwrap();
            append_seg(&mut path, seg);
            let end = seg.end();
            if (end - first_pt).hypot() <= eps {
                break;
            }
            // A survivor continuing from this exact point (any contour).
            let mut next = (0..n).find(|&k| {
                k != cur
                    && !used[k]
                    && kept[k].is_some_and(|(_, s)| (s.start() - end).hypot() <= eps)
            });
            if next.is_none() {
                // Otherwise the next survivor of the same contour, bridged.
                next = (1..=n)
                    .map(|d| (cur + d) % n)
                    .find(|&k| !used[k] && kept[k].is_some_and(|(o, _)| o == owner));
            }
            let Some(k) = next else { break };
            let s = kept[k].unwrap().1.start();
            if (s - end).hypot() > eps {
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

fn append_seg(path: &mut BezPath, seg: PathSeg) {
    match seg {
        PathSeg::Line(l) => path.line_to(l.p1),
        PathSeg::Quad(q) => path.quad_to(q.p1, q.p2),
        PathSeg::Cubic(c) => path.curve_to(c.p1, c.p2, c.p3),
    }
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
