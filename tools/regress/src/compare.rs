// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The geometric comparator. It decides whether two fill outlines paint
//! the same region under the nonzero winding rule, to within a distance
//! `δ`.
//!
//! An outline is what `stroke_aligned` returns. It overlaps itself by
//! design, so its edges say little. What matters is the region it paints,
//! `{p : winding(p) ≠ 0}`, with open subpaths closed as filling closes
//! them. Two outlines are equivalent when both of these hold, up to the
//! resolution below:
//!
//! 1. Every point on the boundary of either region lies within `δ` of the
//!    other region's boundary. That is a Hausdorff distance `H ≤ δ`.
//! 2. Every point painted by exactly one of them lies within `δ` of both
//!    boundaries.
//!
//! The second condition catches what the first misses. A filled disc and a
//! thin ring with the same outer edge have boundaries within the ring's
//! thickness of each other, but they differ by the whole hole.
//!
//! # Resolution
//!
//! Stroke outlines are full of features far thinner than the tolerance:
//! a curve traversed forward and straight back again, an offset that runs
//! back along another a hundredth of a unit away, the sliver where a
//! near-straight cubic meets the line it cancels, the tip of a wedge
//! between two edges that cross at a grazing angle. Whether such a sliver
//! is open or closed, and where exactly a grazing crossing lands, depends
//! on rounding and on how a curve is flattened. So it cannot be part of
//! the contract.
//!
//! Both conditions therefore also accept a point that lies within the
//! resolution `ρ = tol / 2` of an edge of both outlines. That is where such
//! slivers live. Any edge counts here, boundary or not. A thin feature may
//! open, close or shift where the other outline has structure. But a
//! feature that moves away from the other outline's edges, or vanishes,
//! still fails.
//!
//! When exactly one region is empty, the other must be a degenerate
//! sliver: its mean thickness must be below `tol / 64`. The mean thickness
//! is twice the area over the boundary length.
//!
//! # Method
//!
//! Both outlines are flattened into polygons at `tol / 16`. Each polygon
//! edge is split wherever another edge of the same outline crosses or
//! touches it. It is also split wherever another edge crosses the lines
//! that the side probes below travel along. Between those points the
//! winding numbers on either side are constant. So two probes along the
//! normal at the middle of each piece, at least `1e-7 · extent` away,
//! classify it exactly: it is boundary when the region indicator differs
//! across it. Edges inside an overlap, between winding 1 and 2, are never
//! boundary, and neither are coincident opposite edges.
//!
//! Boundary pieces and all edges are bucketed in uniform grids. The
//! directed distance from one boundary to the other is computed per piece,
//! without sampling. The points of a segment within `r` of another segment
//! form one interval, because a line meets a capsule in a convex set. So
//! whether every point of piece `a` lies within `r` of the other boundary,
//! or within `ρ` of the other outline's edges, is an interval cover test.
//! Bisection on `r` then brackets the distance. `H` is reported as an
//! upper bound, at most `δ / 128` above the true value for the flattened
//! boundaries. A case near `δ` is decided by an exact cover test at
//! `r = δ`.
//!
//! Condition 2 is checked on a grid of spacing `max(extent / 1024, δ / 10)`,
//! classified one scanline at a time. It is also checked at probe points
//! beside every boundary piece, just off it and `1.5 ρ` off it on each
//! side. These catch thin features and narrow differences where the grid
//! misses them. The mismatched grid points give the estimate of the
//! symmetric difference. Region areas come from the boundary itself, as
//! `∮ x dy` over the classified pieces. That stays accurate for thin
//! strokes that a grid would miss.

use kurbo::{BezPath, PathEl, Point, Rect, Vec2};

/// What the comparison tolerates: boundaries up to `delta()` apart.
#[derive(Clone, Copy, Debug)]
pub struct Contract {
    /// The tolerance the stroke was computed at.
    pub tol: f64,
    /// `δ = delta_scale · tol`.
    pub delta_scale: f64,
}

impl Contract {
    /// The allowed boundary distance `δ`.
    pub fn delta(&self) -> f64 {
        self.tol * self.delta_scale
    }
}

/// The outcome class of one comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Verdict {
    /// Outlines with the same elements, bit for bit.
    Identical,
    /// Different outlines that paint the same region, within `δ`.
    Equivalent,
    /// Anything else, including non-finite output and panics.
    Fail,
}

impl Verdict {
    /// The upper-case label used in reports.
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Identical => "IDENTICAL",
            Verdict::Equivalent => "EQUIVALENT",
            Verdict::Fail => "FAIL",
        }
    }
}

/// The result of comparing a candidate outline with the reference.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// The verdict.
    pub verdict: Verdict,
    /// Upper bound on the boundary distance `H`, in path units. It is `0`
    /// for identical outlines, and `NaN` when it could not be measured:
    /// after a panic, or with one region empty. When `capped`, it is a
    /// lower bound instead: the true distance is at least this.
    pub hausdorff: f64,
    /// Whether `hausdorff` was cut short at the search cap of `16 δ`.
    pub capped: bool,
    /// Estimated area painted by exactly one outline. `NaN` when not
    /// computed, as for identical outlines.
    pub symdiff: f64,
    /// Area painted by the reference, or `NaN` when not computed.
    pub area_ref: f64,
    /// Area painted by the candidate, or `NaN` when not computed.
    pub area_cand: f64,
    /// Why the case failed, or a note on a pass with an empty region.
    pub reason: String,
    /// Where the outlines differ most: the worst boundary deviation, or
    /// the first grid point painted by one outline far from both
    /// boundaries.
    pub at: Option<Point>,
}

impl Outcome {
    fn new(verdict: Verdict) -> Self {
        Outcome {
            verdict,
            hausdorff: f64::NAN,
            capped: false,
            symdiff: f64::NAN,
            area_ref: f64::NAN,
            area_cand: f64::NAN,
            reason: String::new(),
            at: None,
        }
    }

    /// A failure that never reached the geometric check.
    pub fn failed(reason: impl Into<String>) -> Self {
        Outcome {
            reason: reason.into(),
            ..Outcome::new(Verdict::Fail)
        }
    }
}

/// Compare `candidate` against `reference`.
///
/// Bitwise identical outlines are `Identical` without further work.
/// `force_geometric` runs the geometric check on them anyway. That checks
/// the comparator itself, since identical outlines must pass it.
pub fn compare(
    reference: &BezPath,
    candidate: &BezPath,
    contract: Contract,
    force_geometric: bool,
) -> Outcome {
    let identical = bitwise_equal(reference, candidate);
    if identical && !force_geometric {
        return Outcome {
            hausdorff: 0.0,
            ..Outcome::new(Verdict::Identical)
        };
    }
    if !candidate.is_finite() {
        return Outcome::failed("candidate output is not finite");
    }
    if !reference.is_finite() {
        return Outcome::failed("reference output is not finite; cannot compare");
    }
    let mut outcome = compare_geometric(reference, candidate, contract);
    if identical {
        if outcome.verdict == Verdict::Fail {
            outcome.reason = format!("comparator rejected identical outlines: {}", outcome.reason);
        } else {
            outcome.verdict = Verdict::Identical;
        }
    }
    outcome
}

/// Whether two paths have the same elements with bitwise equal coordinates.
pub fn bitwise_equal(a: &BezPath, b: &BezPath) -> bool {
    fn bits(p: Point) -> (u64, u64) {
        (p.x.to_bits(), p.y.to_bits())
    }
    let (a, b) = (a.elements(), b.elements());
    a.len() == b.len()
        && a.iter().zip(b).all(|pair| match pair {
            (PathEl::MoveTo(p), PathEl::MoveTo(q)) | (PathEl::LineTo(p), PathEl::LineTo(q)) => {
                bits(*p) == bits(*q)
            }
            (PathEl::QuadTo(p1, p2), PathEl::QuadTo(q1, q2)) => {
                bits(*p1) == bits(*q1) && bits(*p2) == bits(*q2)
            }
            (PathEl::CurveTo(p1, p2, p3), PathEl::CurveTo(q1, q2, q3)) => {
                bits(*p1) == bits(*q1) && bits(*p2) == bits(*q2) && bits(*p3) == bits(*q3)
            }
            (PathEl::ClosePath, PathEl::ClosePath) => true,
            _ => false,
        })
}

/// The geometric check proper, for finite outlines.
pub fn compare_geometric(reference: &BezPath, candidate: &BezPath, contract: Contract) -> Outcome {
    let (tol, delta) = (contract.tol, contract.delta());
    assert!(
        delta.is_finite() && delta > 0.0 && tol > 0.0,
        "comparison needs a positive tolerance and delta"
    );
    let flat_tol = tol / 16.0;
    let edges_ref = flatten(reference, flat_tol);
    let edges_cand = flatten(candidate, flat_tol);
    let Some(frame) = Frame::new(&edges_ref, &edges_cand, tol, delta) else {
        // No edge of positive length on either side: both paint nothing.
        return Outcome {
            hausdorff: 0.0,
            symdiff: 0.0,
            area_ref: 0.0,
            area_cand: 0.0,
            ..Outcome::new(Verdict::Equivalent)
        };
    };
    let ref_region = Region::new(edges_ref, &frame);
    let cand_region = Region::new(edges_cand, &frame);
    let mut outcome = Outcome {
        area_ref: ref_region.area,
        area_cand: cand_region.area,
        ..Outcome::new(Verdict::Equivalent)
    };

    // An empty region has no boundary to measure against.
    match (ref_region.is_empty(), cand_region.is_empty()) {
        (true, true) => {
            outcome.hausdorff = 0.0;
            outcome.symdiff = 0.0;
            return outcome;
        }
        (false, false) => {}
        (ref_empty, _) => {
            let (empty, other, region) = if ref_empty {
                ("reference", "candidate", &cand_region)
            } else {
                ("candidate", "reference", &ref_region)
            };
            // A real stroke may not vanish, however thin. A sliver of
            // rounding may.
            let thickness = 2.0 * region.area / region.length;
            outcome.symdiff = region.area;
            if thickness < tol / 64.0 {
                outcome.reason = format!(
                    "{empty} region empty; {other} is a sliver (mean thickness {:.3e} tol)",
                    thickness / tol
                );
            } else {
                outcome.verdict = Verdict::Fail;
                outcome.reason = format!(
                    "{empty} region empty; {other} paints area {:.4} (mean thickness {:.3} tol)",
                    region.area,
                    thickness / tol
                );
            }
            return outcome;
        }
    }

    let ref_side = Side::new(&ref_region, &frame);
    let cand_side = Side::new(&cand_region, &frame);

    // Condition 1, both ways.
    let eps_h = delta / 128.0;
    let cap = 16.0 * delta;
    let ref_to_cand = directed(&ref_side.boundary.segs, &cand_side, &frame, eps_h, cap);
    let cand_to_ref = directed(&cand_side.boundary.segs, &ref_side, &frame, eps_h, cap);
    let lo = ref_to_cand.lo.max(cand_to_ref.lo);
    outcome.capped = ref_to_cand.capped || cand_to_ref.capped;
    outcome.hausdorff = if outcome.capped { lo } else { lo + eps_h };
    let boundary_ok = if outcome.capped || lo > delta {
        false
    } else if lo + eps_h <= delta {
        true
    } else {
        // Within the bisection's resolution of δ: decide exactly.
        covers_all(&ref_side.boundary.segs, &cand_side, &frame, delta)
            && covers_all(&cand_side.boundary.segs, &ref_side, &frame, delta)
    };

    // Condition 2.
    let grid = region_check(&ref_region, &cand_region, &ref_side, &cand_side, &frame);
    outcome.symdiff = grid.mismatches as f64 * grid.spacing * grid.spacing;

    let mut reasons = Vec::new();
    if !boundary_ok {
        let (worst, direction) = if ref_to_cand.lo >= cand_to_ref.lo {
            (&ref_to_cand, "reference boundary far from candidate's")
        } else {
            (&cand_to_ref, "candidate boundary far from reference's")
        };
        reasons.push(format!(
            "boundary distance H {} {:.3} tol > δ = {:.3} tol ({direction} near ({:.4}, {:.4}))",
            if outcome.capped { ">=" } else { "<=" },
            outcome.hausdorff / tol,
            delta / tol,
            worst.at.x,
            worst.at.y,
        ));
        outcome.at = Some(worst.at);
    }
    if let Some(at) = grid.first_bad {
        reasons.push(format!(
            "regions differ away from the boundaries: {}{} sample points (grid spacing {:.3e}), e.g. at ({:.4}, {:.4})",
            grid.bad,
            if grid.bad_truncated { "+" } else { "" },
            grid.spacing,
            at.x,
            at.y,
        ));
        outcome.at.get_or_insert(at);
    }
    if !reasons.is_empty() {
        outcome.verdict = Verdict::Fail;
        outcome.reason = reasons.join("; ");
    }
    outcome
}

// ---------------------------------------------------------------------------
// Polygons
// ---------------------------------------------------------------------------

/// A straight edge from `a` to `b`.
#[derive(Clone, Copy, Debug)]
struct Seg {
    a: Point,
    b: Point,
}

impl Seg {
    fn vec(&self) -> Vec2 {
        self.b - self.a
    }

    fn len(&self) -> f64 {
        self.vec().hypot()
    }

    fn at(&self, t: f64) -> Point {
        self.a + self.vec() * t
    }

    fn bbox(&self) -> Rect {
        Rect::from_points(self.a, self.b)
    }

    /// Squared distance from `p` to the segment.
    fn dist2(&self, p: Point) -> f64 {
        let d = self.vec();
        let dd = d.hypot2();
        let t = if dd > 0.0 {
            ((p - self.a).dot(d) / dd).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (p - self.at(t)).hypot2()
    }
}

/// Flatten `path` into closed polygon rings, as a list of edges. Every
/// subpath is closed, as filling closes it. Zero-length edges are dropped.
///
/// Curves are cut into equal parameter steps, as many as the classic bound
/// on interpolation error asks for. A polyline through `n` equal steps of
/// a curve stays within `max |B''| / (8 n²)` of it. `max |B''|` is at most
/// `6 · max |Δ²P|` for a cubic and `2 |Δ²P|` for a quad, where `Δ²P` are
/// the second differences of the control points.
///
/// `kurbo::flatten` is not used. In kurbo 0.13.1 it can stray far past its
/// tolerance on an S-shaped cubic, and by a different amount for the same
/// curve traversed the other way. The comparator then saw a reversed
/// outline as a different region.
fn flatten(path: &BezPath, flat_tol: f64) -> Vec<Seg> {
    let mut edges = Vec::new();
    let mut push = |a: Point, b: Point| {
        if a != b {
            edges.push(Seg { a, b });
        }
    };
    let mut start = None::<Point>;
    let mut cur = Point::ORIGIN;
    for el in path.iter() {
        match el {
            PathEl::MoveTo(p) => {
                if let Some(s) = start {
                    push(cur, s);
                }
                start = Some(p);
                cur = p;
            }
            PathEl::ClosePath => {
                if let Some(s) = start {
                    push(cur, s);
                    cur = s;
                }
            }
            _ => {
                let mut a = cur;
                polyline_to(cur, el, flat_tol, |b| {
                    push(a, b);
                    a = b;
                });
                cur = el.end_point().unwrap_or(cur);
            }
        }
    }
    if let Some(s) = start {
        push(cur, s);
    }
    edges
}

/// Call `f` with each vertex after `from` of a polyline within `flat_tol`
/// of the drawing element `el`, which starts at `from`. The last vertex is
/// exactly the element's end point. See [`flatten`] for the bound.
pub(crate) fn polyline_to(from: Point, el: PathEl, flat_tol: f64, mut f: impl FnMut(Point)) {
    /// At most this many steps per curve, so a pathological input cannot
    /// exhaust memory. At this count the bound may be exceeded, but not the
    /// memory.
    const MAX_STEPS: usize = 1 << 16;
    let steps = |second_diff: f64, factor: f64| -> usize {
        let n = (factor * second_diff / flat_tol).sqrt().ceil();
        if n.is_finite() {
            (n as usize).clamp(1, MAX_STEPS)
        } else {
            MAX_STEPS
        }
    };
    let (seg, n) = match el {
        PathEl::LineTo(p) => {
            f(p);
            return;
        }
        PathEl::QuadTo(p1, p2) => {
            let q = kurbo::QuadBez::new(from, p1, p2);
            let d = q.p0.to_vec2() - 2.0 * q.p1.to_vec2() + q.p2.to_vec2();
            (kurbo::PathSeg::Quad(q), steps(d.hypot(), 0.25))
        }
        PathEl::CurveTo(p1, p2, p3) => {
            let c = kurbo::CubicBez::new(from, p1, p2, p3);
            let d1 = c.p0.to_vec2() - 2.0 * c.p1.to_vec2() + c.p2.to_vec2();
            let d2 = c.p1.to_vec2() - 2.0 * c.p2.to_vec2() + c.p3.to_vec2();
            (kurbo::PathSeg::Cubic(c), steps(d1.hypot().max(d2.hypot()), 0.75))
        }
        PathEl::MoveTo(_) | PathEl::ClosePath => return,
    };
    for k in 1..n {
        f(kurbo::ParamCurve::eval(&seg, k as f64 / n as f64));
    }
    f(kurbo::ParamCurve::end(&seg));
}

/// Cut `seg` into equal chunks no longer than `max`, and call `f` with each.
fn for_each_chunk(seg: Seg, max: f64, mut f: impl FnMut(Seg)) {
    let n = ((seg.len() / max).ceil() as usize).max(1);
    let mut a = seg.a;
    for k in 1..=n {
        let b = if k == n {
            seg.b
        } else {
            seg.at(k as f64 / n as f64)
        };
        f(Seg { a, b });
        a = b;
    }
}

/// Scales shared by both sides of one comparison.
struct Frame {
    /// Bounding box of both outlines' polygons.
    bbox: Rect,
    /// Its larger side.
    extent: f64,
    /// How far off an edge the probes that classify a piece sit. It is on
    /// the scale of rounding.
    eps_strict: f64,
    /// The resolution `ρ`. A point within it of edges of both outlines is
    /// excused.
    resolution: f64,
    /// Distance at which two edges count as touching, and the shortest
    /// piece kept.
    geo_eps: f64,
    /// The allowed boundary distance.
    delta: f64,
    /// Grid cell for splitting edges at their crossings.
    split_cell: f64,
    /// Grid for the boundary distance queries.
    dist_grid: Grid,
}

impl Frame {
    fn new(a: &[Seg], b: &[Seg], tol: f64, delta: f64) -> Option<Self> {
        let mut pts = a.iter().chain(b).flat_map(|s| [s.a, s.b]);
        let first = pts.next()?;
        let bbox = pts.fold(Rect::from_points(first, first), |r, p| r.union_pt(p));
        let extent = bbox.width().max(bbox.height());
        let max_abs = [bbox.x0, bbox.x1, bbox.y0, bbox.y1]
            .iter()
            .fold(0.0_f64, |m, v| m.max(v.abs()));
        let eps_strict = (1e-7 * extent).max(1e-13 * max_abs).max(1e-9 * tol);
        let geo_eps = (1e-10 * extent)
            .max(1e-14 * max_abs)
            .max(f64::MIN_POSITIVE);
        Some(Frame {
            bbox,
            extent,
            eps_strict,
            resolution: 0.5 * tol,
            geo_eps,
            delta,
            // Never finer than the probe padding. A chunk's padded box then
            // meets at most four cells across, so the incidence list stays
            // linear in the edges. On a near-empty frame the tolerance floor
            // of `eps_strict` dwarfs the extent. A cell sized from the
            // extent alone spread each chunk over millions of cells.
            split_cell: (extent / 1024.0)
                .max(16.0 * geo_eps)
                .max(2.0 * eps_strict),
            dist_grid: Grid::new(bbox, (extent / 512.0).max(delta)),
        })
    }
}

/// A uniform grid of square cells over a box.
#[derive(Clone, Copy, Debug)]
struct Grid {
    x0: f64,
    y0: f64,
    cell: f64,
    nx: usize,
    ny: usize,
}

impl Grid {
    fn new(bbox: Rect, cell: f64) -> Self {
        Grid {
            x0: bbox.x0,
            y0: bbox.y0,
            cell,
            nx: (bbox.width() / cell) as usize + 1,
            ny: (bbox.height() / cell) as usize + 1,
        }
    }

    /// Cell coordinates of a point, unclamped. Far points saturate.
    fn coords(&self, p: Point) -> (isize, isize) {
        (
            ((p.x - self.x0) / self.cell).floor() as isize,
            ((p.y - self.y0) / self.cell).floor() as isize,
        )
    }

    /// The clamped cell ranges `(x0..=x1, y0..=y1)` overlapping `r`.
    fn range(&self, r: Rect) -> (usize, usize, usize, usize) {
        let (ix0, iy0) = self.coords(Point::new(r.x0, r.y0));
        let (ix1, iy1) = self.coords(Point::new(r.x1, r.y1));
        let cx = |i: isize| i.clamp(0, self.nx as isize - 1) as usize;
        let cy = |i: isize| i.clamp(0, self.ny as isize - 1) as usize;
        (cx(ix0), cx(ix1), cy(iy0), cy(iy1))
    }
}

// ---------------------------------------------------------------------------
// Regions: winding numbers and boundary extraction
// ---------------------------------------------------------------------------

/// The nonzero region of one flattened outline.
struct Region {
    edges: Vec<Seg>,
    bands: Bands,
    /// The region boundary: pieces of edges across which the region
    /// indicator changes.
    strict: Vec<Seg>,
    /// Area of the region.
    area: f64,
    /// Length of its boundary.
    length: f64,
}

/// Edges bucketed by horizontal bands, for winding numbers.
struct Bands {
    y0: f64,
    y1: f64,
    inv_height: f64,
    count: usize,
    starts: Vec<u32>,
    items: Vec<u32>,
}

impl Bands {
    fn new(edges: &[Seg]) -> Self {
        let sloped = || edges.iter().enumerate().filter(|(_, e)| e.a.y != e.b.y);
        let (mut y0, mut y1, mut n) = (f64::INFINITY, f64::NEG_INFINITY, 0usize);
        for (_, e) in sloped() {
            y0 = y0.min(e.a.y.min(e.b.y));
            y1 = y1.max(e.a.y.max(e.b.y));
            n += 1;
        }
        if n == 0 {
            return Bands {
                y0: 0.0,
                y1: -1.0,
                inv_height: 0.0,
                count: 0,
                starts: vec![0],
                items: Vec::new(),
            };
        }
        // About two edges per band, unless long edges spanning many bands
        // would make the index much larger than the edge list.
        let inv = |count: usize| {
            if y1 > y0 {
                count as f64 / (y1 - y0)
            } else {
                0.0
            }
        };
        let mut count = (n / 2).clamp(1, 1 << 14);
        let limit = (16 * n).max(1 << 20);
        while count > 1 {
            let k = inv(count);
            let total: usize = sloped()
                .map(|(_, e)| ((e.a.y - e.b.y).abs() * k) as usize + 2)
                .sum();
            if total <= limit {
                break;
            }
            count /= 2;
        }
        let inv_height = inv(count);
        let band = |y: f64| (((y - y0) * inv_height) as usize).min(count - 1);
        let mut starts = vec![0u32; count + 1];
        for (_, e) in sloped() {
            for b in band(e.a.y.min(e.b.y))..=band(e.a.y.max(e.b.y)) {
                starts[b + 1] += 1;
            }
        }
        for b in 0..count {
            starts[b + 1] += starts[b];
        }
        let mut fill = starts.clone();
        let mut items = vec![0u32; starts[count] as usize];
        for (i, e) in sloped() {
            for b in band(e.a.y.min(e.b.y))..=band(e.a.y.max(e.b.y)) {
                items[fill[b] as usize] = i as u32;
                fill[b] += 1;
            }
        }
        Bands {
            y0,
            y1,
            inv_height,
            count,
            starts,
            items,
        }
    }

    /// The edges whose vertical extent overlaps the band containing `y`.
    fn candidates(&self, y: f64) -> &[u32] {
        if self.count == 0 || !(y >= self.y0 && y <= self.y1) {
            return &[];
        }
        let b = (((y - self.y0) * self.inv_height) as usize).min(self.count - 1);
        &self.items[self.starts[b] as usize..self.starts[b + 1] as usize]
    }
}

/// Where edge `e` crosses the horizontal line at `y`, with the edge's
/// direction. The test is half-open in `y`, so a vertex on the line counts
/// once.
fn crossing(e: &Seg, y: f64) -> Option<(f64, i32)> {
    if (e.a.y <= y) == (e.b.y <= y) {
        return None;
    }
    let x = e.a.x + (y - e.a.y) * (e.b.x - e.a.x) / (e.b.y - e.a.y);
    Some((x, if e.b.y > e.a.y { 1 } else { -1 }))
}

impl Region {
    fn new(edges: Vec<Seg>, frame: &Frame) -> Self {
        let bands = Bands::new(&edges);
        let mut region = Region {
            edges,
            bands,
            strict: Vec::new(),
            area: 0.0,
            length: 0.0,
        };
        region.extract_boundary(frame);
        region
    }

    fn is_empty(&self) -> bool {
        self.strict.is_empty()
    }

    /// The winding number at `p`, counted along a ray to `+x`.
    fn winding(&self, p: Point) -> i32 {
        self.bands
            .candidates(p.y)
            .iter()
            .filter_map(|&i| crossing(&self.edges[i as usize], p.y))
            .filter(|&(x, _)| x > p.x)
            .map(|(_, dir)| dir)
            .sum()
    }

    /// The sorted crossings of the horizontal line at `y`.
    fn crossings(&self, y: f64, out: &mut Vec<(f64, i32)>) {
        out.clear();
        out.extend(
            self.bands
                .candidates(y)
                .iter()
                .filter_map(|&i| crossing(&self.edges[i as usize], y)),
        );
        out.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    }

    /// Split every edge where another edge crosses or touches it, and
    /// classify each piece by probing both of its sides. Keep the pieces
    /// where the region indicator changes, and sum the region's area and
    /// boundary length over them.
    fn extract_boundary(&mut self, frame: &Frame) {
        let splits = split_params(&self.edges, frame);
        let origin = frame.bbox.center();
        let (mut area2, mut length) = (0.0, 0.0);
        let mut strict = Vec::new();
        let mut next = 0;
        let mut ts = Vec::new();
        for (i, e) in self.edges.iter().enumerate() {
            let len = e.len();
            ts.clear();
            ts.push(0.0);
            while next < splits.len() && splits[next].0 as usize == i {
                ts.push(splits[next].1);
                next += 1;
            }
            ts.push(1.0);
            let unit_normal = Vec2::new(-e.vec().y, e.vec().x) / len;
            for w in ts.windows(2) {
                let (ta, tb) = (w[0], w[1]);
                if (tb - ta) * len <= frame.geo_eps {
                    continue;
                }
                let m = e.at(0.5 * (ta + tb));
                let n = unit_normal * frame.eps_strict;
                let (wl, wr) = (self.winding(m + n), self.winding(m - n));
                let jump = i32::from(wl != 0) - i32::from(wr != 0);
                if jump == 0 {
                    continue;
                }
                // A boundary shared by `k` coincident edges counts `1/k`
                // for each of them: the weight is `jump / (wl - wr)`.
                let weight = f64::from(jump) / f64::from(wl - wr);
                let piece = Seg {
                    a: e.at(ta),
                    b: e.at(tb),
                };
                area2 += (piece.a - origin).cross(piece.b - origin) * weight;
                length += piece.len() * weight.abs();
                strict.push(piece);
            }
        }
        self.strict = strict;
        self.area = (0.5 * area2).max(0.0);
        self.length = length;
    }
}

/// One outline's spatial indexes for the distance queries.
struct Side {
    /// Its region boundary, chunked.
    boundary: SegIndex,
    /// All of its edges, chunked.
    edges: SegIndex,
}

impl Side {
    fn new(region: &Region, frame: &Frame) -> Self {
        Side {
            boundary: SegIndex::new(&region.strict, frame),
            edges: SegIndex::new(&region.edges, frame),
        }
    }
}

/// The parameters at which each edge must be split so that each piece has
/// one classification. They come as `(edge, t)`, sorted by edge and then
/// by `t`, with `t` strictly inside `(0, 1)`.
///
/// An edge is split where another edge crosses or touches it. It is also
/// split where another edge crosses one of the lines its side probes
/// travel along, which are the edge offset by `±eps_strict`. The winding
/// number at a probe changes exactly there, so classifying each piece at
/// its middle is exact. That holds even where two edges run within
/// `eps_strict` of each other or cross at a grazing angle.
///
/// Edges shorter than `ρ / 4` skip the probe lines. Misjudging part of one
/// moves a boundary by less than that. A tangle of short edges, as in a
/// collapsed arc, would otherwise split into quadratically many pieces.
fn split_params(edges: &[Seg], frame: &Frame) -> Vec<(u32, f64)> {
    let cell = frame.split_cell;
    let (x0, y0) = (frame.bbox.x0, frame.bbox.y0);
    let pad = 1.5 * frame.eps_strict;
    let key = |ix: f64, iy: f64| ((iy.max(0.0) as u64) << 32) | (ix.max(0.0) as u64);

    // Every (cell, edge) incidence. Candidate pairs are edges sharing a cell.
    let mut incidences: Vec<(u64, u32)> = Vec::with_capacity(edges.len() * 2);
    let mut keys = Vec::new();
    for (i, &e) in edges.iter().enumerate() {
        keys.clear();
        for_each_chunk(e, cell, |c| {
            let r = c.bbox().inflate(pad, pad);
            let (ix0, ix1) = (((r.x0 - x0) / cell).floor(), ((r.x1 - x0) / cell).floor());
            let (iy0, iy1) = (((r.y0 - y0) / cell).floor(), ((r.y1 - y0) / cell).floor());
            let mut iy = iy0;
            while iy <= iy1 {
                let mut ix = ix0;
                while ix <= ix1 {
                    keys.push(key(ix, iy));
                    ix += 1.0;
                }
                iy += 1.0;
            }
        });
        keys.sort_unstable();
        keys.dedup();
        incidences.extend(keys.iter().map(|&k| (k, i as u32)));
    }
    incidences.sort_unstable();

    let mut splits = Vec::new();
    for run in incidences.chunk_by(|a, b| a.0 == b.0) {
        for (n, &(_, i)) in run.iter().enumerate() {
            let s = edges[i as usize];
            let sb = s.bbox().inflate(pad, pad);
            for &(_, j) in &run[n + 1..] {
                let r = edges[j as usize];
                if !touches(sb, r.bbox()) {
                    continue;
                }
                intersect(s, r, frame.geo_eps, |ts, tr| {
                    if ts > 0.0 && ts < 1.0 {
                        splits.push((i, ts));
                    }
                    if tr > 0.0 && tr < 1.0 {
                        splits.push((j, tr));
                    }
                });
                for (a, b, k) in [(s, r, i), (r, s, j)] {
                    if a.len() < 0.25 * frame.resolution {
                        continue;
                    }
                    let n = Vec2::new(-a.vec().y, a.vec().x) * (frame.eps_strict / a.len());
                    for off in [n, -n] {
                        let probe_line = Seg {
                            a: a.a + off,
                            b: a.b + off,
                        };
                        intersect(probe_line, b, frame.geo_eps, |t, _| {
                            if t > 0.0 && t < 1.0 {
                                splits.push((k, t));
                            }
                        });
                    }
                }
            }
        }
    }
    splits.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    splits.dedup_by(|later, kept| later.0 == kept.0 && later.1 - kept.1 <= 1e-12);
    splits
}

/// Whether two boxes overlap or touch.
fn touches(a: Rect, b: Rect) -> bool {
    a.x0 <= b.x1 && b.x0 <= a.x1 && a.y0 <= b.y1 && b.y0 <= a.y1
}

/// Report where segments `s` and `r` cross or touch, as a parameter on
/// each. A parameter of `-1` means none on that segment. Collinear overlaps
/// report each segment's end points projected onto the other. Points
/// within `geo_eps` count as touching.
fn intersect(s: Seg, r: Seg, geo_eps: f64, mut emit: impl FnMut(f64, f64)) {
    let (d, e) = (s.vec(), r.vec());
    let w = r.a - s.a;
    let (ld, le) = (d.hypot(), e.hypot());
    let denom = d.cross(e);
    if denom.abs() > 1e-12 * ld * le {
        let ts = w.cross(e) / denom;
        let tr = w.cross(d) / denom;
        let (ms, mr) = (geo_eps / ld, geo_eps / le);
        if (-ms..=1.0 + ms).contains(&ts) && (-mr..=1.0 + mr).contains(&tr) {
            emit(ts.clamp(0.0, 1.0), tr.clamp(0.0, 1.0));
        }
        return;
    }
    // Parallel: only collinear overlaps matter.
    if w.cross(d).abs() / ld > geo_eps {
        return;
    }
    let (dd, ee) = (ld * ld, le * le);
    for t in [w.dot(d) / dd, (r.b - s.a).dot(d) / dd] {
        if t > 0.0 && t < 1.0 {
            emit(t, -1.0);
        }
    }
    for t in [(s.a - r.a).dot(e) / ee, (s.b - r.a).dot(e) / ee] {
        if t > 0.0 && t < 1.0 {
            emit(-1.0, t);
        }
    }
}

// ---------------------------------------------------------------------------
// Distance queries against a set of segments
// ---------------------------------------------------------------------------

/// Segments cut into chunks no longer than a grid cell and bucketed by
/// cell. Each chunk goes in every cell its bounding box overlaps.
struct SegIndex {
    grid: Grid,
    segs: Vec<Seg>,
    starts: Vec<u32>,
    items: Vec<u32>,
}

impl SegIndex {
    fn new<'a>(segments: impl IntoIterator<Item = &'a Seg>, frame: &Frame) -> Self {
        let grid = frame.dist_grid;
        let mut segs = Vec::new();
        for &s in segments {
            for_each_chunk(s, grid.cell, |c| segs.push(c));
        }
        let cells = grid.nx * grid.ny;
        let mut starts = vec![0u32; cells + 1];
        let ranges: Vec<_> = segs.iter().map(|s| grid.range(s.bbox())).collect();
        for &(x0, x1, y0, y1) in &ranges {
            for iy in y0..=y1 {
                for ix in x0..=x1 {
                    starts[iy * grid.nx + ix + 1] += 1;
                }
            }
        }
        for c in 0..cells {
            starts[c + 1] += starts[c];
        }
        let mut fill = starts.clone();
        let mut items = vec![0u32; starts[cells] as usize];
        for (i, &(x0, x1, y0, y1)) in ranges.iter().enumerate() {
            for iy in y0..=y1 {
                for ix in x0..=x1 {
                    let c = iy * grid.nx + ix;
                    items[fill[c] as usize] = i as u32;
                    fill[c] += 1;
                }
            }
        }
        SegIndex {
            grid,
            segs,
            starts,
            items,
        }
    }

    fn cell(&self, ix: usize, iy: usize) -> &[u32] {
        let c = iy * self.grid.nx + ix;
        &self.items[self.starts[c] as usize..self.starts[c + 1] as usize]
    }

    /// Distance from `p` to the nearest segment, or infinity when none lies
    /// within `rmax`.
    fn nearest(&self, p: Point, rmax: f64) -> f64 {
        if self.segs.is_empty() {
            return f64::INFINITY;
        }
        let g = &self.grid;
        let (cx, cy) = g.coords(p);
        let (nx, ny) = (g.nx as isize, g.ny as isize);
        let kmax = (rmax / g.cell).ceil() as isize + 1;
        let mut best = f64::INFINITY;
        let visit = |ix: isize, iy: isize, best: &mut f64| {
            if ix >= 0 && iy >= 0 && ix < nx && iy < ny {
                for &i in self.cell(ix as usize, iy as usize) {
                    *best = best.min(self.segs[i as usize].dist2(p));
                }
            }
        };
        for k in 0..=kmax {
            if k == 0 {
                visit(cx, cy, &mut best);
            } else {
                for ix in cx - k..=cx + k {
                    visit(ix, cy - k, &mut best);
                    visit(ix, cy + k, &mut best);
                }
                for iy in cy - k + 1..cy + k {
                    visit(cx - k, iy, &mut best);
                    visit(cx + k, iy, &mut best);
                }
            }
            // Everything within this distance of `p` has been seen.
            let seen = [
                p.x - (g.x0 + (cx - k) as f64 * g.cell),
                g.x0 + (cx + k + 1) as f64 * g.cell - p.x,
                p.y - (g.y0 + (cy - k) as f64 * g.cell),
                g.y0 + (cy + k + 1) as f64 * g.cell - p.y,
            ]
            .into_iter()
            .fold(f64::INFINITY, f64::min);
            let whole_grid = cx - k <= 0 && cy - k <= 0 && cx + k >= nx - 1 && cy + k >= ny - 1;
            if best <= seen * seen || seen >= rmax || whole_grid {
                break;
            }
        }
        let d = best.sqrt();
        if d <= rmax { d } else { f64::INFINITY }
    }

    /// Push the parameter interval of `a` within `r` of each segment that
    /// comes that close.
    fn intervals(&self, a: Seg, r: f64, out: &mut Vec<(f64, f64)>) {
        let (x0, x1, y0, y1) = self.grid.range(a.bbox().inflate(r, r));
        for iy in y0..=y1 {
            for ix in x0..=x1 {
                for &i in self.cell(ix, iy) {
                    if let Some(iv) = capsule_interval(a, self.segs[i as usize], r) {
                        out.push(iv);
                    }
                }
            }
        }
    }
}

/// Whether `intervals` cover `[0, 1]`.
fn covered(intervals: &mut [(f64, f64)]) -> bool {
    intervals.sort_unstable_by(|p, q| p.0.total_cmp(&q.0));
    // Rounding can open a gap between abutting intervals.
    let slack = 1e-9;
    let mut reach = 0.0;
    for &(s, e) in intervals.iter() {
        if s > reach + slack {
            return false;
        }
        reach = f64::max(reach, e);
        if reach >= 1.0 {
            return true;
        }
    }
    reach >= 1.0 - slack
}

/// The parameter interval of segment `a` within distance `r` of segment
/// `b`, clipped to `[0, 1]`. The `r`-neighbourhood of a segment is a
/// convex capsule, so this is one interval. It is the hull of where the
/// line meets the two end discs and the rectangle between them.
fn capsule_interval(a: Seg, b: Seg, r: f64) -> Option<(f64, f64)> {
    let d = a.vec();
    let dd = d.hypot2();
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for c in [b.a, b.b] {
        let f = a.a - c;
        let half_b = f.dot(d);
        let disc = half_b * half_b - dd * (f.hypot2() - r * r);
        if disc >= 0.0 {
            let sq = disc.sqrt();
            lo = lo.min((-half_b - sq) / dd);
            hi = hi.max((-half_b + sq) / dd);
        }
    }
    let v = b.vec();
    let lv = v.hypot();
    if lv > 0.0 {
        let u = v / lv;
        let n = Vec2::new(-u.y, u.x);
        let w = a.a - b.a;
        if let (Some(along), Some(across)) = (
            linear_interval(w.dot(u), d.dot(u), 0.0, lv),
            linear_interval(w.dot(n), d.dot(n), -r, r),
        ) {
            let (s, e) = (along.0.max(across.0), along.1.min(across.1));
            if s <= e {
                lo = lo.min(s);
                hi = hi.max(e);
            }
        }
    }
    let (lo, hi) = (lo.max(0.0), hi.min(1.0));
    (lo <= hi).then_some((lo, hi))
}

/// The `t` with `lo <= alpha + beta·t <= hi`.
fn linear_interval(alpha: f64, beta: f64, lo: f64, hi: f64) -> Option<(f64, f64)> {
    if beta == 0.0 {
        return (lo..=hi)
            .contains(&alpha)
            .then_some((f64::NEG_INFINITY, f64::INFINITY));
    }
    let (t0, t1) = ((lo - alpha) / beta, (hi - alpha) / beta);
    Some((t0.min(t1), t0.max(t1)))
}

/// A bracket on the directed distance from one boundary to another.
struct Directed {
    /// A lower bound on the distance. Unless `capped`, the true value is
    /// at most `lo + eps_h`.
    lo: f64,
    /// The distance reached the cap, and `lo` is the cap.
    capped: bool,
    /// Where `lo` was attained.
    at: Point,
}

/// Whether every point of chunk `a` lies within `r` of `to`'s boundary or
/// within the resolution of its edges. `excused` holds the intervals
/// within the resolution, computed once per chunk. `scratch` is working
/// space.
fn covers(
    a: Seg,
    r: f64,
    to: &Side,
    excused: &[(f64, f64)],
    scratch: &mut Vec<(f64, f64)>,
) -> bool {
    scratch.clear();
    scratch.extend_from_slice(excused);
    to.boundary.intervals(a, r, scratch);
    covered(scratch)
}

/// The directed boundary distance from the chunks `from` to the boundary
/// of `to`. Points within the resolution of `to`'s edges are ignored. The
/// distance is bracketed to within `eps_h` and searched up to `cap`.
///
/// Each chunk first raises the lower bound with the distances of its ends
/// and middle. If the chunk is then covered at `lo + eps_h`, it cannot
/// raise the maximum further. Otherwise bisection on the cover radius
/// brackets that chunk's own distance.
fn directed(from: &[Seg], to: &Side, frame: &Frame, eps_h: f64, cap: f64) -> Directed {
    let rho = frame.resolution;
    let mut out = Directed {
        lo: 0.0,
        capped: false,
        at: from.first().map_or(Point::ORIGIN, |s| s.a),
    };
    let (mut excused, mut scratch) = (Vec::new(), Vec::new());
    for &a in from {
        excused.clear();
        to.edges.intervals(a, rho, &mut excused);
        if covered(&mut excused) {
            continue;
        }
        let mid = a.at(0.5);
        let ends = [a.a, a.b, mid].map(|p| (p, to.boundary.nearest(p, cap)));
        for (p, d) in ends {
            if d > out.lo && to.edges.nearest(p, rho) > rho {
                out.lo = d;
                out.at = p;
            }
        }
        if out.lo > cap {
            return Directed {
                lo: cap,
                capped: true,
                at: out.at,
            };
        }
        // Every point of `a` is within half its length of an end.
        let mut hi = ends[0].1.max(ends[1].1) + 0.5 * a.len();
        if covers(a, out.lo + eps_h, to, &excused, &mut scratch) {
            continue;
        }
        if hi > cap {
            if !covers(a, cap, to, &excused, &mut scratch) {
                return Directed {
                    lo: cap,
                    capped: true,
                    at: mid,
                };
            }
            hi = cap;
        }
        let mut lo = out.lo + eps_h;
        while hi - lo > eps_h {
            let r = 0.5 * (lo + hi);
            if covers(a, r, to, &excused, &mut scratch) {
                hi = r;
            } else {
                lo = r;
            }
        }
        if lo > out.lo {
            out.lo = lo;
            out.at = mid;
        }
    }
    out
}

/// Whether every chunk of `from` lies within `r` of `to`'s boundary or
/// within the resolution of its edges.
fn covers_all(from: &[Seg], to: &Side, frame: &Frame, r: f64) -> bool {
    let (mut excused, mut scratch) = (Vec::new(), Vec::new());
    from.iter().all(|&a| {
        excused.clear();
        to.edges.intervals(a, frame.resolution, &mut excused);
        covers(a, r, to, &excused, &mut scratch)
    })
}

// ---------------------------------------------------------------------------
// Region difference on a grid
// ---------------------------------------------------------------------------

struct GridCheck {
    spacing: f64,
    /// Grid points painted by exactly one outline. Probes are not counted.
    mismatches: usize,
    /// Mismatched sample points, grid and probes alike, that are not
    /// excused.
    bad: usize,
    /// `bad` stopped counting at its limit.
    bad_truncated: bool,
    first_bad: Option<Point>,
}

/// Check condition 2 at the points of a grid over both outlines, and at
/// probe points beside every boundary chunk of either outline. The grid is
/// classified one scanline at a time. Every point painted by exactly one
/// outline must lie within `δ` of both boundaries, or within `ρ` of edges
/// of both outlines. A point of the second kind sits inside a feature
/// thinner than the resolution, where both outlines have structure.
fn region_check(a: &Region, b: &Region, sa: &Side, sb: &Side, frame: &Frame) -> GridCheck {
    /// Past this many unexcused mismatches the case has failed, so the
    /// count stops there.
    const BAD_LIMIT: usize = 1000;
    let spacing = (frame.extent / 1024.0).max(frame.delta / 10.0);
    let bbox = frame.bbox;
    let nx = ((bbox.width() / spacing).ceil() as usize).max(1);
    let ny = ((bbox.height() / spacing).ceil() as usize).max(1);
    let (rho, delta) = (frame.resolution, frame.delta);
    let mut out = GridCheck {
        spacing,
        mismatches: 0,
        bad: 0,
        bad_truncated: false,
        first_bad: None,
    };
    let mismatch = |p: Point, out: &mut GridCheck| {
        if out.bad >= BAD_LIMIT {
            out.bad_truncated = true;
            return;
        }
        let near_boundaries =
            sa.boundary.nearest(p, delta) <= delta && sb.boundary.nearest(p, delta) <= delta;
        let near_edges = sa.edges.nearest(p, rho) <= rho && sb.edges.nearest(p, rho) <= rho;
        if !near_boundaries && !near_edges {
            out.bad += 1;
            out.first_bad.get_or_insert(p);
        }
    };
    let (mut ca, mut cb) = (Vec::new(), Vec::new());
    for j in 0..ny {
        let y = bbox.y0 + (j as f64 + 0.5) * spacing;
        a.crossings(y, &mut ca);
        b.crossings(y, &mut cb);
        if ca.is_empty() && cb.is_empty() {
            continue;
        }
        let (mut ka, mut kb, mut wa, mut wb) = (0, 0, 0, 0);
        for i in 0..nx {
            let x = bbox.x0 + (i as f64 + 0.5) * spacing;
            while ka < ca.len() && ca[ka].0 < x {
                wa += ca[ka].1;
                ka += 1;
            }
            while kb < cb.len() && cb[kb].0 < x {
                wb += cb[kb].1;
                kb += 1;
            }
            if (wa != 0) != (wb != 0) {
                out.mismatches += 1;
                mismatch(Point::new(x, y), &mut out);
            }
        }
    }
    for s in sa.boundary.segs.iter().chain(&sb.boundary.segs) {
        let m = s.at(0.5);
        let n = Vec2::new(-s.vec().y, s.vec().x) / s.len();
        for off in [2.0 * frame.eps_strict, 1.5 * rho] {
            for p in [m + n * off, m - n * off] {
                if (a.winding(p) != 0) != (b.winding(p) != 0) {
                    mismatch(p, &mut out);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    //! Synthetic perturbations of real outlines that the comparator must
    //! accept, and ones it must reject.

    use super::*;
    use crate::perturb;
    use kurbo::{Circle, Rect, Shape};
    use kurbo_se::{Cap, DashStyle, Join, StrokeAlignment, StrokeStyle, stroke_aligned};

    const TOL: f64 = 0.25;

    fn contract() -> Contract {
        Contract {
            tol: TOL,
            delta_scale: 1.0,
        }
    }

    fn star() -> BezPath {
        let mut p = BezPath::new();
        for i in 0..10 {
            let r = if i % 2 == 0 { 100.0 } else { 40.0 };
            let a = core::f64::consts::PI * f64::from(i) / 5.0;
            let pt = (150.0 + r * a.cos(), 150.0 + r * a.sin());
            if i == 0 {
                p.move_to(pt);
            } else {
                p.line_to(pt);
            }
        }
        p.close_path();
        p
    }

    fn wavy() -> BezPath {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        for i in 0..12 {
            let x = f64::from(i) * 10.0;
            p.curve_to((x + 3.0, 15.0), (x + 7.0, -15.0), (x + 10.0, 0.0));
        }
        p
    }

    /// Real stroke outlines with curves, overlaps and several loops. The
    /// dashed one comes last, for `removing_one_dash_fails`.
    fn outlines() -> Vec<(&'static str, BezPath)> {
        let circle = Circle::new((100.0, 100.0), 60.0).to_path(1e-6);
        vec![
            (
                "star inside",
                stroke_aligned(
                    star(),
                    &StrokeStyle::new(12.0).with_alignment(StrokeAlignment::Inside),
                    TOL,
                ),
            ),
            (
                "circle center round",
                stroke_aligned(&circle, &StrokeStyle::new(10.0).with_join(Join::Round), TOL),
            ),
            (
                "wavy center caps",
                stroke_aligned(
                    wavy(),
                    &StrokeStyle::new(6.0)
                        .with_join(Join::Round)
                        .with_start_cap(Cap::Round)
                        .with_end_cap(Cap::Square),
                    TOL,
                ),
            ),
            (
                "circle dashed",
                stroke_aligned(
                    &circle,
                    &StrokeStyle::new(4.0).with_dash(DashStyle::new(20.0, 29.0).with_cap(Cap::Round)),
                    TOL,
                ),
            ),
        ]
    }

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> BezPath {
        Rect::new(x0, y0, x1, y1).to_path(1e-9)
    }

    #[track_caller]
    fn assert_pass(name: &str, a: &BezPath, b: &BezPath) -> Outcome {
        let out = compare(a, b, contract(), false);
        assert_ne!(
            out.verdict,
            Verdict::Fail,
            "{name}: expected a pass, got {:?} (H = {:.3} tol): {}",
            out.verdict,
            out.hausdorff / TOL,
            out.reason
        );
        out
    }

    #[track_caller]
    fn assert_fail(name: &str, a: &BezPath, b: &BezPath) -> Outcome {
        let out = compare(a, b, contract(), false);
        assert_eq!(
            out.verdict,
            Verdict::Fail,
            "{name}: expected a failure, got {:?} (H = {:.3} tol, symdiff {:.3})",
            out.verdict,
            out.hausdorff / TOL,
            out.symdiff
        );
        out
    }

    #[test]
    fn identical_outlines_are_identical_even_through_the_geometric_check() {
        for (name, a) in outlines() {
            assert_eq!(
                compare(&a, &a.clone(), contract(), false).verdict,
                Verdict::Identical
            );
            let forced = compare(&a, &a.clone(), contract(), true);
            assert_eq!(forced.verdict, Verdict::Identical, "{name}: {}", forced.reason);
            assert!(forced.hausdorff <= TOL / 64.0, "{name}: H = {}", forced.hausdorff);
            assert!(forced.symdiff == 0.0, "{name}: symdiff {}", forced.symdiff);
        }
    }

    #[test]
    fn reordered_subpaths_pass() {
        for (name, a) in outlines() {
            assert_pass(name, &a, &perturb::reorder_subpaths(&a));
        }
    }

    #[test]
    fn reversed_subpaths_pass() {
        for (name, a) in outlines() {
            assert_pass(name, &a, &a.reverse_subpaths());
        }
    }

    #[test]
    fn split_cubics_pass() {
        let mut split = 0;
        for (name, a) in outlines() {
            let b = perturb::split_cubics(&a);
            split += usize::from(!bitwise_equal(&a, &b));
            assert_pass(name, &a, &b);
        }
        assert!(split >= 3, "only {split} outlines had cubics to split");
    }

    #[test]
    fn degenerate_loops_pass() {
        for (name, a) in outlines() {
            assert_pass(name, &a, &perturb::add_degenerate_loops(&a));
        }
    }

    #[test]
    fn jitter_within_a_tenth_of_tol_passes() {
        // A shift below the resolution `ρ = tol / 2` is excused outright,
        // so the distance is not measured here. `translation_sensitivity`
        // measures it.
        for (name, a) in outlines() {
            assert_pass(name, &a, &perturb::jitter(&a, TOL / 10.0, 7));
        }
    }

    /// The edge of the contract. A shift of the whole outline well inside
    /// `δ` passes, and one a quarter past it fails. The shift in between is
    /// measured, not excused.
    #[test]
    fn translation_sensitivity() {
        for (name, a) in outlines() {
            assert_pass(name, &a, &perturb::translate(&a, Vec2::new(0.4 * TOL, 0.0)));
            let out = assert_pass(name, &a, &perturb::translate(&a, Vec2::new(0.0, 0.8 * TOL)));
            assert!(
                (0.7..0.9).contains(&(out.hausdorff / TOL)),
                "{name}: a shift of 0.8 tol measured as H = {} tol",
                out.hausdorff / TOL
            );
            assert_fail(name, &a, &perturb::translate(&a, Vec2::new(1.25 * TOL, 0.0)));
        }
    }

    #[test]
    fn reflattened_curves_pass() {
        for (name, a) in outlines() {
            assert_pass(name, &a, &perturb::reflatten(&a, TOL / 20.0));
        }
    }

    #[test]
    fn overlapping_contours_match_their_union() {
        // Two overlapping squares wound the same way paint their union. The
        // edges inside the overlap separate winding 1 from 2, so they are
        // not region boundary.
        let mut a = square(0.0, 0.0, 100.0, 100.0);
        a.extend(&square(50.0, 50.0, 150.0, 150.0));
        let mut b = BezPath::new();
        b.move_to((0.0, 0.0));
        for p in [
            (100.0, 0.0),
            (100.0, 50.0),
            (150.0, 50.0),
            (150.0, 150.0),
            (50.0, 150.0),
            (50.0, 100.0),
            (0.0, 100.0),
        ] {
            b.line_to(p);
        }
        b.close_path();
        let out = assert_pass("union", &a, &b);
        assert!((out.area_ref - 17500.0).abs() < 1e-6, "area {}", out.area_ref);
        assert!((out.area_cand - 17500.0).abs() < 1e-6, "area {}", out.area_cand);
        // Nested same-direction squares paint the outer one.
        let mut nested = square(0.0, 0.0, 100.0, 100.0);
        nested.extend(&square(30.0, 30.0, 70.0, 70.0));
        assert_pass("nested", &square(0.0, 0.0, 100.0, 100.0), &nested);
    }

    /// Reference outlines contain zero-area spikes, where a curve runs
    /// forward and back along a line. Splitting the curve moves its
    /// flattening off the line and opens a sliver. That is noise, so it
    /// must pass.
    #[test]
    fn a_cancelling_spike_flattened_differently_passes() {
        let mut a = square(0.0, 0.0, 100.0, 100.0);
        a.move_to((20.0, 50.0));
        a.line_to((80.0, 50.0));
        a.curve_to((60.0, 50.01), (40.0, 50.01), (20.0, 50.0));
        a.close_path();
        assert_pass("spike", &a, &perturb::split_cubics(&a));
    }

    /// A crack thinner than the resolution may open between pieces that
    /// abut, as dashes do across a zero gap. A wider one may not. No crack
    /// may open where the other outline has no edge at all.
    #[test]
    fn cracks_open_only_below_the_resolution_and_at_edges() {
        let cracked = |gap: f64| {
            let mut p = square(0.0, 0.0, 50.0 - gap / 2.0, 100.0);
            p.extend(&square(50.0 + gap / 2.0, 0.0, 100.0, 100.0));
            p
        };
        let abutting = cracked(0.0);
        let whole = square(0.0, 0.0, 100.0, 100.0);
        assert_pass("thin crack", &abutting, &cracked(0.4 * TOL));
        assert_pass("thin crack, reversed roles", &cracked(0.4 * TOL), &abutting);
        assert_fail("wide crack", &abutting, &cracked(2.5 * TOL));
        assert_fail("wide crack, reversed roles", &cracked(2.5 * TOL), &abutting);
        assert_fail("crack without an edge", &whole, &cracked(0.4 * TOL));
    }

    /// A band thinner than the resolution is below the boundary check. It
    /// still may not move away or vanish.
    #[test]
    fn a_thin_band_may_not_move_or_vanish() {
        let band = stroke_aligned(
            Circle::new((100.0, 100.0), 60.0).to_path(1e-6),
            &StrokeStyle::new(0.05),
            TOL,
        );
        assert_fail(
            "moved",
            &band,
            &perturb::translate(&band, Vec2::new(3.0 * TOL, 0.0)),
        );
        assert_fail("vanished", &band, &BezPath::new());
        assert_pass("jittered", &band, &perturb::jitter(&band, TOL / 10.0, 3));
    }

    #[test]
    fn translation_by_three_tol_fails() {
        for (name, a) in outlines() {
            let b = perturb::translate(&a, Vec2::new(3.0 * TOL, 0.0));
            let out = assert_fail(name, &a, &b);
            assert!(
                (2.9..3.1).contains(&(out.hausdorff / TOL)),
                "{name}: H = {} tol",
                out.hausdorff / TOL
            );
        }
    }

    #[test]
    fn dropping_a_loop_fails() {
        for (name, a) in outlines() {
            let subpaths = perturb::subpaths(&a);
            if subpaths.len() < 2 {
                continue;
            }
            for drop in [0, subpaths.len() - 1] {
                assert_fail(name, &a, &perturb::without_subpath(&a, drop));
            }
        }
    }

    #[test]
    fn reversing_only_the_hole_fails() {
        let outer = Circle::new((130.0, 130.0), 100.0).to_path(1e-6);
        let inner = Circle::new((130.0, 130.0), 45.0).to_path(1e-6);
        let mut donut = outer.clone();
        donut.extend(&inner.reverse_subpaths());
        let mut filled = outer;
        filled.extend(&inner);
        assert_fail("donut", &donut, &filled);

        // The same on a stroke outline: an annulus, outer loop and hole.
        let ring = stroke_aligned(
            Circle::new((100.0, 100.0), 60.0).to_path(1e-6),
            &StrokeStyle::new(20.0).with_join(Join::Round),
            TOL,
        );
        let subpaths = perturb::subpaths(&ring);
        assert!(subpaths.len() >= 2, "expected an outer loop and a hole");
        let hole = (0..subpaths.len())
            .min_by(|&i, &j| subpaths[i].area().abs().total_cmp(&subpaths[j].area().abs()))
            .unwrap();
        assert_fail("ring", &ring, &perturb::reverse_subpath(&ring, hole));
    }

    #[test]
    fn a_thin_ring_is_not_a_disc() {
        let r = 60.0;
        let disc = Circle::new((100.0, 100.0), r).to_path(1e-6);
        let mut ring = disc.clone();
        ring.extend(
            &Circle::new((100.0, 100.0), r - TOL / 2.0)
                .to_path(1e-6)
                .reverse_subpaths(),
        );
        let out = assert_fail("disc vs ring", &disc, &ring);
        // The boundaries alone are within `δ`, so only the region check
        // sees it.
        assert!(out.hausdorff <= TOL, "H = {}", out.hausdorff);
        assert!(out.reason.contains("regions differ"), "{}", out.reason);
    }

    #[test]
    fn scaling_by_three_tol_fails() {
        for (name, a) in outlines() {
            assert_fail(name, &a, &perturb::scale_about_center(&a, 3.0 * TOL));
        }
    }

    #[test]
    fn removing_one_dash_fails() {
        let (_, dashed) = outlines().pop().unwrap();
        let n = perturb::subpaths(&dashed).len();
        assert!(n > 4, "expected several dashes, got {n}");
        for drop in [0, n / 2, n - 1] {
            assert_fail("dash", &dashed, &perturb::without_subpath(&dashed, drop));
        }
    }

    #[test]
    fn empty_and_non_finite_outlines() {
        let empty = BezPath::new();
        assert_eq!(
            compare(&empty, &empty, contract(), true).verdict,
            Verdict::Identical
        );
        // A degenerate outline paints nothing and matches an empty one.
        let mut degenerate = BezPath::new();
        degenerate.move_to((10.0, 10.0));
        degenerate.line_to((10.0, 10.0));
        degenerate.move_to((20.0, 20.0));
        degenerate.line_to((30.0, 20.0));
        degenerate.close_path();
        assert_pass("degenerate", &degenerate, &empty);
        // A sliver of rounding may vanish. A thin stroke may not.
        assert_pass("sliver", &square(0.0, 0.0, 100.0, 1e-4), &empty);
        assert_fail("thin", &square(0.0, 0.0, 100.0, 0.05), &empty);
        // A real region may not.
        let filled = square(0.0, 0.0, 10.0, 10.0);
        assert_fail("square vs empty", &filled, &empty);
        assert_fail("empty vs square", &empty, &filled);
        let mut nan = filled.clone();
        nan.line_to((f64::NAN, 0.0));
        assert_fail("nan", &filled, &nan);
    }

    #[test]
    fn areas_follow_winding() {
        let s = square(0.0, 0.0, 10.0, 10.0);
        for p in [s.clone(), s.reverse_subpaths()] {
            let out = compare(&p, &p, contract(), true);
            assert!((out.area_ref - 100.0).abs() < 1e-9, "area {}", out.area_ref);
        }
        // Double winding paints once.
        let mut twice = s.clone();
        twice.extend(&s);
        let out = assert_pass("twice", &twice, &s);
        assert!((out.area_ref - 100.0).abs() < 1e-9, "area {}", out.area_ref);
    }
}
