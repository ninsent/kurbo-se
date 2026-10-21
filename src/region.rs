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
//! Take the raw offset of every contour and cut it at every intersection
//! with itself and with the others. Then discard each piece that lies
//! inside the band of width `w`, on the wrong side of the fill, inside
//! another loop's region, or on the band side of a strand it crosses, and
//! stitch what survives.
//!
//! The distance test uses the real shape of the joins, so a bevel or miter
//! corner is not mistaken for a round one; see `Corner`. The region test
//! is a winding test against the raw loops themselves, which miter and
//! bevel joins require: their loops deviate from the distance-`w` set at
//! corners. The crossing test catches strands that cross too shallowly for
//! the distance test's slack, as near the collapse of an erosion. Cutting
//! across contours is what makes overlapping bands merge into one boundary
//! instead of two overlapping ones, which nonzero winding could not
//! express.
//!
//! When nothing survives, the erosion is empty and the stroke saturates:
//! the shape fills completely, as Figma renders a wedge, a thin star, or a
//! rectangle narrower than `2w`. Dropped pieces whose neighbours survive
//! are rejoined with a straight line.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use kurbo::{BezPath, Join, ParamCurve, PathEl, PathSeg, Point, Rect, Shape, Vec2};

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
    /// Per edge, which of its ends are corners of the source contour and
    /// on which side each corner's join lies, as bits like [`FILL_START`].
    /// Empty means no edge has a corner.
    ends: Vec<u8>,
    buckets: Vec<Vec<u32>>,
    y0: f64,
    inv_h: f64,
}

/// The edge's start point is a corner whose join lies on the fill side.
const FILL_START: u8 = 1;
/// The edge's end point is a corner whose join lies on the fill side.
const FILL_END: u8 = 2;
/// The edge's start point is a corner whose join lies away from the fill.
const OUT_START: u8 = 4;
/// The edge's end point is a corner whose join lies away from the fill.
const OUT_END: u8 = 8;

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
                ends: Vec::new(),
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
            ends: Vec::new(),
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
    ///
    /// An edge does not count where `p` lies past an end flagged in `skip`.
    /// The nearest point is then a corner, and around a corner the band
    /// covers its join, which [`SourceIndex::is_clear_of_joined`] tests.
    fn within_where(
        &self,
        p: Point,
        r_sq: f64,
        keep: impl Fn(u32, bool) -> bool,
        skip: u8,
    ) -> bool {
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
                    (p - a).dot(ab) / len2
                } else {
                    0.0
                };
                if skip != 0 && !(0.0..=1.0).contains(&t) {
                    let ends = self.ends.get(ix as usize).copied().unwrap_or(0);
                    let past = if t < 0.0 {
                        FILL_START | OUT_START
                    } else {
                        FILL_END | OUT_END
                    };
                    if ends & skip & past != 0 {
                        continue;
                    }
                }
                let t = t.clamp(0.0, 1.0);
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
    /// The contours' corners, where a band emits a join, and their ordinals
    /// bucketed by `y` like the edges. Empty unless built `with_joins`.
    corners: Vec<Corner>,
    corner_buckets: Vec<Vec<u32>>,
    /// The farthest any join reaches from its corner, at the widest width
    /// asked about. A query searches its bucket within this distance in `x`.
    corner_reach: f64,
}

/// A corner of a source contour: a joint where the outline turns by more
/// than the join-omission threshold, so a band emits a join there.
///
/// A band covers the points within its width of an edge, measured square
/// to the edge, plus its joins. At a corner the two edges' strips leave a
/// wedge between their normals on the outer side of the turn. The join
/// fills that wedge: a disc around the corner for a round join, a triangle
/// for a bevel, a kite for a miter. Plain distance treats every corner as
/// round. A bevel chord then looks too close to its corner and is pruned,
/// although it is the true boundary of the region. Where the boundary is
/// mostly chords, as across a star's arms or between a gear's teeth, the
/// region would tear apart.
///
/// A one-sided band emits a join only where its side is the outer side of
/// the turn, so a query counts only the joins on the side being built. A
/// corner on the other side keeps the plain distance, a disc. A miter's
/// kite there reaches up to the miter limit times the width, and counting
/// it would prune real boundary across a thin wall that no raw offset
/// replaces.
#[derive(Copy, Clone)]
struct Corner {
    v: Point,
    /// Unit normals of the incoming and outgoing segments, on the outer
    /// side of the turn. The join spans the wedge between them.
    a: Vec2,
    b: Vec2,
    /// Where a miter applies, its tip at unit width, `(a + b) / (1 + a·b)`.
    miter: Option<Vec2>,
    /// How far from `v` the join reaches at unit width.
    reach: f64,
    round: bool,
    /// Whether the join lies on the fill side, which is the right of travel
    /// once the contours are normalised and the side an erosion offsets to.
    into_fill: bool,
    tag: u32,
}

impl Corner {
    /// The corner at a joint where the incoming segment ends with tangent
    /// `t_in` and the outgoing one starts with `t_out`, if a band emits a
    /// join there. Mirrors `Band::do_join`.
    fn at(
        v: Point,
        t_in: Vec2,
        t_out: Vec2,
        join: Join,
        miter_limit: f64,
        join_thresh: f64,
        tag: u32,
    ) -> Option<Corner> {
        let cross = t_in.cross(t_out);
        let dot = t_in.dot(t_out);
        let hypot = math::hypot(cross, dot);
        if hypot == 0.0 || (dot > 0.0 && cross.abs() < hypot * join_thresh) {
            return None;
        }
        let (h_in, h_out) = (t_in.hypot(), t_out.hypot());
        // Right normals. In Y-down a positive `cross` is a right turn, and
        // its outer side is the left.
        let n_in = t_in.turn_90() * (1.0 / h_in);
        let n_out = t_out.turn_90() * (1.0 / h_out);
        let (a, b) = if cross > 0.0 {
            (-n_in, -n_out)
        } else {
            (n_in, n_out)
        };
        let miter_ok = 2.0 * hypot < (hypot + dot) * miter_limit * miter_limit;
        let miter = (join == Join::Miter && miter_ok).then(|| miter_tip(a, b));
        Some(Corner {
            v,
            a,
            b,
            miter,
            reach: miter.map_or(1.0, |m| m.hypot().max(1.0)),
            round: join == Join::Round,
            into_fill: cross <= 0.0,
            tag,
        })
    }

    /// Take the join's normals from the flattened edges that meet here, not
    /// from the exact tangents. Where a curve's first chord leans away from
    /// its tangent, the exact normals would leave a sliver between the join
    /// and the edge's strip.
    fn reshape(&mut self, d_in: Vec2, d_out: Vec2) {
        let (h_in, h_out) = (d_in.hypot(), d_out.hypot());
        if h_in == 0.0 || h_out == 0.0 {
            return;
        }
        let sign = if self.into_fill { 1.0 } else { -1.0 };
        self.a = d_in.turn_90() * (sign / h_in);
        self.b = d_out.turn_90() * (sign / h_out);
        if self.miter.is_some() {
            let m = miter_tip(self.a, self.b);
            self.miter = Some(m);
            self.reach = m.hypot().max(1.0);
        }
    }

    /// Whether `p` lies strictly inside this corner's join at width `r`.
    fn covers(&self, p: Point, r: f64) -> bool {
        let d = p - self.v;
        if d.hypot2() >= (r * self.reach) * (r * self.reach) {
            return false;
        }
        let q = d * (1.0 / r);
        if self.round {
            return q.hypot2() < 1.0;
        }
        match self.miter {
            Some(m) => inside_convex(q, &[Vec2::ZERO, self.a, m, self.b]),
            None => inside_convex(q, &[Vec2::ZERO, self.a, self.b]),
        }
    }
}

/// The miter tip between unit normals `a` and `b`, at unit width.
fn miter_tip(a: Vec2, b: Vec2) -> Vec2 {
    (a + b) * (1.0 / (1.0 + a.dot(b)))
}

/// Whether `q` lies strictly inside the convex polygon `poly`, in either
/// orientation. A degenerate polygon contains nothing.
fn inside_convex(q: Vec2, poly: &[Vec2]) -> bool {
    let mut sign = 0.0;
    for (i, &p0) in poly.iter().enumerate() {
        let p1 = poly[(i + 1) % poly.len()];
        let c = (p1 - p0).cross(q - p0);
        if c == 0.0 || (sign != 0.0 && (c > 0.0) != (sign > 0.0)) {
            return false;
        }
        sign = c;
    }
    true
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
            corners: Vec::new(),
            corner_buckets: Vec::new(),
            corner_reach: 0.0,
        }
    }

    /// An index over closed `contours`, tagged by ordinal, that also knows
    /// where a band with `join` emits joins; see [`Corner`]. `reach` is the
    /// widest width [`SourceIndex::is_clear_of_joined`] will be asked about.
    pub(crate) fn with_joins(
        contours: &[BezPath],
        flat_tol: f64,
        join: Join,
        miter_limit: f64,
        join_thresh: f64,
        reach: f64,
    ) -> Self {
        let extent = contours
            .iter()
            .map(|c| c.control_box().size().max_side())
            .fold(0.0, f64::max);
        let flat_tol = flat_tol.max(extent * MIN_RELATIVE_FLAT_TOL).max(1e-9);
        let mut edges: Vec<(Point, Point, u32, bool)> = Vec::new();
        let mut ends: Vec<u8> = Vec::new();
        let mut corners: Vec<Corner> = Vec::new();
        for (tag, contour) in contours.iter().enumerate() {
            let tag = tag as u32;
            let segs: Vec<PathSeg> = contour
                .segments()
                .filter(|s| !math::is_degenerate(s))
                .collect();
            let n = segs.len();
            if n == 0 {
                continue;
            }
            // The corner, if any, at the joint where `segs[j]` starts.
            let first = corners.len();
            let mut corner_at: Vec<Option<usize>> = alloc::vec![None; n];
            for j in 0..n {
                let (_, t_in) = crate::expand::tangents(&segs[(j + n - 1) % n]);
                let (t_out, _) = crate::expand::tangents(&segs[j]);
                let v = segs[j].start();
                if let Some(c) = Corner::at(v, t_in, t_out, join, miter_limit, join_thresh, tag) {
                    corner_at[j] = Some(corners.len());
                    corners.push(c);
                }
            }
            let start = segs[0].start();
            if corners.len() == first {
                // No corners: plain edges, no bookkeeping.
                let mut last = start;
                kurbo::flatten(contour.iter(), flat_tol, |el| {
                    let p = match el {
                        PathEl::LineTo(p) => p,
                        PathEl::ClosePath => start,
                        _ => return,
                    };
                    if (p - last).hypot2() > 0.0 {
                        edges.push((last, p, tag, true));
                        ends.push(0);
                        last = p;
                    }
                });
                continue;
            }
            // The edges that meet at each of this contour's corners.
            let mut meeting = alloc::vec![(usize::MAX, usize::MAX); corners.len() - first];
            {
                let bits = |c: usize| {
                    if corners[c].into_fill {
                        (FILL_START, FILL_END)
                    } else {
                        (OUT_START, OUT_END)
                    }
                };
                // Flatten the whole contour in one call, since `flatten`
                // sets up its buffers per call. It ends every element exactly
                // on its end point, so segment `k` is done when an edge ends
                // on `segs[k].end()`. Counting segments instead of matching
                // corner positions keeps a contour that passes through one
                // of its corners earlier from being taken for the corner.
                let mut k = 0;
                let mut last = segs[0].start();
                let mut entering = corner_at[0];
                let mut push = |p: Point, edges: &mut Vec<_>, ends: &mut Vec<u8>| {
                    if (p - last).hypot2() == 0.0 {
                        return;
                    }
                    let ix = edges.len();
                    edges.push((last, p, tag, true));
                    ends.push(0);
                    if let Some(c) = entering.take() {
                        ends[ix] |= bits(c).0;
                        meeting[c - first].1 = ix;
                    }
                    if k < n && p == segs[k].end() {
                        k += 1;
                        if let Some(c) = corner_at[k % n] {
                            ends[ix] |= bits(c).1;
                            meeting[c - first].0 = ix;
                            if k < n {
                                entering = Some(c);
                            }
                        }
                    }
                    last = p;
                };
                kurbo::flatten(contour.iter(), flat_tol, |el| match el {
                    PathEl::LineTo(p) => push(p, &mut edges, &mut ends),
                    PathEl::ClosePath => push(start, &mut edges, &mut ends),
                    _ => {}
                });
            }
            for (c, &(e_in, e_out)) in corners[first..].iter_mut().zip(&meeting) {
                if let (Some(&(a0, a1, _, _)), Some(&(b0, b1, _, _))) =
                    (edges.get(e_in), edges.get(e_out))
                {
                    c.reshape(a1 - a0, b1 - b0);
                }
            }
        }
        let mut index = EdgeIndex::from_edges(edges);
        index.ends = ends;
        let n_buckets = if corners.is_empty() {
            0
        } else {
            index.buckets.len()
        };
        let mut corner_buckets: Vec<Vec<u32>> = alloc::vec![Vec::new(); n_buckets];
        let mut corner_reach: f64 = 0.0;
        for (i, c) in corners.iter().enumerate() {
            let r = reach * c.reach;
            corner_reach = corner_reach.max(r);
            let lo = index.bucket_ix(c.v.y - r);
            let hi = index.bucket_ix(c.v.y + r);
            for bucket in &mut corner_buckets[lo..=hi] {
                bucket.push(i as u32);
            }
        }
        // Sort each bucket by `x`, so a query visits only the corners within
        // reach sideways, not every corner in the bucket. A bucket across
        // the top of a gear holds dozens of teeth.
        for bucket in &mut corner_buckets {
            bucket.sort_by(|&i, &j| corners[i as usize].v.x.total_cmp(&corners[j as usize].v.x));
        }
        SourceIndex {
            index,
            flat_tol,
            corners,
            corner_buckets,
            corner_reach,
        }
    }

    /// Whether `p` lies outside the band of width `r` along contour
    /// `contour`, joins included: farther than `r` from every edge, measured
    /// square to it, and outside every join on the side `into_fill` picks.
    /// With round joins this is the plain distance test.
    pub(crate) fn is_clear_of_joined(
        &self,
        p: Point,
        r: f64,
        contour: usize,
        into_fill: bool,
    ) -> bool {
        if r <= 0.0 {
            return true;
        }
        let skip = if into_fill {
            FILL_START | FILL_END
        } else {
            OUT_START | OUT_END
        };
        if self
            .index
            .within_where(p, r * r, |tag, real| real && tag == contour as u32, skip)
        {
            return false;
        }
        let Some(bucket) = self.corner_buckets.get(self.index.bucket_ix(p.y)) else {
            return true;
        };
        let x0 = p.x - self.corner_reach;
        let from = bucket.partition_point(|&i| self.corners[i as usize].v.x < x0);
        !bucket[from..]
            .iter()
            .map(|&i| &self.corners[i as usize])
            .take_while(|c| c.v.x <= p.x + self.corner_reach)
            .any(|c| c.tag == contour as u32 && c.into_fill == into_fill && c.covers(p, r))
    }

    /// Whether `p` is at least `sqrt(thresh_sq)` away from contour
    /// `contour` of the path (`None`: away from the whole path).
    ///
    /// Implicit closing chords are not part of the path for distance.
    pub(crate) fn is_clear_of(&self, p: Point, thresh_sq: f64, contour: Option<usize>) -> bool {
        if thresh_sq <= 0.0 {
            return true;
        }
        !self.index.within_where(
            p,
            thresh_sq,
            |tag, real| real && contour.is_none_or(|c| c as u32 == tag),
            0,
        )
    }

    /// Nonzero winding number of the flattened path at `p` (implicit closing
    /// chords included).
    pub(crate) fn winding(&self, p: Point) -> i32 {
        self.index.winding_where(p, |_| true)
    }

    /// Whether every edge of the path bounds its fill, given which contours
    /// run clockwise. No two edges may cross, and each contour must lie in
    /// the others' fill exactly as deep as a boundary does: outside all of
    /// them if it runs clockwise, inside one if it is a hole. Overlapping
    /// contours fail, as does one nested in another of the same direction.
    fn bounds_fill_everywhere(&self, cw: &[bool]) -> bool {
        let edges = &self.index.edges;
        let x_range = |i: u32| {
            let (a, b, _, _) = edges[i as usize];
            (a.x.min(b.x), a.x.max(b.x))
        };
        // Per bucket, sweep in `x`: only edges whose spans overlap can cross.
        let mut order: Vec<u32> = Vec::new();
        let mut active: Vec<u32> = Vec::new();
        for bucket in &self.index.buckets {
            order.clear();
            order.extend_from_slice(bucket);
            order.sort_by(|&i, &j| x_range(i).0.total_cmp(&x_range(j).0));
            active.clear();
            for &i in &order {
                let (a, b, ti, _) = edges[i as usize];
                let x0 = x_range(i).0;
                active.retain(|&j| x_range(j).1 >= x0);
                for &j in &active {
                    let (c, d, tj, _) = edges[j as usize];
                    // Neighbours along a contour share an end exactly.
                    if ti == tj && (b == c || d == a) {
                        continue;
                    }
                    let (ab, cd) = (b - a, d - c);
                    if ab.cross(c - a) * ab.cross(d - a) < 0.0
                        && cd.cross(a - c) * cd.cross(b - c) < 0.0
                    {
                        return false;
                    }
                }
                active.push(i);
            }
        }
        let mut seen = alloc::vec![false; cw.len()];
        edges.iter().all(|&(a, _, tag, _)| {
            let tag = tag as usize;
            if tag >= cw.len() || core::mem::replace(&mut seen[tag], true) {
                return true;
            }
            let others = self.index.winding_where(a, |t| t as usize != tag);
            others == if cw[tag] { 0 } else { 1 }
        })
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
            .within_where(p, r_sq, |tag, _| tag as usize != owner, 0)
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
        {
            let mut band = Band::new(params, left, right, scratch, sink);
            band.suppress_finish = true;
            band.run(c.els.iter().copied());
        }
        let raw: &BezPath = if side == StrokeSide::Left {
            left
        } else {
            right
        };
        if raw.elements().len() < 2 {
            raws.push(BezPath::new());
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
            continue;
        }
        raws.push(closed);
    }

    // --- find every mutual and self intersection of the offsets ---
    let cut = CutSegs::collect(&raws, accuracy);

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
    let keep_at = |p: Point, owner: usize| {
        // Self-validity: outside the band of the piece's own contour, which
        // prunes fold-over past the local thickness. Only the own contour:
        // cross-contour validity is the winding test below. The joins keep
        // their real shape, because plain distance would prune bevel chords
        // that are the true boundary; see `Corner`.
        if !index.is_clear_of_joined(p, thresh, owner, want_filled) {
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
    let keep_piece = |seg: &PathSeg, owner: usize| {
        let first = keep_at(seg.eval(0.5), owner);
        if keep_at(seg.eval(0.25), owner) == first {
            return first;
        }
        keep_at(seg.eval(0.75), owner)
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
                    if keep_at(seg.eval(0.5), ri) {
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
    // A piece that leaves a crossing on the band side of the other strand
    // is pruned, whatever the distance test says. Each offset strand bounds
    // its own band: to its left for an erosion, to its right for a
    // dilation. So of a strand's two pieces at a crossing, exactly one lies
    // inside the other strand's band, and it stays there until its next
    // cut. The distance test misses this where strands cross at a shallow
    // angle. Past the crossing the piece sinks into the other band only
    // slowly, and it is kept until it lies deeper than the slack, which may
    // never happen. Near collapse this is typical: on a 24-tooth gear
    // eroded to 8/9 of its inner radius, arcs ran up to 0.64 deep into
    // their neighbours' bands against a slack of 0.74. The overshoot
    // stitched into extra loops that filled the middle.
    //
    // This rule needs two things. First, the crossing strand must run the
    // same way as its source. The short connector that rejoins two offsets
    // across an inner turn runs backwards, and so does the folded part of
    // a curve's offset. Their band side faces away from the source. A
    // strand that runs with its source has that source `w` away across it,
    // which the foot test checks. Second, the band's far edge must be an
    // offset strand too, so a piece cannot leave the band without a cut.
    // That fails where contours overlap: an edge that runs through another
    // contour's fill has band on both sides, and a piece can leave across
    // the edge itself, as on Inter's `4`, whose stem overlaps the diagonal
    // and the bar. So the rule applies only when every edge of the source
    // bounds the fill. That check sweeps the whole source, so it runs once,
    // and only when a crossing would ban something.
    //
    // `materialize` also ignores crossings too shallow to read, and pieces
    // that run on too deep for their crossing to explain.
    let band_normal = if want_filled { -1.0 } else { 1.0 };
    let foot_r = 0.5 * width;
    let one_sided = core::cell::OnceCell::new();
    let bounds_band = |x: Point, dir: Vec2, owner: usize| {
        let foot = x + dir.turn_90() * (band_normal * width);
        !index.is_clear_of(foot, foot_r * foot_r, Some(owner))
            && *one_sided.get_or_init(|| index.bounds_fill_everywhere(&cw))
    };
    let kept: Vec<Option<(usize, PathSeg)>> = cut
        .materialize(want_filled, bounds_band, MAX_BAN_DEPTH * fuzz)
        .into_iter()
        .map(|(owner, seg, banned)| (!banned && keep_piece(&seg, owner)).then_some((owner, seg)))
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
    cuts: Vec<Vec<f64>>,
    /// The crossings behind the cuts, as `(i, ta, j, tb)`: segment `i` at
    /// `ta` meets segment `j` at `tb`. Only the cut path reads them, so
    /// their angles are worked out later, in `materialize`.
    crossings: Vec<(u32, f64, u32, f64)>,
    /// Whether any cut lands in a segment's interior. `subdivide_at` drops
    /// endpoint touches anyway, so without interior cuts the subdivision is
    /// a no-op and the fast path applies.
    any_interior: bool,
}

impl CutSegs {
    fn collect(raws: &[BezPath], accuracy: f64) -> CutSegs {
        // Flatten to a segment list, remembering the owner and the index
        // range of each contour so adjacency can be recognised.
        let mut segs: Vec<PathSeg> = Vec::new();
        let mut owner: Vec<usize> = Vec::new();
        let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(raws.len());
        for (ix, raw) in raws.iter().enumerate() {
            let start = segs.len();
            for seg in raw.segments() {
                segs.push(seg);
                owner.push(ix);
            }
            ranges.push((start, segs.len()));
        }
        let n = segs.len();
        let bboxes: Vec<Rect> = segs
            .iter()
            .map(kurbo::ParamCurveExtrema::bounding_box)
            .collect();
        let mut cuts: Vec<Vec<f64>> = alloc::vec![Vec::new(); n];
        let mut crossings: Vec<(u32, f64, u32, f64)> = Vec::new();
        let mut any_interior = false;
        let add = |cuts: &mut Vec<Vec<f64>>, any: &mut bool, ix: usize, t: f64| {
            *any |= t > split::T_EPS && t < 1.0 - split::T_EPS;
            cuts[ix].push(t);
        };

        for i in 0..n {
            let selfs = split::segment_self_intersection_params(&segs[i], accuracy);
            for pair in selfs.chunks_exact(2) {
                let (ta, tb) = (pair[0], pair[1]);
                add(&mut cuts, &mut any_interior, i, ta);
                add(&mut cuts, &mut any_interior, i, tb);
                crossings.push((i as u32, ta, i as u32, tb));
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
                    crossings.push((i as u32, ta, j as u32, tb));
                }
            }
        }
        CutSegs {
            segs,
            owner,
            cuts,
            crossings,
            any_interior,
        }
    }

    /// Subdivide every segment at its cuts. Returns `(owner, piece,
    /// banned)` in owner-major path order. `banned` marks a piece that
    /// leaves one of its crossings on the band side of the other strand.
    ///
    /// The band lies to the left of every offset strand when `band_left`,
    /// as for an erosion, and to the right otherwise.
    /// `bounds_band(x, dir, owner)` says whether a strand of loop `owner`
    /// passing `x` in unit direction `dir` really bounds its band there.
    /// A strand that does not bans nothing. Nor does a crossing ban a piece
    /// that would run deeper than `max_depth` into the band.
    fn materialize(
        mut self,
        band_left: bool,
        bounds_band: impl Fn(Point, Vec2, usize) -> bool,
        max_depth: f64,
    ) -> Vec<(usize, PathSeg, bool)> {
        // Per segment, its crossings as `(t, sin)`: the cut parameter, and
        // the sine of the angle from the other strand's direction to this
        // one's. It is positive where this strand passes from the other's
        // left to its right. For a band on the right the sign is flipped, so
        // negative always means leaving into the band.
        let sign = if band_left { 1.0 } else { -1.0 };
        let mut sides: Vec<Vec<(f64, f64)>> = alloc::vec![Vec::new(); self.segs.len()];
        for &(i, ta, j, tb) in &self.crossings {
            let (i, j) = (i as usize, j as usize);
            let Some((x, da, db)) = crossing(&self.segs[i], ta, &self.segs[j], tb) else {
                continue;
            };
            let sin = sign * db.cross(da);
            if sin.abs() < MIN_CROSSING_SINE || (sin.abs() < MIN_OPPOSED_SINE && da.dot(db) < 0.0) {
                continue;
            }
            if bounds_band(x, db, self.owner[j]) {
                sides[i].push((ta, sin));
            }
            if bounds_band(x, da, self.owner[i]) {
                sides[j].push((tb, -sin));
            }
        }
        let mut out = Vec::with_capacity(self.segs.len());
        for (i, sides) in sides.iter().enumerate() {
            let pieces = split::subdivide_at(&self.segs[i], &mut self.cuts[i]);
            let first = out.len();
            for piece in pieces {
                out.push((self.owner[i], piece, false));
            }
            // `subdivide_at` leaves the remaining cuts in place, sorted.
            // Piece `k` runs from cut `k - 1` to cut `k`, and the segment's
            // ends stand in before the first cut and after the last.
            let ts = &self.cuts[i];
            for &(t, sin) in sides {
                // The piece boundary this crossing landed on: the nearest
                // remaining cut, or an end of the segment.
                let b = ts.partition_point(|&c| c < t);
                let below = if b == 0 { 0.0 } else { ts[b - 1] };
                let above = ts.get(b).copied().unwrap_or(1.0);
                let b = if t - below <= above - t { b } else { b + 1 };
                // Leaving into the band bans the piece that starts here.
                // Arriving from the band bans the piece that ends here.
                let k = if sin < 0.0 {
                    if b > ts.len() {
                        continue;
                    }
                    b
                } else {
                    if b == 0 {
                        continue;
                    }
                    b - 1
                };
                // A piece that runs deeper than `max_depth` past its
                // crossing, a few times the slack, would fail the distance
                // test if it were still in the band. If it passes anyway, it
                // left the band in a way the crossing cannot see, so the
                // crossing says nothing about it. That happens across a
                // stroke a little thinner than twice the width: what
                // survives of both inner offsets barely passes the distance
                // test, and only cancels while both remain.
                let piece = &out[first + k].1;
                if (piece.end() - piece.start()).hypot() * sin.abs() <= max_depth {
                    out[first + k].2 = true;
                }
            }
        }
        out
    }
}

/// Below this sine a crossing bans nothing, and its pieces are left to the
/// distance test. At such shallow angles, rounding in the directions can
/// decide which side a piece leaves on.
const MIN_CROSSING_SINE: f64 = 1e-3;

/// The same floor for strands running within about 3° of opposite ways.
/// Such a pair is usually an offset and the backwards connector beside it,
/// and on glyph outlines their crossings cut long runs of real boundary.
/// Strands running the same way keep the lower floor above: on a fine gear
/// the bevel chords of neighbouring roots cross within a degree of each
/// other, and that overshoot has to be pruned.
const MIN_OPPOSED_SINE: f64 = 0.05;

/// How deep a banned piece may run into the band past its crossing, in
/// multiples of the pruning fuzz; see `materialize`. Near an erosion's
/// collapse the overshoot sinks only a fraction of the fuzz.
const MAX_BAN_DEPTH: f64 = 4.0;

/// Where `a` near `ta` crosses `b` near `tb`, and the unit directions of
/// both there. `None` where either direction vanishes.
///
/// The search already moves its crossings onto the curves where it can.
/// Polishing again costs one evaluation for those, and covers the ones it
/// had to leave as found.
fn crossing(a: &PathSeg, ta: f64, b: &PathSeg, tb: f64) -> Option<(Point, Vec2, Vec2)> {
    let (ta, tb) = split::polish_crossing(a, ta, b, tb);
    let (da, db) = (split::deriv(a, ta), split::deriv(b, tb));
    let (la, lb) = (da.hypot(), db.hypot());
    (la > 0.0 && lb > 0.0 && (la * lb).is_finite())
        .then(|| (a.eval(ta).midpoint(b.eval(tb)), da / la, db / lb))
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

/// Bridge chords longer than this multiple of the band width are refused.
///
/// A legitimate bridge replaces dropped join geometry, so it is bounded by
/// the join: a bevel chord spans at most `2w`, and a miter leg at the miter
/// limit somewhat more. A gap far beyond that means pruning removed a whole
/// run of pieces, and connecting across it would draw a confident straight
/// line through geometry that was never part of the boundary. Ending the
/// loop there yields a visibly missing piece instead of a wrong one.
const MAX_BRIDGE: f64 = 8.0;

/// Reassemble surviving pieces into closed loops.
///
/// At an intersection the surviving strand continues from the same point,
/// so the walk hands over to whichever unused piece starts there. That
/// includes a piece belonging to another contour, which is how overlapping
/// bands merge into a single boundary. Elsewhere, as after a trimmed spike
/// or a piece whose validity changed where nothing cut it, it bridges with
/// a line to the nearest survivor of any contour, unless its own start is
/// nearer and the loop closes.
fn stitch(kept: &[Option<(usize, PathSeg)>], accuracy: f64, width: f64) -> Vec<BezPath> {
    let n = kept.len();
    // The match radius. A crossing's two cut points agree to within the
    // intersection search's accuracy, and this allows for that sixteenfold,
    // but never past a quarter of the band width: at a coarse tolerance
    // sixteenfold reached past the band itself (4 against a width of 2), so
    // the walk handed over to whatever piece of the boundary lay nearby, or
    // closed on its own start before going round.
    //
    // Even so, the radius can be longer than the pieces. Near collapse an
    // erosion is a ring of short arcs: on a gear eroded to a tenth of its
    // radius at tolerance 0.25, each is under two units long against a
    // radius of 4. So coming back within the radius of the start closes the
    // loop only if it bounds more than a sliver. Otherwise every walk would
    // end after its first piece, and the sliver filter would drop the whole
    // erosion.
    let eps = (accuracy * 16.0).min(0.25 * width).max(1e-9);
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
    // match, preferring the lowest index among valid candidates. Bridges
    // reach further, through a grid keyed at the band width, searched ring
    // by ring outwards until no nearer start can remain.
    let cell = eps;
    let key = |p: Point| -> (i64, i64) {
        (
            math::floor(p.x / cell) as i64,
            math::floor(p.y / cell) as i64,
        )
    };
    let coarse = width.max(eps);
    let coarse_key = |p: Point| -> (i64, i64) {
        (
            math::floor(p.x / coarse) as i64,
            math::floor(p.y / coarse) as i64,
        )
    };
    let mut starts: BTreeMap<(i64, i64), Vec<u32>> = BTreeMap::new();
    let mut near: BTreeMap<(i64, i64), Vec<u32>> = BTreeMap::new();
    for (i, k) in kept.iter().enumerate() {
        if let Some((_, seg)) = k {
            starts.entry(key(seg.start())).or_default().push(i as u32);
            near.entry(coarse_key(seg.start()))
                .or_default()
                .push(i as u32);
        }
    }
    let rings = math::ceil(max_bridge / coarse) as i64 + 1;
    // The unused survivor starting nearest `end`, lowest index among equals,
    // or nothing within the bridge limit's reach.
    let nearest = |end: Point, used: &[bool]| -> Option<(f64, usize)> {
        let (kx, ky) = coarse_key(end);
        let mut best: Option<(f64, usize)> = None;
        for r in 0..=rings {
            // Every start in ring `r` lies at least `(r - 1)` cells away.
            if best.is_some_and(|(d, _)| d <= (r - 1) as f64 * coarse) {
                break;
            }
            for dy in -r..=r {
                let step = if dy == -r || dy == r { 1 } else { 2 * r.max(1) };
                let mut dx = -r;
                while dx <= r {
                    // Saturating, as for continuations below.
                    let cell = (kx.saturating_add(dx), ky.saturating_add(dy));
                    for &k in near.get(&cell).into_iter().flatten() {
                        let k = k as usize;
                        if used[k] {
                            continue;
                        }
                        let Some((_, s)) = kept[k] else { continue };
                        let d = (s.start() - end).hypot();
                        if best.is_none_or(|(b, bk)| d < b || (d == b && k < bk)) {
                            best = Some((d, k));
                        }
                    }
                    dx += step;
                }
            }
        }
        best
    };

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
            let (_, seg) = kept[cur].unwrap();
            split::append_seg(&mut path, seg);
            let end = seg.end();
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
            // Back within the match radius of its start, the loop closes,
            // unless it would be a sliver and a piece continues from here.
            if (end - first_pt).hypot() <= eps
                && (next.is_none() || {
                    let mut closed = path.clone();
                    closed.close_path();
                    substantial_loop(&closed, accuracy)
                })
            {
                break;
            }
            if next.is_none() {
                // Otherwise the nearest survivor, bridged, unless the loop's
                // own start is nearer.
                next = nearest(end, &used)
                    .filter(|&(d, _)| d < (first_pt - end).hypot())
                    .map(|(_, k)| k);
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
