//! Nonzero union of closed Bézier contours, for merging glyph outlines.
//!
//! Works on the curves directly, so the outline keeps its Bézier
//! segments. It does what the crate's region stitching does: cut the
//! contours where they cross, keep the pieces with fill on exactly one
//! side, and stitch those back into loops.
//!
//! Glyphs cross both between contours and within one. Composite accents
//! and Inter's k are separate contours that overlap. Inter's e and & are
//! single contours that cross themselves: the e's bar runs on into the
//! bowl, and the & crosses its own loop.

use kurbo_se::kurbo::{
    BezPath, ParamCurve, ParamCurveDeriv, PathEl, PathSeg, Point, Rect, Shape, Vec2,
};

/// Cut parameters this close to a segment end are the joint itself.
const T_EPS: f64 = 1e-6;
/// Hits closer than this in both parameters are one crossing.
const DEDUP_EPS: f64 = 1e-6;
/// Neighbouring segments share an endpoint. Their crossing search skips
/// this much of each parameter range at that end, so the shared point
/// never counts as a crossing.
const ADJ_TRIM: f64 = 1e-3;
/// Subdivision depth and node budget for one pair search. The crate's
/// `split.rs` explains why a budget is needed.
const MAX_DEPTH: u32 = 28;
const SEARCH_BUDGET: u32 = 10_000;
/// How far from a piece the fill is probed, in multiples of the accuracy.
/// Short pieces probe closer.
const PROBE: f64 = 64.0;

struct Contour {
    path: BezPath,
    /// Without zero-length segments, which carry no direction to probe.
    segs: Vec<PathSeg>,
    boxes: Vec<Rect>,
    bbox: Rect,
}

/// The nonzero union of `path`'s closed contours, as loops that keep the
/// input's curves. The fill is always on the right of travel, so outer
/// loops come out clockwise in y-down space and holes counterclockwise.
/// `accuracy` bounds the crossing search. Pass a small fraction of the
/// outline's size.
pub(crate) fn union_nonzero(path: &BezPath, accuracy: f64) -> BezPath {
    let contours = split_contours(path);

    // --- every crossing, within a contour and between two ---
    let mut cuts: Vec<Vec<Vec<f64>>> = contours
        .iter()
        .map(|c| vec![Vec::new(); c.segs.len()])
        .collect();
    for a in 0..contours.len() {
        let ca = &contours[a];
        let m = ca.segs.len();
        for i in 0..m {
            for j in (i + 1)..m {
                if !overlaps(ca.boxes[i], ca.boxes[j], accuracy) {
                    continue;
                }
                let next = j == i + 1;
                let wrap = i == 0 && j == m - 1;
                for (ti, tj) in crossings(&ca.segs[i], &ca.segs[j], next, wrap, accuracy) {
                    cuts[a][i].push(ti);
                    cuts[a][j].push(tj);
                }
            }
        }
        for b in (a + 1)..contours.len() {
            let cb = &contours[b];
            if !overlaps(ca.bbox, cb.bbox, accuracy) {
                continue;
            }
            for i in 0..m {
                if !overlaps(ca.boxes[i], cb.bbox, accuracy) {
                    continue;
                }
                for j in 0..cb.segs.len() {
                    if !overlaps(ca.boxes[i], cb.boxes[j], accuracy) {
                        continue;
                    }
                    for (ti, tj) in crossings(&ca.segs[i], &cb.segs[j], false, false, accuracy) {
                        cuts[a][i].push(ti);
                        cuts[b][j].push(tj);
                    }
                }
            }
        }
    }

    // Nonzero fill of the whole path at `p`. A point outside a contour's
    // bounds has winding 0 for it, so only the contours around `p` count.
    let filled = |p: Point| {
        contours
            .iter()
            .filter(|c| contains(c.bbox, p))
            .map(|c| c.path.winding(p))
            .sum::<i32>()
            != 0
    };
    // A piece bounds the union where the fill is on exactly one side of it.
    // Probe both sides at parameter `t`. The answer is whether to reverse
    // the piece so the fill ends up on its right, or None to drop it.
    let decide = |seg: &PathSeg, t: f64| -> Option<bool> {
        let tan = tangent(seg, t);
        let len = tan.hypot();
        if len == 0.0 {
            return None;
        }
        // `n` points right of travel in y-down space. On a short piece the
        // probe distance shrinks to a tenth of its chord, so the probe stays
        // clear of the other boundaries that cross at its ends.
        let chord = (seg.end() - seg.start()).hypot();
        let d = (PROBE * accuracy).min(0.1 * chord.max(len * 1e-3));
        let n = Vec2::new(-tan.y, tan.x) * (d / len);
        let p = seg.eval(t);
        let (right, left) = (filled(p + n), filled(p - n));
        (right != left).then_some(left)
    };
    // Majority of three samples, so a piece still classifies when one
    // probe misjudges it, for example at a tangency the search missed.
    let decide3 = |s: [(&PathSeg, f64); 3]| {
        let first = decide(s[1].0, s[1].1);
        let second = decide(s[0].0, s[0].1);
        if first == second {
            return first;
        }
        let third = decide(s[2].0, s[2].1);
        if third == second { second } else { first }
    };

    let mut out = BezPath::new();
    let mut pieces: Vec<PathSeg> = Vec::new();
    for (ci, c) in contours.iter().enumerate() {
        if cuts[ci].iter().all(Vec::is_empty) {
            // Nothing crosses this contour, not even itself, so all of it
            // is on one side of everything else. Keep or drop it whole.
            let n = c.segs.len();
            let samples = [
                (&c.segs[0], 0.5),
                (&c.segs[n / 2], 0.5),
                (&c.segs[n - 1], 0.5),
            ];
            match decide3(samples) {
                Some(false) => out.extend(c.path.iter()),
                Some(true) => out.extend(c.path.reverse_subpaths().iter()),
                None => {}
            }
            continue;
        }
        for (si, seg) in c.segs.iter().enumerate() {
            for piece in subdivide_at(seg, &mut cuts[ci][si]) {
                match decide3([(&piece, 0.25), (&piece, 0.5), (&piece, 0.75)]) {
                    Some(false) => pieces.push(piece),
                    Some(true) => pieces.push(piece.reverse()),
                    None => {}
                }
            }
        }
    }
    stitch(&pieces, accuracy, &mut out);
    out
}

/// The closed contours of `path`. Open subpaths get closed, as the fill
/// closes them. Subpaths with no extent bound nothing and are dropped.
fn split_contours(path: &BezPath) -> Vec<Contour> {
    let els = path.elements();
    let mut out = Vec::new();
    let mut i = 0;
    while i < els.len() {
        let mut j = i + 1;
        while j < els.len() && !matches!(els[j], PathEl::MoveTo(_)) {
            j += 1;
        }
        let mut sub = BezPath::from_vec(els[i..j].to_vec());
        if !matches!(sub.elements().last(), Some(PathEl::ClosePath)) {
            sub.close_path();
        }
        let segs: Vec<PathSeg> = sub
            .segments()
            .filter(|s| {
                let b = s.bounding_box();
                b.width() > 0.0 || b.height() > 0.0
            })
            .collect();
        if !segs.is_empty() {
            out.push(Contour {
                boxes: segs.iter().map(Shape::bounding_box).collect(),
                bbox: sub.bounding_box(),
                path: sub,
                segs,
            });
        }
        i = j;
    }
    out
}

fn deriv(seg: &PathSeg, t: f64) -> Vec2 {
    match seg {
        PathSeg::Line(l) => l.p1 - l.p0,
        PathSeg::Quad(q) => q.deriv().eval(t).to_vec2(),
        PathSeg::Cubic(c) => c.deriv().eval(t).to_vec2(),
    }
}

/// Direction of travel at `t`. Falls back to the chord where the
/// derivative vanishes, as when a control point sits on an endpoint.
fn tangent(seg: &PathSeg, t: f64) -> Vec2 {
    let d = deriv(seg, t);
    if d.hypot2() > 1e-24 {
        d
    } else {
        seg.end() - seg.start()
    }
}

/// Chain the kept pieces into closed loops. At a crossing the boundary
/// continues from the same point on another contour, so the walk moves on
/// to whichever unused piece starts there. If none does, as when the
/// samples misjudged a tiny piece, a line bridges the gap to the nearest
/// start, unless the loop's own start is nearer.
fn stitch(pieces: &[PathSeg], accuracy: f64, out: &mut BezPath) {
    // Cut points of a crossing agree to about the search accuracy.
    let eps = 16.0 * accuracy;
    let max_bridge = 1e3 * accuracy;
    let mut used = vec![false; pieces.len()];
    for start in 0..pieces.len() {
        if used[start] {
            continue;
        }
        let first = pieces[start].start();
        out.move_to(first);
        let mut cur = start;
        loop {
            used[cur] = true;
            append(out, pieces[cur]);
            let end = pieces[cur].end();
            let to_first = (end - first).hypot();
            if to_first <= eps {
                break;
            }
            let next = (0..pieces.len())
                .filter(|&k| !used[k])
                .map(|k| ((pieces[k].start() - end).hypot(), k))
                .min_by(|a, b| a.0.total_cmp(&b.0));
            match next {
                Some((d, k)) if d <= eps || (d <= max_bridge && d < to_first) => {
                    if d > eps {
                        out.line_to(pieces[k].start());
                    }
                    cur = k;
                }
                _ => break,
            }
        }
        out.close_path();
    }
}

fn append(path: &mut BezPath, seg: PathSeg) {
    match seg {
        PathSeg::Line(l) => path.line_to(l.p1),
        PathSeg::Quad(q) => path.quad_to(q.p1, q.p2),
        PathSeg::Cubic(c) => path.curve_to(c.p1, c.p2, c.p3),
    }
}

/// Split `seg` at the interior parameters in `ts`.
fn subdivide_at(seg: &PathSeg, ts: &mut Vec<f64>) -> Vec<PathSeg> {
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

/// Whether two boxes come within `slack` of touching.
fn overlaps(a: Rect, b: Rect, slack: f64) -> bool {
    !(a.x0 > b.x1 + slack || b.x0 > a.x1 + slack || a.y0 > b.y1 + slack || b.y0 > a.y1 + slack)
}

/// Inclusive bounds test. A point outside a closed contour's bounds has
/// winding 0 for it.
fn contains(r: Rect, p: Point) -> bool {
    p.x >= r.x0 && p.x <= r.x1 && p.y >= r.y0 && p.y <= r.y1
}

/// Crossing parameters `(ta, tb)` of two segments. Both are split down to
/// flat pieces, whose chords are then intersected. For neighbours in one
/// contour, `next` marks `end(a) = start(b)` and `wrap` marks
/// `start(a) = end(b)`. The shared end is trimmed off both search ranges.
fn crossings(a: &PathSeg, b: &PathSeg, next: bool, wrap: bool, accuracy: f64) -> Vec<(f64, f64)> {
    let ar = (
        if wrap { ADJ_TRIM } else { 0.0 },
        if next { 1.0 - ADJ_TRIM } else { 1.0 },
    );
    let br = (
        if next { ADJ_TRIM } else { 0.0 },
        if wrap { 1.0 - ADJ_TRIM } else { 1.0 },
    );
    let mut hits = Vec::new();
    let mut budget = SEARCH_BUDGET;
    recurse(
        &a.subsegment(ar.0..ar.1),
        ar,
        &b.subsegment(br.0..br.1),
        br,
        accuracy.max(1e-12),
        0,
        &mut hits,
        &mut budget,
    );
    // The chord solve only estimates. A crossing at the joint between two
    // segments can land just outside each segment's range, so a strict
    // range test would drop it from both. The missing cut tears the
    // stitched loop open, as in Inter's & at some sizes. So polish every
    // estimate on the real curves, then test the range.
    let mut hits: Vec<(f64, f64)> = hits
        .into_iter()
        .filter_map(|(s, t)| {
            let (s, t) = refine(a, b, s, t, accuracy)?;
            let inside = |x: f64, r: (f64, f64)| x >= r.0 - 1e-9 && x <= r.1 + 1e-9;
            (inside(s, ar) && inside(t, br)).then_some((s.clamp(0.0, 1.0), t.clamp(0.0, 1.0)))
        })
        .collect();
    hits.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.total_cmp(&y.1)));
    hits.dedup_by(|x, y| (x.0 - y.0).abs() < DEDUP_EPS && (x.1 - y.1).abs() < DEDUP_EPS);
    hits
}

/// Polish a crossing estimate with Newton's method on `a(s) = b(t)`.
/// Returns None when it does not converge. That happens at a tangency,
/// where the curves run parallel and there is no crossing to cut.
fn refine(a: &PathSeg, b: &PathSeg, mut s: f64, mut t: f64, accuracy: f64) -> Option<(f64, f64)> {
    for _ in 0..12 {
        let f = a.eval(s) - b.eval(t);
        let (da, db) = (deriv(a, s), deriv(b, t));
        let cross = da.cross(db);
        if cross.abs() <= 1e-12 * da.hypot() * db.hypot() {
            return None;
        }
        // Solve da·Δs − db·Δt = −f by Cramer's rule.
        let (ds, dt) = (-f.cross(db) / cross, -f.cross(da) / cross);
        s += ds;
        t += dt;
        if !(s.is_finite() && t.is_finite()) {
            return None;
        }
        if ds.abs() < 1e-14 && dt.abs() < 1e-14 {
            break;
        }
    }
    ((a.eval(s) - b.eval(t)).hypot() <= accuracy).then_some((s, t))
}

#[allow(clippy::too_many_arguments)]
fn recurse(
    a: &PathSeg,
    ar: (f64, f64),
    b: &PathSeg,
    br: (f64, f64),
    accuracy: f64,
    depth: u32,
    out: &mut Vec<(f64, f64)>,
    budget: &mut u32,
) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    if !overlaps(a.bounding_box(), b.bounding_box(), accuracy) {
        return;
    }
    let (fa, fb) = (flatness(a), flatness(b));
    if (fa <= accuracy && fb <= accuracy) || depth >= MAX_DEPTH {
        let (a0, b0) = (a.start(), b.start());
        let (da, db) = (a.end() - a0, b.end() - b0);
        let denom = da.cross(db);
        if denom == 0.0 {
            return; // parallel chords: a tangency, not a crossing
        }
        let s = (b0 - a0).cross(db) / denom;
        let t = (b0 - a0).cross(da) / denom;
        // Loose on purpose: this is only the estimate `refine` starts
        // from, and the real range test comes after it.
        if !(-0.5..=1.5).contains(&s) || !(-0.5..=1.5).contains(&t) {
            return;
        }
        out.push((ar.0 + s * (ar.1 - ar.0), br.0 + t * (br.1 - br.0)));
        return;
    }
    // Halve the less flat one.
    if fa >= fb {
        let am = 0.5 * (ar.0 + ar.1);
        let (lo, hi) = (a.subsegment(0.0..0.5), a.subsegment(0.5..1.0));
        recurse(&lo, (ar.0, am), b, br, accuracy, depth + 1, out, budget);
        recurse(&hi, (am, ar.1), b, br, accuracy, depth + 1, out, budget);
    } else {
        let bm = 0.5 * (br.0 + br.1);
        let (lo, hi) = (b.subsegment(0.0..0.5), b.subsegment(0.5..1.0));
        recurse(a, ar, &lo, (br.0, bm), accuracy, depth + 1, out, budget);
        recurse(a, ar, &hi, (bm, br.1), accuracy, depth + 1, out, budget);
    }
}

/// Largest distance from a control point to the chord.
fn flatness(seg: &PathSeg) -> f64 {
    let dist = |p: Point, a: Point, b: Point| {
        let d = b - a;
        let len = d.hypot();
        if len == 0.0 {
            (p - a).hypot()
        } else {
            (p - a).cross(d).abs() / len
        }
    };
    match seg {
        PathSeg::Line(_) => 0.0,
        PathSeg::Quad(q) => dist(q.p1, q.p0, q.p2),
        PathSeg::Cubic(c) => dist(c.p1, c.p0, c.p3).max(dist(c.p2, c.p0, c.p3)),
    }
}
