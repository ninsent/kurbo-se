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

use alloc::vec::Vec;
use core::ops::RangeInclusive;

use kurbo::{BezPath, Join, ParamCurve, PathEl, PathSeg, Point, Rect, Shape, Vec2};

use crate::buckets::{Bands, Buckets};
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
///
/// The edges are listed by horizontal band. Each band is sorted by the left
/// end of its edges' `x` ranges and keeps the running maximum of their
/// right ends. A winding query reads all of one band. A distance query
/// needs only the edges within its radius in `x`, and the two orderings
/// narrow those to one run per band: see [`EdgeIndex::within_where`]. A
/// band through a detailed outline holds edges from all the way across it,
/// so scanning all of them is slow.
///
/// The bands are fine, so that a ray meets few edges. On a wide, flat
/// outline a band is then much thinner than a distance query's radius. A
/// query would search dozens of bands and meet the same long edges in each.
/// So such an index also lists its edges by groups of whole fine bands,
/// about half that radius tall, for distance queries only: see
/// [`CoarseRows`].
#[derive(Default, Debug)]
struct EdgeIndex {
    edges: Vec<(Point, Point, u32, bool)>,
    /// Per edge, which of its ends are corners of the source contour and
    /// on which side each corner's join lies, as bits like [`FILL_START`].
    /// Empty means no edge has a corner.
    ends: Vec<u8>,
    /// The `y` range of each edge: working storage of [`EdgeIndex::reindex`].
    spans: Vec<(f64, f64)>,
    /// The fine bands the edges are listed by.
    bands: Bands,
    /// The edges whose `y` range meets each fine band, by left end.
    rows: XBands,
    /// The same edges by groups of fine bands. Built only where the fine
    /// bands are far thinner than a distance query's radius: see
    /// [`EdgeIndex::coarse`].
    coarse: CoarseRows,
    /// The largest magnitude of any edge coordinate. Queries round at this
    /// scale.
    scale: f64,
    /// The smallest nonzero height of an edge, or infinity.
    min_rise: f64,
}

/// Edges listed by band, each band sorted by the left end of the edges'
/// `x` ranges, with the running maximum of their right ends alongside.
#[derive(Default, Debug)]
struct XBands {
    buckets: Buckets,
    /// Parallel to the entries. For each entry, the largest right end of an
    /// `x` range among the band's edges up to and including that one.
    reach: Vec<f64>,
}

impl XBands {
    /// Sort each band of the freshly rebuilt `buckets` by left end, and
    /// record the running maxima of the right ends. Keeps the storage.
    fn sort_by_left_end(&mut self, edges: &[(Point, Point, u32, bool)]) {
        let XBands { buckets, reach } = self;
        reach.clear();
        reach.resize(buckets.total(), 0.0);
        for k in 0..buckets.len() {
            buckets.get_mut(k).sort_by(|&i, &j| {
                let (ai, bi, _, _) = edges[i as usize];
                let (aj, bj, _, _) = edges[j as usize];
                x_range(ai, bi).0.total_cmp(&x_range(aj, bj).0)
            });
            let mut far = f64::NEG_INFINITY;
            let from = buckets.offset(k);
            for (e, &ix) in buckets.get(k).iter().enumerate() {
                let (a, b, _, _) = edges[ix as usize];
                far = far.max(x_range(a, b).1);
                reach[from + e] = far;
            }
        }
    }

    /// The edge ids of band `k`, by left end, and their running right-end
    /// maxima.
    fn band(&self, k: usize) -> (&[u32], &[f64]) {
        let ids = self.buckets.get(k);
        let from = self.buckets.offset(k);
        (ids, &self.reach[from..from + ids.len()])
    }
}

/// An index's edges listed again by groups of `group` whole fine bands.
///
/// A query over a run of fine bands reads the groups that hold them. It
/// skips the edges a group lists only because of fine bands outside the
/// run. Every edge listed in a fine band of the run is also listed in that
/// band's group. So the query tests exactly the edges a query over the
/// fine bands would, and finds the same. That matters because a distance
/// test can accept an edge a rounding error outside the span asked about,
/// and which of those a query finds depends on the bands it reads.
#[derive(Default, Debug)]
struct CoarseRows {
    /// Fine bands per group, or `0` when the index has no groups.
    group: usize,
    rows: XBands,
    /// Per edge, the first and last fine band it is listed in.
    fine: Vec<(u32, u32)>,
}

impl CoarseRows {
    /// Build groups about half `radius` tall if each holds at least four
    /// fine bands, and none otherwise. Keeps the storage either way.
    fn rebuild(
        &mut self,
        bands: &Bands,
        spans: &[(f64, f64)],
        edges: &[(Point, Point, u32, bool)],
        radius: f64,
    ) {
        // A group holds at least four fine bands. With fewer bands there is
        // no group to build, and no need to measure the outline.
        if bands.len() < 4 {
            self.group = 0;
            return;
        }
        let height = spans.iter().map(|s| s.1).fold(f64::NEG_INFINITY, f64::max)
            - spans.iter().map(|s| s.0).fold(f64::INFINITY, f64::min);
        // A ratio too large for `usize` saturates.
        let groups = (crate::math::ceil(2.0 * height / radius) as usize).max(1);
        if !(radius > 0.0 && height.is_finite() && groups <= bands.len() / 4) {
            self.group = 0;
            return;
        }
        let group = bands.len() / groups;
        let CoarseRows {
            group: g,
            rows,
            fine,
        } = self;
        *g = group;
        fine.clear();
        fine.extend(spans.iter().map(|&span| {
            let k = bands.spanned(span);
            (*k.start() as u32, *k.end() as u32)
        }));
        rows.buckets
            .rebuild_by_bands(bands.len().div_ceil(group), edges.len(), |i| {
                fine[i].0 as usize / group..=fine[i].1 as usize / group
            });
        rows.sort_by_left_end(edges);
    }

    /// The groups holding the fine bands `fine`.
    fn holding(&self, fine: &RangeInclusive<usize>) -> RangeInclusive<usize> {
        fine.start() / self.group..=fine.end() / self.group
    }

    /// Whether edge `ix` is listed in any of the fine bands `fine`.
    fn lists(&self, ix: u32, fine: &RangeInclusive<usize>) -> bool {
        let (first, last) = self.fine[ix as usize];
        first as usize <= *fine.end() && last as usize >= *fine.start()
    }
}

/// The edge's start point is a corner whose join lies on the fill side.
const FILL_START: u8 = 1;
/// The edge's end point is a corner whose join lies on the fill side.
const FILL_END: u8 = 2;
/// The edge's start point is a corner whose join lies away from the fill.
const OUT_START: u8 = 4;
/// The edge's end point is a corner whose join lies away from the fill.
const OUT_END: u8 = 8;

/// The `x` range of an edge.
fn x_range(a: Point, b: Point) -> (f64, f64) {
    if a.x <= b.x { (a.x, b.x) } else { (b.x, a.x) }
}

/// The `y` range of an edge.
fn y_range(a: Point, b: Point) -> (f64, f64) {
    if a.y <= b.y { (a.y, b.y) } else { (b.y, a.y) }
}

impl EdgeIndex {
    /// List the current `edges` again for winding queries, and for distance
    /// queries with radii around `radius`. Keeps the storage. Callers
    /// refill `edges` and `ends` first.
    fn reindex(&mut self, radius: f64) {
        let EdgeIndex {
            edges,
            spans,
            bands,
            rows,
            coarse,
            scale,
            min_rise,
            ..
        } = self;
        spans.clear();
        spans.extend(edges.iter().map(|&(a, b, _, _)| y_range(a, b)));
        *bands = Bands::fine(spans);
        rows.buckets.rebuild(bands, spans);
        rows.sort_by_left_end(edges);
        coarse.rebuild(bands, spans, edges, radius);
        *scale = edges
            .iter()
            .map(|(a, b, _, _)| a.x.abs().max(a.y.abs()).max(b.x.abs()).max(b.y.abs()))
            .fold(0.0, f64::max);
        *min_rise = edges
            .iter()
            .map(|(a, b, _, _)| (b.y - a.y).abs())
            .filter(|&rise| rise > 0.0)
            .fold(f64::INFINITY, f64::min);
    }

    /// The groups of fine bands, if the index has them.
    fn coarse(&self) -> Option<&CoarseRows> {
        (self.coarse.group > 0).then_some(&self.coarse)
    }

    /// The rows a distance query over the fine bands `fine` reads, and
    /// which of their bands it reads.
    fn distance_rows(&self, fine: &RangeInclusive<usize>) -> (&XBands, RangeInclusive<usize>) {
        match self.coarse() {
            Some(coarse) => (&coarse.rows, coarse.holding(fine)),
            None => (&self.rows, fine.clone()),
        }
    }

    /// Nonzero winding at `p` of the edges whose tag passes `keep`.
    ///
    /// Exactly one bucket is scanned. An edge crossing `y = p.y` always
    /// spans that bucket, and visiting a neighbour would double-count edges
    /// registered in both, breaking the cancellation that makes the winding
    /// zero outside the shape.
    fn winding_where(&self, p: Point, keep: impl Fn(u32) -> bool) -> i32 {
        let mut w = 0;
        for &ix in self.rows.buckets.get(self.bands.of(p.y)) {
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

    /// Whether the box of any edge meets `r`.
    ///
    /// The comparisons are exact, so an edge whose box meets `r` shares a
    /// height with `r`. It is then listed in a band that [`Bands::spanned`]
    /// gives, or in that band's group. In that band it lies in the run that
    /// starts where the running right-end maximum reaches `r.x0` and ends
    /// where left ends pass `r.x1`.
    fn any_edge_box_meets(&self, r: Rect) -> bool {
        let (rows, bands) = self.distance_rows(&self.bands.spanned((r.y0, r.y1)));
        bands.into_iter().any(|k| {
            let (ids, reach) = rows.band(k);
            let from = reach.partition_point(|&far| far < r.x0);
            ids[from..]
                .iter()
                .map(|&ix| self.edges[ix as usize])
                .take_while(|&(a, b, _, _)| x_range(a, b).0 <= r.x1)
                .any(|(a, b, _, _)| {
                    let (y0, y1) = y_range(a, b);
                    x_range(a, b).1 >= r.x0 && y0 <= r.y1 && y1 >= r.y0
                })
        })
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
    ///
    /// It searches the fine bands that [`Bands::searched`] gives, through
    /// their groups where the index has them. In each band the scan starts
    /// where the running right-end maximum first reaches the window
    /// `[lo_x, hi_x]`, and stops where left ends pass it. So every edge it
    /// skips lies wholly outside the window. The window is the radius
    /// around `p.x`, widened by a margin far above rounding, so the box test
    /// of [`EdgeIndex::edge_within`] rejects every skipped edge. The answer
    /// is the same as a scan over the whole of each fine band.
    fn within_where(
        &self,
        p: Point,
        r_sq: f64,
        keep: impl Fn(u32, bool) -> bool,
        skip: u8,
    ) -> bool {
        let r = crate::math::sqrt(r_sq);
        let fine = self.bands.searched((p.y - r, p.y + r));
        // Two copies of the scan. The common one, over the fine bands, then
        // carries no test for the groups.
        match self.coarse() {
            None => self.any_within(&self.rows, fine, p, r_sq, keep, skip, |_| true),
            Some(coarse) => {
                let groups = coarse.holding(&fine);
                self.any_within(&coarse.rows, groups, p, r_sq, keep, skip, |ix| {
                    coarse.lists(ix, &fine)
                })
            }
        }
    }

    /// The scan of [`EdgeIndex::within_where`] over `bands` of `rows`. It
    /// tests only the edges that pass `listed`.
    #[allow(clippy::too_many_arguments)]
    fn any_within(
        &self,
        rows: &XBands,
        bands: RangeInclusive<usize>,
        p: Point,
        r_sq: f64,
        keep: impl Fn(u32, bool) -> bool,
        skip: u8,
        listed: impl Fn(u32) -> bool,
    ) -> bool {
        let r = crate::math::sqrt(r_sq);
        let margin = 1e-9 * (crate::math::abs(p.x) + r);
        let (lo_x, hi_x) = (p.x - r - margin, p.x + r + margin);
        for k in bands {
            let (ids, reach) = rows.band(k);
            let from = reach.partition_point(|&far| far < lo_x);
            for &ix in &ids[from..] {
                let (a, b, tag, real) = self.edges[ix as usize];
                if x_range(a, b).0 > hi_x {
                    break;
                }
                if listed(ix) && keep(tag, real) && self.edge_within(ix, p, r_sq, skip) {
                    return true;
                }
            }
        }
        false
    }

    /// Whether edge `ix` lies within `sqrt(r_sq)` of `p`: the per-edge test
    /// of [`EdgeIndex::within_where`]. Past an end flagged in `skip`, the
    /// edge does not count.
    ///
    /// It runs for every candidate edge of every distance query. Called out
    /// of line from the two copies of the scan, it was several percent
    /// slower on dense outlines.
    #[inline(always)]
    fn edge_within(&self, ix: u32, p: Point, r_sq: f64, skip: u8) -> bool {
        let (a, b, _, _) = self.edges[ix as usize];
        let (x0, x1) = x_range(a, b);
        let dx = (x0 - p.x).max(p.x - x1).max(0.0);
        if dx * dx >= r_sq {
            return false;
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
                return false;
            }
        }
        let t = t.clamp(0.0, 1.0);
        ((p - a) - t * ab).hypot2() < r_sq
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
#[derive(Default, Debug)]
pub(crate) struct SourceIndex {
    index: EdgeIndex,
    flat_tol: f64,
    /// The corners of the contours, where a band emits a join. Empty unless
    /// built by [`SourceIndex::rebuild_with_joins`].
    corners: Vec<Corner>,
    /// The corners' ordinals listed by the bands of the edge index, each
    /// band sorted by `x`. Unused without corners.
    corner_buckets: Buckets,
    /// The farthest any join reaches from its corner, at the widest width
    /// asked about. A query searches its bucket within this distance in `x`.
    corner_reach: f64,
    /// Working storage of [`SourceIndex::rebuild_with_joins`]. Per contour:
    /// its segments, the corner at each joint, and the edges that meet at
    /// each corner. Then over all corners, the span of `y` each one reaches.
    segs: Vec<PathSeg>,
    corner_at: Vec<Option<usize>>,
    meeting: Vec<(usize, usize)>,
    corner_spans: Vec<(f64, f64)>,
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
#[derive(Copy, Clone, Debug)]
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
    #[cfg(test)]
    pub(crate) fn new(path: &BezPath, flat_tol: f64, radius: f64) -> Self {
        let mut index = SourceIndex::default();
        index.rebuild(path, flat_tol, radius);
        index
    }

    /// Index `path` flattened at `flat_tol`, for distance queries with radii
    /// around `radius`. Keeps the storage.
    pub(crate) fn rebuild(&mut self, path: &BezPath, flat_tol: f64, radius: f64) {
        let extent = path.control_box().size().max_side();
        let flat_tol = flat_tol.max(extent * MIN_RELATIVE_FLAT_TOL).max(1e-9);
        let edges = &mut self.index.edges;
        edges.clear();
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
                close(edges, last, start, sub);
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
        close(edges, last, start, sub);
        self.index.ends.clear();
        self.index.reindex(radius);
        self.flat_tol = flat_tol;
        self.corners.clear();
        self.corner_reach = 0.0;
    }

    /// Rebuild as an index over closed `contours`, tagged by ordinal, that
    /// also knows where a band with `join` emits joins: see [`Corner`].
    /// `reach` is the widest width [`SourceIndex::is_clear_of_joined`] will
    /// be asked about. Keeps the storage.
    pub(crate) fn rebuild_with_joins(
        &mut self,
        contours: &[BezPath],
        flat_tol: f64,
        join: Join,
        miter_limit: f64,
        join_thresh: f64,
        reach: f64,
    ) {
        let extent = contours
            .iter()
            .map(|c| c.control_box().size().max_side())
            .fold(0.0, f64::max);
        let flat_tol = flat_tol.max(extent * MIN_RELATIVE_FLAT_TOL).max(1e-9);
        let SourceIndex {
            index,
            corners,
            corner_buckets,
            segs,
            corner_at,
            meeting,
            corner_spans,
            ..
        } = self;
        let (edges, ends) = (&mut index.edges, &mut index.ends);
        edges.clear();
        ends.clear();
        corners.clear();
        for (tag, contour) in contours.iter().enumerate() {
            let tag = tag as u32;
            segs.clear();
            segs.extend(contour.segments().filter(|s| !math::is_degenerate(s)));
            let n = segs.len();
            if n == 0 {
                continue;
            }
            // The corner, if any, at the joint where `segs[j]` starts.
            let first = corners.len();
            corner_at.clear();
            corner_at.resize(n, None);
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
            meeting.clear();
            meeting.resize(corners.len() - first, (usize::MAX, usize::MAX));
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
                    PathEl::LineTo(p) => push(p, edges, ends),
                    PathEl::ClosePath => push(start, edges, ends),
                    _ => {}
                });
            }
            for (c, &(e_in, e_out)) in corners[first..].iter_mut().zip(meeting.iter()) {
                if let (Some(&(a0, a1, _, _)), Some(&(b0, b1, _, _))) =
                    (edges.get(e_in), edges.get(e_out))
                {
                    c.reshape(a1 - a0, b1 - b0);
                }
            }
        }
        index.reindex(reach);
        self.corner_reach = corners.iter().map(|c| reach * c.reach).fold(0.0, f64::max);
        if !corners.is_empty() {
            corner_spans.clear();
            corner_spans.extend(
                corners
                    .iter()
                    .map(|c| (c.v.y - reach * c.reach, c.v.y + reach * c.reach)),
            );
            corner_buckets.rebuild(&index.bands, corner_spans);
            // Sort each band by `x`, so a query visits only the corners within
            // reach sideways, not every corner in the band. A band across the
            // top of a gear holds dozens of teeth.
            for k in 0..index.bands.len() {
                corner_buckets
                    .get_mut(k)
                    .sort_by(|&i, &j| corners[i as usize].v.x.total_cmp(&corners[j as usize].v.x));
            }
        }
        self.flat_tol = flat_tol;
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
        if self.corners.is_empty() {
            return true;
        }
        let bucket = self.corner_buckets.get(self.index.bands.of(p.y));
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

    /// Whether no edge of the flattened path, implicit closing chords
    /// included, comes within `margin` of `rect`.
    ///
    /// If so, no edge crosses `rect`. Every point in it then has the same
    /// [`winding`](Self::winding), and lies at least `margin` from the path
    /// for [`is_clear_of`](Self::is_clear_of). The test compares the edges'
    /// boxes with `rect` grown a little more than `margin`, so it can only
    /// err towards saying no.
    ///
    /// The computed answers agree too. Beside an edge, the winding is the
    /// sign of a cross product and the distance is a difference. Both round
    /// at the scale of the edge's and the point's coordinates. A billionth
    /// of each scale in the slack keeps the computed sign and distance far
    /// from their thresholds, however long the edge. Rounding is relative
    /// only above the subnormal range, though. If an edge rises so little
    /// that its cross products with points the slack away could fall into
    /// that range, the test says no. No real outline has such an edge.
    pub(crate) fn is_box_clear(&self, rect: Rect, margin: f64) -> bool {
        let scale =
            margin + rect.size().max_side() + rect.origin().to_vec2().hypot() + self.index.scale;
        let slack = margin + 1e-9 * scale;
        if self.index.min_rise * slack < 1e-280 {
            return false;
        }
        let r = rect.inflate(slack, slack);
        !self.index.any_edge_box_meets(r)
    }

    /// Whether every edge of the path bounds its fill, given which contours
    /// run clockwise. No two edges may cross, and each contour must lie in
    /// the others' fill exactly as deep as a boundary does: outside all of
    /// them if it runs clockwise, inside one if it is a hole. Overlapping
    /// contours fail, as does one nested in another of the same direction.
    fn bounds_fill_everywhere(&self, cw: &[bool]) -> bool {
        let edges = &self.index.edges;
        let span = |i: u32| {
            let (a, b, _, _) = edges[i as usize];
            x_range(a, b)
        };
        // Per band, sweep in `x` over the edges in the band's order, by left
        // end. Only edges whose spans overlap can cross.
        let mut active: Vec<u32> = Vec::new();
        for k in 0..self.index.bands.len() {
            active.clear();
            for &i in self.index.rows.buckets.get(k) {
                let (a, b, ti, _) = edges[i as usize];
                let x0 = span(i).0;
                active.retain(|&j| span(j).1 >= x0);
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
#[derive(Default, Debug)]
struct RawIndex {
    index: EdgeIndex,
}

impl RawIndex {
    /// Index the raw loops `raws` flattened at `flat_tol`, for distance
    /// queries with radii around `radius`. Keeps the storage.
    fn rebuild(&mut self, raws: &[BezPath], flat_tol: f64, radius: f64) {
        let extent = raws
            .iter()
            .map(|r| r.control_box().size().max_side())
            .fold(0.0, f64::max);
        let flat_tol = flat_tol.max(extent * MIN_RELATIVE_FLAT_TOL);
        let edges = &mut self.index.edges;
        edges.clear();
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
        self.index.ends.clear();
        self.index.reindex(radius);
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
pub(crate) fn region_loops<'w>(
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
    work: &'w mut RegionWork,
) -> &'w [BezPath] {
    // Resolution of the pruning predicate. Offset approximation, tolerance
    // and flattening error stack up to about this length; features smaller
    // than it fall inside the documented boundary band.
    let fuzz = PRUNE_SLACK * width + 2.0 * base.tolerance + index.slack();

    let RegionWork {
        raws,
        cut,
        raw_index,
        cw,
        keep,
        pieces,
        kept,
        stitch_work,
        loops,
    } = work;

    // --- raw offsets, one per contour ---
    if raws.len() < contours.len() {
        raws.resize_with(contours.len(), BezPath::new);
    }
    let raws = &mut raws[..contours.len()];
    for (c, dst) in contours.iter().zip(raws.iter_mut()) {
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
        dst.truncate(0);
        if raw.elements().len() < 2 {
            continue;
        }
        dst.extend(raw.iter());
        dst.close_path();
        // Collapsed offset, where the width reaches the local thickness:
        // the loop degenerates to a tolerance-scale cluster, which
        // `offset_cubic`'s cusp handling can shatter into thousands of
        // segments, and the pairwise cut of that cluster is quadratic in
        // them. A loop smaller than the pruning fuzz cannot bound a
        // resolvable region feature, so drop it before it reaches the
        // cutter.
        if dst.control_box().size().max_side() <= fuzz {
            dst.truncate(0);
        }
    }

    // --- find every mutual and self intersection of the offsets ---
    cut.collect(raws, accuracy);

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
    cw.clear();
    cw.extend(contours.iter().map(|c| c.els.area() >= 0.0));
    let live = raws.iter().filter(|r| r.elements().len() >= 2).count();
    let cross_check = live >= 2;
    if cross_check {
        raw_index.rebuild(raws, index.slack(), fuzz);
    }
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
        if !cross_check {
            return true;
        }
        let expected = if cw[owner] { 0 } else { 1 };
        raw_index.winding_excluding(p, owner) == expected
            && !raw_index.is_near_other(p, fuzz * fuzz, owner)
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
        keep.clear();
        keep.resize(raws.len(), false);
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
            loops.clear();
            for (raw, _) in raws.iter_mut().zip(keep.iter()).filter(|(_, keep)| **keep) {
                core::mem::swap(loops.push_new(), raw);
            }
            drop_unresolvable(loops, fuzz, raw_index);
            return loops.as_slice();
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
            && *one_sided.get_or_init(|| index.bounds_fill_everywhere(cw))
    };
    cut.materialize(want_filled, bounds_band, MAX_BAN_DEPTH * fuzz, pieces);
    kept.clear();
    kept.extend(
        pieces.iter().map(|&(owner, seg, banned)| {
            (!banned && keep_piece(&seg, owner)).then_some((owner, seg))
        }),
    );

    stitch(kept, accuracy, width, stitch_work, loops);
    drop_unresolvable(loops, fuzz, raw_index);
    loops.as_slice()
}

/// Working storage of the region construction. `AlignedStrokeCtx` keeps it,
/// so repeated strokes reuse its buffers instead of allocating new ones.
/// Each call clears what it uses first, so nothing carries over from one
/// call to the next.
#[derive(Default, Debug)]
pub(crate) struct RegionWork {
    /// The raw offset loops, one per contour of the current call.
    raws: Vec<BezPath>,
    cut: CutSegs,
    raw_index: RawIndex,
    /// Per contour, whether it runs clockwise.
    cw: Vec<bool>,
    /// Per raw loop, whether the fast path keeps it.
    keep: Vec<bool>,
    /// The pieces of the raw loops, and those that survive pruning.
    pieces: Vec<(usize, PathSeg, bool)>,
    kept: Vec<Option<(usize, PathSeg)>>,
    stitch_work: StitchWork,
    /// The boundary loops found.
    loops: Paths,
}

/// Paths reused as a pool. The first `len` are live, and the rest keep
/// their storage for the next call.
#[derive(Default, Debug)]
pub(crate) struct Paths {
    paths: Vec<BezPath>,
    len: usize,
}

impl Paths {
    fn clear(&mut self) {
        self.len = 0;
    }

    /// An empty path, appended to the live ones.
    fn push_new(&mut self) -> &mut BezPath {
        if self.len == self.paths.len() {
            self.paths.push(BezPath::new());
        }
        self.len += 1;
        let path = &mut self.paths[self.len - 1];
        path.truncate(0);
        path
    }

    /// Discard the last live path.
    fn pop(&mut self) {
        self.len -= 1;
    }

    fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn as_slice(&self) -> &[BezPath] {
        &self.paths[..self.len]
    }

    /// Keep the live paths that pass `keep`, in order.
    fn retain(&mut self, mut keep: impl FnMut(&BezPath) -> bool) {
        let mut kept = 0;
        for i in 0..self.len {
            if keep(&self.paths[i]) {
                self.paths.swap(kept, i);
                kept += 1;
            }
        }
        self.len = kept;
    }
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
///
/// When the net area is near zero, `scratch` is rebuilt over the loops to
/// test whether they coincide. It is the caller's index of the raw loops,
/// which the caller no longer needs by then, so this reuses its storage.
fn drop_unresolvable(loops: &mut Paths, fuzz: f64, scratch: &mut RawIndex) {
    let perimeter_of = |l: &BezPath| {
        let mut per = 0.0;
        for seg in l.segments() {
            per += (seg.end() - seg.start()).hypot();
        }
        per
    };
    loops.retain(|l| l.elements().area().abs() > 0.5 * fuzz * perimeter_of(l).max(1.0));
    if loops.len() < 2 {
        return;
    }
    let net: f64 = loops.as_slice().iter().map(|l| l.elements().area()).sum();
    let perimeter: f64 = loops.as_slice().iter().map(&perimeter_of).sum();
    if net.abs() > 0.5 * fuzz * perimeter.max(1.0) {
        return;
    }
    scratch.rebuild(loops.as_slice(), fuzz * 0.25, fuzz);
    let index = &*scratch;
    let coincident = loops.as_slice().iter().enumerate().all(|(i, l)| {
        l.segments()
            .all(|seg| index.is_near_other(seg.eval(0.5), fuzz * fuzz, i))
    });
    if coincident {
        loops.clear();
    }
}

/// Split off and return the leading entries of `list` that belong to
/// segment `seg`. `list` is grouped by segment in ascending order. Called
/// for each segment in turn, it has already lost the earlier segments'
/// entries, so this segment's entries come first.
fn take_run<'a, T>(list: &mut &'a [T], seg: u32, seg_of: impl Fn(&T) -> u32) -> &'a [T] {
    let (run, rest) = list.split_at(list.partition_point(|e| seg_of(e) == seg));
    *list = rest;
    run
}

/// The segments of a family of closed offset curves, with every mutual and
/// self intersection collected as cut parameters.
#[derive(Default, Debug)]
struct CutSegs {
    segs: Vec<PathSeg>,
    owner: Vec<usize>,
    /// Per loop, the range of `segs` it owns.
    ranges: Vec<(usize, usize)>,
    /// Per segment, its exact box.
    bboxes: Vec<Rect>,
    /// Every cut as `(segment, t)`, in no particular order.
    cuts: Vec<(u32, f64)>,
    /// The crossings behind the cuts, as `(i, ta, j, tb)`: segment `i` at
    /// `ta` meets segment `j` at `tb`. Only the cut path reads them, so
    /// their angles are worked out later, in `materialize`.
    crossings: Vec<(u32, f64, u32, f64)>,
    /// Whether any cut lands in a segment's interior. `split::clean_cuts`
    /// drops endpoint touches anyway, so without interior cuts the
    /// subdivision is a no-op and the fast path applies.
    any_interior: bool,
    /// Working storage of [`CutSegs::materialize`]: the crossings by
    /// segment with the side each leaves on, one segment's cuts, and that
    /// segment's pieces.
    sides: Vec<(u32, f64, f64)>,
    ts: Vec<f64>,
    pieces: Vec<PathSeg>,
}

impl CutSegs {
    /// Collect the segments of `raws` and every cut between them. Keeps the
    /// storage.
    fn collect(&mut self, raws: &[BezPath], accuracy: f64) {
        let CutSegs {
            segs,
            owner,
            ranges,
            bboxes,
            cuts,
            crossings,
            any_interior,
            ..
        } = self;
        // Flatten to a segment list, remembering the owner and the index
        // range of each contour so adjacency can be recognised.
        segs.clear();
        owner.clear();
        ranges.clear();
        for (ix, raw) in raws.iter().enumerate() {
            let start = segs.len();
            for seg in raw.segments() {
                segs.push(seg);
                owner.push(ix);
            }
            ranges.push((start, segs.len()));
        }
        bboxes.clear();
        bboxes.extend(segs.iter().map(kurbo::ParamCurveExtrema::bounding_box));
        cuts.clear();
        crossings.clear();
        *any_interior = false;
        let add = |cuts: &mut Vec<(u32, f64)>, any: &mut bool, ix: usize, t: f64| {
            *any |= t > split::T_EPS && t < 1.0 - split::T_EPS;
            cuts.push((ix as u32, t));
        };

        for (i, seg) in segs.iter().enumerate() {
            let selfs = split::segment_self_intersection_params(seg, accuracy);
            for pair in selfs.chunks_exact(2) {
                let (ta, tb) = (pair[0], pair[1]);
                add(cuts, any_interior, i, ta);
                add(cuts, any_interior, i, tb);
                crossings.push((i as u32, ta, i as u32, tb));
            }
        }
        // The order of the pairs does not matter. Each segment's cuts are
        // sorted before use, and a crossing only ever marks pieces.
        crate::broad::for_each_overlapping_pair(
            bboxes.len(),
            |i| bboxes[i],
            accuracy,
            |i, j| {
                let same = owner[i] == owner[j];
                let (lo, hi) = ranges[owner[i]];
                let next = same && j == i + 1;
                let wrap = same && i == lo && j == hi - 1 && hi - lo > 1;
                let hits = split::segment_pair_params_boxed(
                    (&segs[i], bboxes[i]),
                    (&segs[j], bboxes[j]),
                    next,
                    wrap,
                    accuracy,
                );
                for (ta, tb) in hits {
                    add(cuts, any_interior, i, ta);
                    add(cuts, any_interior, j, tb);
                    crossings.push((i as u32, ta, j as u32, tb));
                }
            },
        );
    }

    /// Subdivide every segment at its cuts into `out`, as `(owner, piece,
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
        &mut self,
        band_left: bool,
        bounds_band: impl Fn(Point, Vec2, usize) -> bool,
        max_depth: f64,
        out: &mut Vec<(usize, PathSeg, bool)>,
    ) {
        // The crossings as `(segment, t, sin)`. There is one entry for each
        // strand that crosses another where the other bounds the band. It
        // holds the crossing strand's segment and cut parameter, and the
        // sine of the angle from the bounding strand's direction to its
        // own. The sine is positive where the strand passes from the
        // bounding strand's left to its right. For a band on the right the
        // sign is flipped, so negative always means leaving into the band.
        let sign = if band_left { 1.0 } else { -1.0 };
        let CutSegs {
            segs,
            owner,
            cuts,
            crossings,
            sides,
            ts,
            pieces,
            ..
        } = self;
        sides.clear();
        for &(i, ta, j, tb) in crossings.iter() {
            let (iu, ju) = (i as usize, j as usize);
            let Some((x, da, db)) = crossing(&segs[iu], ta, &segs[ju], tb) else {
                continue;
            };
            let sin = sign * db.cross(da);
            if sin.abs() < MIN_CROSSING_SINE || (sin.abs() < MIN_OPPOSED_SINE && da.dot(db) < 0.0) {
                continue;
            }
            if bounds_band(x, db, owner[ju]) {
                sides.push((i, ta, sin));
            }
            if bounds_band(x, da, owner[iu]) {
                sides.push((j, tb, -sin));
            }
        }
        // Group both flat lists by segment, to walk them in step with the
        // segments below. The order within a segment does not matter: its
        // cuts are sorted by `t` before use, and its crossings only mark
        // pieces.
        cuts.sort_unstable_by_key(|&(seg, _)| seg);
        sides.sort_unstable_by_key(|&(seg, _, _)| seg);
        let (mut cuts, mut sides) = (cuts.as_slice(), sides.as_slice());
        out.clear();
        for (i, seg) in segs.iter().enumerate() {
            let i = i as u32;
            let here = take_run(&mut cuts, i, |&(s, _)| s);
            let crossed = take_run(&mut sides, i, |&(s, _, _)| s);
            ts.clear();
            ts.extend(here.iter().map(|&(_, t)| t));
            split::clean_cuts(ts);
            pieces.clear();
            split::subdivide_into(seg, ts, pieces);
            let first = out.len();
            out.extend(
                pieces
                    .iter()
                    .map(|&piece| (owner[i as usize], piece, false)),
            );
            // `ts` holds the cleaned cuts, sorted. Piece `k` runs from cut
            // `k - 1` to cut `k`, and the segment's ends stand in before the
            // first cut and after the last.
            for &(_, t, sin) in crossed {
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
fn stitch(
    kept: &[Option<(usize, PathSeg)>],
    accuracy: f64,
    width: f64,
    work: &mut StitchWork,
    loops: &mut Paths,
) {
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
    let StitchWork {
        starts,
        bridge_cells,
        used,
    } = work;
    used.clear();
    used.resize(n, false);
    loops.clear();

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
    starts.rebuild(kept, eps);
    let bridge_cell = width.max(eps);
    bridge_cells.rebuild(kept, bridge_cell);
    let near = NearestStart::new(kept, bridge_cells, bridge_cell, max_bridge);

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
        let path = loops.push_new();
        path.move_to(first_pt);
        let mut cur = start;
        loop {
            used[cur] = true;
            let (_, seg) = kept[cur].unwrap();
            split::append_seg(path, seg);
            let end = seg.end();
            // A survivor continuing from this exact point (any contour).
            let (kx, ky) = cell_of(end, eps);
            let mut next: Option<usize> = None;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    // Saturating: a coordinate far enough out of range that
                    // its cell index pins to the end of `i64` would
                    // otherwise overflow while stepping to the neighbour.
                    for k in starts.get((kx.saturating_add(dx), ky.saturating_add(dy))) {
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
                let home = (first_pt - end).hypot();
                next = near.nearest(end, used, home);
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
        if !substantial_loop(path, accuracy) {
            loops.pop();
        }
    }
}

/// The cell of side `cell` that `p` falls in. Indices beyond the range of
/// `i64` saturate at its ends.
fn cell_of(p: Point, cell: f64) -> (i64, i64) {
    (
        math::floor(p.x / cell) as i64,
        math::floor(p.y / cell) as i64,
    )
}

/// Pieces keyed by the grid cell their start point falls in, for the
/// neighbourhood queries of [`stitch`].
///
/// The entries are sorted, so a cell's pieces form one run in ascending
/// piece order, which a binary search finds. This does the job of a map
/// from cell to piece list with a single sort. A map spent an allocation
/// on every occupied cell, which was most of the cost of stitching a small
/// outline.
#[derive(Default, Debug)]
struct CellIndex {
    entries: Vec<((i64, i64), u32)>,
}

impl CellIndex {
    /// List the start of every surviving piece of `kept` by its cell of
    /// side `cell`. Keeps the storage.
    fn rebuild(&mut self, kept: &[Option<(usize, PathSeg)>], cell: f64) {
        self.entries.clear();
        self.entries.extend(
            kept.iter()
                .enumerate()
                .filter_map(|(i, k)| k.map(|(_, seg)| (cell_of(seg.start(), cell), i as u32))),
        );
        self.entries.sort_unstable();
    }

    /// The pieces in `cell`, in ascending order.
    fn get(&self, cell: (i64, i64)) -> impl Iterator<Item = u32> + '_ {
        let from = self.entries.partition_point(|&(c, _)| c < cell);
        self.entries[from..]
            .iter()
            .take_while(move |&&(c, _)| c == cell)
            .map(|&(_, k)| k)
    }
}

/// Working storage of [`stitch`]: the survivors' starts by match cell and
/// by bridge cell, and which survivors the walk has used.
#[derive(Default, Debug)]
struct StitchWork {
    starts: CellIndex,
    bridge_cells: CellIndex,
    used: Vec<bool>,
}

/// The bridge lookup of [`stitch`]. It keeps the start points of the
/// surviving pieces in square cells, and searches them ring by ring
/// outwards from a point for the nearest.
struct NearestStart<'a> {
    kept: &'a [Option<(usize, PathSeg)>],
    cells: &'a CellIndex,
    cell: f64,
    /// How many rings out the search goes to reach every start within the
    /// bridge limit.
    rings: i64,
}

impl<'a> NearestStart<'a> {
    /// A search of the starts of `kept`, which `cells` lists by cell of side
    /// `cell`, for bridges up to `reach` long.
    fn new(
        kept: &'a [Option<(usize, PathSeg)>],
        cells: &'a CellIndex,
        cell: f64,
        reach: f64,
    ) -> NearestStart<'a> {
        NearestStart {
            kept,
            cells,
            cell,
            rings: math::ceil(reach / cell) as i64 + 1,
        }
    }

    /// The unused survivor that starts nearest `end`, the lowest index among
    /// equals, if one lies nearer than `limit` and within the reach the
    /// index was built for.
    ///
    /// Far from the origin the cell indices round by whole cells. There the
    /// first stop in the loop can settle on a start that is not quite the
    /// nearest, as the search always could.
    fn nearest(&self, end: Point, used: &[bool], limit: f64) -> Option<usize> {
        let (kx, ky) = cell_of(end, self.cell);
        // A start's ring can overstate its distance in two ways. `floor`
        // can put two points anywhere within their cells, which adds less
        // than a cell. Rounding the quotients `x / cell` adds up to `e`
        // cells more, which reaches whole cells about 2^52 cells from the
        // origin. The starts that matter lie within `limit` of `end`, which
        // also bounds their coordinates. So a start nearer than `limit` lies
        // in a ring below `limit / cell + 1 + e`. The stop below allows
        // `e2 = 2e`, and a spare cell.
        let e2 = 2.0 * f64::EPSILON * (end.x.abs().max(end.y.abs()) + limit) / self.cell;
        let mut best: Option<(f64, usize)> = None;
        for r in 0..=self.rings {
            // While the cell indices are exact, every start in ring `r` lies
            // at least `(r - 1)` cells away.
            if best.is_some_and(|(d, _)| d <= (r - 1) as f64 * self.cell) {
                break;
            }
            // Nor can a ring this far out hold a start nearer than `limit`.
            // The walk would close on its own start rather than bridge that
            // far, so the search stops. Without this stop, a walk ending a
            // little short of its start, with no unused start within the
            // bridge limit, would sweep every ring out to that limit.
            if (r as f64 - 2.0 - e2) * self.cell >= limit {
                break;
            }
            for dy in -r..=r {
                let step = if dy == -r || dy == r { 1 } else { 2 * r.max(1) };
                let mut dx = -r;
                while dx <= r {
                    // Saturating, as for continuations in `stitch`.
                    let cell = (kx.saturating_add(dx), ky.saturating_add(dy));
                    for k in self.cells.get(cell) {
                        let k = k as usize;
                        if used[k] {
                            continue;
                        }
                        let Some((_, s)) = self.kept[k] else { continue };
                        let d = (s.start() - end).hypot();
                        if best.is_none_or(|(b, bk)| d < b || (d == b && k < bk)) {
                            best = Some((d, k));
                        }
                    }
                    dx += step;
                }
            }
        }
        best.filter(|&(d, _)| d < limit).map(|(_, k)| k)
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

    /// A cell lists exactly the surviving pieces whose start falls in it,
    /// in ascending order, as the map of lists it replaced did. That
    /// includes keys at the ends of `i64`, where far-out coordinates
    /// saturate.
    #[test]
    fn cell_index_lists_each_cell_in_ascending_order() {
        let mut state = 0x1f83_d9ab_fb41_bd6b_u64;
        let mut rand = |m: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % m
        };
        let extremes = [i64::MIN, -1, 0, 1, i64::MAX];
        let coord = |k: i64| match k {
            i64::MIN => -1e300,
            i64::MAX => 1e300,
            k => k as f64 + 0.5,
        };
        let mut lists: alloc::collections::BTreeMap<(i64, i64), Vec<u32>> = Default::default();
        let kept: Vec<Option<(usize, PathSeg)>> = (0..400u32)
            .map(|i| {
                let key = if i % 9 == 0 {
                    (extremes[rand(5) as usize], extremes[rand(5) as usize])
                } else {
                    (rand(7) as i64 - 3, rand(7) as i64 - 3)
                };
                if i % 11 == 5 {
                    return None;
                }
                lists.entry(key).or_default().push(i);
                let p = Point::new(coord(key.0), coord(key.1));
                Some((
                    0,
                    PathSeg::Line(kurbo::Line::new(p, p + Vec2::new(0.25, 0.0))),
                ))
            })
            .collect();
        let mut cells = CellIndex::default();
        cells.rebuild(&kept, 1.0);
        for (key, want) in lists {
            assert_eq!(cells.get(key).collect::<Vec<_>>(), want, "{key:?}");
        }
        assert_eq!(cells.get((5, 5)).count(), 0);
        assert_eq!(cells.get((i64::MAX, i64::MIN + 1)).count(), 0);
    }

    fn loops_for(path: &BezPath, fill_side: StrokeSide, kind: RegionKind, w: f64) -> Vec<BezPath> {
        let index = SourceIndex::new(path, 1e-3, w);
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
        let mut work = RegionWork::default();
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
            &mut work,
        )
        .to_vec()
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
        let index = SourceIndex::new(&donut, 0.012, 1.0);
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

    /// What a distance query answered before the index sorted its bands by
    /// `x`. It tests every edge of every band from the band of `p.y - r` up
    /// to the band `p.y + r` rounds up into, with the bands laid out as they
    /// were then.
    fn band_scan(index: &EdgeIndex, p: Point, r_sq: f64, keep: impl Fn(u32, bool) -> bool) -> bool {
        let edges = &index.edges;
        let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
        for (a, b, _, _) in edges {
            ymin = ymin.min(a.y).min(b.y);
            ymax = ymax.max(a.y).max(b.y);
        }
        let n = (edges.len() / 4).clamp(1, 256);
        let inv_h = n as f64 / (ymax - ymin).max(1e-12);
        let band = |y: f64| (((y - ymin) * inv_h) as usize).min(n - 1);
        let r = math::sqrt(r_sq);
        let lo = math::floor((p.y - r - ymin) * inv_h).clamp(0.0, (n - 1) as f64) as usize;
        let hi = math::ceil((p.y + r - ymin) * inv_h).clamp(0.0, (n - 1) as f64) as usize;
        (0..edges.len()).any(|i| {
            let (a, b, tag, real) = edges[i];
            let (y0, y1) = y_range(a, b);
            band(y0) <= hi
                && band(y1) >= lo
                && keep(tag, real)
                && index.edge_within(i as u32, p, r_sq, 0)
        })
    }

    /// `x` moved by `k` units in the last place, towards larger magnitude
    /// for positive `k`.
    fn ulps(x: f64, k: i64) -> f64 {
        f64::from_bits((x.to_bits() as i64 + k) as u64)
    }

    /// The index answers distance queries exactly as the band scan it
    /// replaced. The queries use random points, with radii at random and at
    /// the exact distances of edges. Their spans also end at the height of
    /// every band boundary and an ulp either side. There, whether a band is
    /// searched changes what a test that rounds at the scale of the edge can
    /// find. Their windows also end at either end of an edge and an ulp
    /// either side, where the window skips edges. Box queries must meet
    /// exactly the boxes of all edges. All of it runs for indexes with no
    /// groups of bands, one group, several, and at the switch between none
    /// and some.
    #[test]
    fn distance_queries_match_the_band_scan() {
        let mut path = BezPath::new();
        path.extend(Rect::new(0.0, 0.0, 300.0, 200.0).to_path(1e-9).iter());
        path.extend(
            kurbo::Circle::new((90.0, 100.0), 60.0)
                .to_path(1e-3)
                .reverse_subpaths()
                .iter(),
        );
        for i in 0..40 {
            let a = core::f64::consts::TAU * i as f64 / 40.0;
            let r = if i % 2 == 0 { 70.0 } else { 25.0 };
            let p = (220.0 + r * a.cos(), 100.0 + r * a.sin());
            if i == 0 {
                path.move_to(p);
            } else {
                path.line_to(p);
            }
        }
        path.close_path();
        let (ymin, ymax) = (0.0, 200.0);
        let n = SourceIndex::new(&path, 0.01, 1.0).index.bands.len();
        // Coarse rows are built while `ceil(400 / radius) <= n / 4`.
        let switch = 2.0 * (ymax - ymin) / (n / 4) as f64;
        let mut groups = Vec::new();
        for radius in [0.5, 10.0, switch * 0.999, switch, 100.0, 1000.0] {
            let source = SourceIndex::new(&path, 0.01, radius);
            let index = &source.index;
            groups.push(index.coarse().map(|c| c.rows.buckets.len()));
            // Any run of fine bands, read through its groups and filtered,
            // yields exactly the edges the fine bands list.
            if let Some(coarse) = index.coarse() {
                for f0 in 0..n {
                    for f1 in f0..n {
                        let fine = f0..=f1;
                        let mut listed: Vec<u32> = fine
                            .clone()
                            .flat_map(|k| index.rows.buckets.get(k))
                            .copied()
                            .collect();
                        let mut read: Vec<u32> = coarse
                            .holding(&fine)
                            .flat_map(|k| coarse.rows.buckets.get(k))
                            .copied()
                            .filter(|&ix| coarse.lists(ix, &fine))
                            .collect();
                        for ids in [&mut listed, &mut read] {
                            ids.sort_unstable();
                            ids.dedup();
                        }
                        assert_eq!(listed, read, "radius {radius}, fine bands {fine:?}");
                    }
                }
            }
            let mut state = 0x2545_f491_4f6c_dd1d_u64 ^ radius.to_bits();
            let mut rand = || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 11) as f64 / (1u64 << 53) as f64
            };
            let mut queries = Vec::new();
            for k in 0..1500 {
                let p = Point::new(rand() * 340.0 - 20.0, rand() * 240.0 - 20.0);
                let r = if k % 2 == 0 {
                    rand() * 40.0
                } else {
                    let (a, b, _, _) = index.edges[k % index.edges.len()];
                    kurbo::Line::new(a, b).nearest(p, 1e-12).distance_sq.sqrt()
                };
                queries.push((p, r));
                // The same radius with the span ending at a band boundary.
                let edge = ymin + (k % n) as f64 * (ymax - ymin) / n as f64;
                // And with the window ending at either end of an edge.
                let (a, b, _, _) = index.edges[(k * 7) % index.edges.len()];
                let (x0, x1) = x_range(a, b);
                for nudge in [-1, 0, 1] {
                    queries.push((Point::new(p.x, ulps(edge - r, nudge)), r));
                    queries.push((Point::new(p.x, ulps(edge + r, nudge)), r));
                    queries.push((Point::new(ulps(x1 + r, nudge), p.y), r));
                    queries.push((Point::new(ulps(x0 - r, nudge), p.y), r));
                }
            }
            for &(p, r) in &queries {
                let r_sq = r * r;
                for contour in [None, Some(0), Some(2)] {
                    let keep =
                        |tag: u32, real: bool| real && contour.is_none_or(|c| c as u32 == tag);
                    assert_eq!(
                        index.within_where(p, r_sq, keep, 0),
                        band_scan(index, p, r_sq, keep),
                        "radius {radius}: {p:?} r {r}"
                    );
                }
                let rect = Rect::from_center_size(p, (r * 0.5, r * 0.1));
                let meets = index.edges.iter().any(|&(a, b, _, _)| {
                    let ((x0, x1), (y0, y1)) = (x_range(a, b), y_range(a, b));
                    x0 <= rect.x1 && x1 >= rect.x0 && y0 <= rect.y1 && y1 >= rect.y0
                });
                assert_eq!(
                    index.any_edge_box_meets(rect),
                    meets,
                    "radius {radius}: {rect:?}"
                );
            }
        }
        // No groups, none, none just below the switch, several from the
        // switch on, then one.
        assert_eq!(groups[..3], [None, None, None]);
        assert!(groups[3].is_some_and(|g| g > 1) && groups[4].is_some_and(|g| g > 1));
        assert_eq!(groups[5], Some(1));
    }

    /// A query read through groups skips the edges a group lists only from
    /// fine bands the query would not search. Here the edge is listed in
    /// band 1 alone, which shares a group with band 2, where the query's
    /// span begins. Rounding would accept the edge if it were tested.
    #[test]
    fn grouped_query_skips_bands_the_fine_query_does_not_search() {
        let ye = 0.5 - (2.0f64).powi(-54);
        let mut edges = alloc::vec![(Point::new(0.0, ye), Point::new(8.0, ye), 0, true)];
        let zigzag = |i: usize| Point::new(100.0 + i as f64, if i % 2 == 0 { 0.0 } else { 4.0 });
        edges.extend((0..63).map(|i| (zigzag(i), zigzag((i + 1) % 63), 1, true)));
        let mut index = EdgeIndex {
            edges,
            ..EdgeIndex::default()
        };
        index.reindex(2.0);
        assert_eq!(index.coarse().map(|c| c.group), Some(4));
        let r = math::sqrt(3.0);
        // The lowest height whose span starts in band 2.
        let mut y = 0.5 + r;
        while index.bands.of(y - r) < 2 {
            y = ulps(y, 1);
        }
        let p = Point::new(4.0, y);
        assert_eq!(index.bands.spanned((ye, ye)), 1..=1);
        assert_eq!(*index.bands.searched((p.y - r, p.y + r)).start(), 2);
        assert!(index.edge_within(0, p, 3.0, 0), "rounding accepts the edge");
        assert!(!index.within_where(p, 3.0, |_, real| real, 0));
        assert!(!band_scan(&index, p, 3.0, |_, real| real));
    }

    /// A query finds an edge that rounding puts within its radius even when
    /// its span ends just below the band that holds the edge, as the band
    /// scan did. The bands are 1/4 high from -1.75. The edge at
    /// `y = 1/4 - 2^-53` is listed in band 8 only, and the query's span ends
    /// at `1/4 - 2^-52`, in band 7.
    #[test]
    fn distance_query_searches_the_band_above_its_span() {
        let ye = 0.25 - (2.0f64).powi(-53);
        let mut edges = alloc::vec![
            (Point::new(0.0, ye), Point::new(10.0, ye), 0, true),
            (Point::new(10.0, ye), Point::new(10.0, 2.25), 0, true),
            (Point::new(10.0, 2.25), Point::new(0.0, 2.25), 0, true),
            (Point::new(0.0, 2.25), Point::new(0.0, ye), 0, true),
        ];
        let zigzag = |i: usize| Point::new(100.0 + i as f64, if i % 2 == 0 { -1.75 } else { 2.25 });
        edges.extend((0..60).map(|i| (zigzag(i), zigzag((i + 1) % 60), 1, true)));
        let mut index = EdgeIndex {
            edges,
            ..EdgeIndex::default()
        };
        index.reindex(1.0);
        let r = math::sqrt(3.0);
        let p = Point::new(0.0, 0.25 - (2.0f64).powi(-52) - r);
        let span = (p.y - r, p.y + r);
        assert_eq!(index.bands.of(ye), 8);
        assert!(!index.bands.spanned(span).contains(&8));
        assert!(index.within_where(p, 3.0, |_, real| real, 0));
        assert!(band_scan(&index, p, 3.0, |_, real| real));
    }

    /// Every point of a box that `is_box_clear` passes has the same winding
    /// and is clear at `margin`, as computed. Boxes lie at random around a
    /// star. Small boxes lie from a third of the margin to a few margins off
    /// the straight edges of a plus sign, where a smaller margin would let
    /// the nearer ones through. Two more cases round badly: a triangle with
    /// edges 2^44 long, whose cross products round far more coarsely than
    /// its boxes, and an edge of subnormal height, whose cross products
    /// underflow.
    #[test]
    fn a_clear_box_winds_the_same_and_is_clear_throughout() {
        fn check(index: &SourceIndex, rect: Rect, margin: f64) -> bool {
            if !index.is_box_clear(rect, margin) {
                return false;
            }
            let w = index.winding(rect.origin());
            for i in 0..=4 {
                for j in 0..=4 {
                    let (u, v) = (i as f64 / 4.0, j as f64 / 4.0);
                    let p = Point::new(rect.x0 + rect.width() * u, rect.y0 + rect.height() * v);
                    assert_eq!(index.winding(p), w, "{rect:?} at {p:?}");
                    assert!(
                        index.is_clear_of(p, margin * margin, None),
                        "{rect:?} at {p:?}"
                    );
                }
            }
            true
        }
        let mut star = BezPath::new();
        for i in 0..10 {
            let r = if i % 2 == 0 { 100.0 } else { 40.0 };
            let a = core::f64::consts::PI * i as f64 / 5.0;
            let p = (150.0 + r * a.cos(), 150.0 + r * a.sin());
            if i == 0 {
                star.move_to(p);
            } else {
                star.line_to(p);
            }
        }
        star.close_path();
        let margin = 0.25;
        let index = SourceIndex::new(&star, 0.01, margin);
        let mut state = 0x3c6e_f372_fe94_f82b_u64;
        let mut rand = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        let (mut clear, mut near_clear) = (0, 0);
        for _ in 0..2000 {
            let c = Point::new(40.0 + rand() * 220.0, 40.0 + rand() * 220.0);
            let size = 0.01 + rand() * 3.0;
            clear += usize::from(check(
                &index,
                Rect::from_center_size(c, (size, size)),
                margin,
            ));
        }
        // Off a straight edge only that edge's own box is near. A plus sign
        // has only straight edges.
        let mut plus = BezPath::new();
        let arms = [
            (40.0, 0.0),
            (80.0, 0.0),
            (80.0, 40.0),
            (120.0, 40.0),
            (120.0, 80.0),
            (80.0, 80.0),
            (80.0, 120.0),
            (40.0, 120.0),
            (40.0, 80.0),
            (0.0, 80.0),
            (0.0, 40.0),
            (40.0, 40.0),
        ];
        plus.move_to(arms[0]);
        for &p in &arms[1..] {
            plus.line_to(p);
        }
        plus.close_path();
        let index = SourceIndex::new(&plus, 0.01, margin);
        for k in 0..2000 {
            let (a, b, _, _) = index.index.edges[k % index.index.edges.len()];
            let normal = (b - a).turn_90().normalize();
            let off = margin * (0.3 + rand() * 2.2);
            let side = if k % 2 == 0 { 1.0 } else { -1.0 };
            let c = a.lerp(b, 0.2 + 0.6 * rand()) + normal * off * side;
            let size = margin * 0.05;
            if check(&index, Rect::from_center_size(c, (size, size)), margin) && off < 2.0 * margin
            {
                near_clear += 1;
            }
        }
        assert!(clear > 200 && clear < 1800, "{clear} of 2000 clear");
        assert!(near_clear > 50, "{near_clear} clear within two margins");
        // An edge 5e-324 high: cross products with it underflow to zero.
        let mut low = Rect::new(0.0, -10.0, 20.0, 10.0).to_path(1e-9);
        low.move_to((-1.7, 0.0));
        low.line_to((-0.7, 5e-324));
        low.line_to((-1.2, 1.0));
        low.close_path();
        let index = SourceIndex::new(&low, 0.0625, 0.25);
        check(&index, Rect::new(-2.0, -10.0, -2.0, 10.0), 0.25);
        let l = (2.0f64).powi(44);
        let mut sliver = BezPath::new();
        sliver.move_to((l, 3.0));
        sliver.line_to((0.0, 1.5));
        sliver.line_to((l, 0.0));
        sliver.close_path();
        let index = SourceIndex::new(&sliver, 0.01, 1e-4);
        for x in [-5e-4, -2e-4, -1.2e-4] {
            check(&index, Rect::new(x, 0.5, x, 2.5), 1e-4);
        }
    }

    /// Bounding the bridge search by the loop's own start changes no answer
    /// the walk keeps. The nearest start nearer than the limit is the same
    /// with and without the bound, for limits at random and at exactly the
    /// distance of a start. This runs near the origin, and far out at 5e16
    /// with cells 1 to 4 wide. There, dividing by the cell rounds the cell
    /// indices by several cells, the allowance for that outgrows the rings,
    /// and the bound must simply do no harm. Last comes a counterexample to
    /// a bound without the allowance.
    #[test]
    fn bounded_bridge_search_finds_what_the_full_search_does() {
        let mut state = 0xda94_2042_e4dd_58b5_u64;
        let mut rand = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        for (origin, rounds) in [(Point::ZERO, 60), (Point::new(5.4e16, -3.3e16), 30)] {
            for round in 0..rounds {
                let kept: Vec<Option<(usize, PathSeg)>> = (0..80)
                    .map(|i| {
                        let a = origin + Vec2::new(rand() * 60.0, rand() * 60.0);
                        let b = a + Vec2::new(rand() - 0.5, rand() - 0.5);
                        (i % 7 != 3).then_some((0, PathSeg::Line(kurbo::Line::new(a, b))))
                    })
                    .collect();
                let used: Vec<bool> = (0..kept.len()).map(|_| rand() < 0.3).collect();
                let cell = 1.0 + round as f64 * 0.1;
                let mut cells = CellIndex::default();
                cells.rebuild(&kept, cell);
                let near = NearestStart::new(&kept, &cells, cell, 8.0);
                let distance = |k: usize, end: Point| (kept[k].unwrap().1.start() - end).hypot();
                for k in 0..200 {
                    let end = origin + Vec2::new(rand() * 70.0 - 5.0, rand() * 70.0 - 5.0);
                    let limit = match (k % 3, kept[k % kept.len()]) {
                        (0, Some((_, s))) => (s.start() - end).hypot(),
                        _ => rand() * 20.0,
                    };
                    let unbounded = near
                        .nearest(end, &used, f64::INFINITY)
                        .filter(|&k| distance(k, end) < limit);
                    assert_eq!(
                        near.nearest(end, &used, limit),
                        unbounded,
                        "round {round}, {end:?}, limit {limit}"
                    );
                }
            }
        }
        // At 3 * 2^54 + 16 and + 32 the points lie 16 apart, 5.3 cells, but
        // their cell indices round to 8 cells apart.
        let end = Point::new(54043195528445968.0, 0.0);
        let start = end + Vec2::new(16.0, 0.0);
        let kept = [Some((
            0,
            PathSeg::Line(kurbo::Line::new(start, start + Vec2::new(0.0, 17.0))),
        ))];
        let mut cells = CellIndex::default();
        cells.rebuild(&kept, 3.0);
        let near = NearestStart::new(&kept, &cells, 3.0, 24.0);
        assert_eq!(near.nearest(end, &[false], 17.0), Some(0));
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
