// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The alignment contract, checked directly against its set definition:
//!
//! ```text
//! Inside(w)  = { p ∈ F : dist(p, path) ≤ w }
//! Outside(w) = { p ∉ F : dist(p, path) ≤ w }
//! ```
//!
//! Sampled on a grid, with a band of `±slack` around `dist = w` excluded.
//! Approximation error that close to the boundary is expected and
//! harmless.

use kurbo::{BezPath, Circle, Join, ParamCurveNearest, Point, Rect, Shape};
use kurbo_se::{StrokeAlignment, StrokeStyle, stroke_aligned};

const TOL: f64 = 1e-3;

fn dist_to_path(path: &BezPath, p: Point) -> f64 {
    path.segments()
        .map(|s| s.nearest(p, 1e-6).distance_sq)
        .fold(f64::INFINITY, f64::min)
        .sqrt()
}

/// Check both alignments of `path` at width `w` against the set
/// definition.
///
/// Round joins throughout: the set definition is the Minkowski sum with a
/// disc, which is what a round join produces. Miter and bevel joins
/// deliberately deviate at convex corners — a miter spike reaches past `w`
/// — so the geometry tests cover those instead.
fn check(name: &str, path: &BezPath, w: f64, grid: usize) {
    check_at(name, path, w, grid, TOL);
}

/// [`check`] at tolerance `tol`.
fn check_at(name: &str, path: &BezPath, w: f64, grid: usize, tol: f64) {
    for alignment in [StrokeAlignment::Inside, StrokeAlignment::Outside] {
        check_side_at(name, path, w, grid, tol, alignment);
    }
}

/// [`check_at`] for one alignment.
fn check_side_at(
    name: &str,
    path: &BezPath,
    w: f64,
    grid: usize,
    tol: f64,
    alignment: StrokeAlignment,
) {
    let style = StrokeStyle::new(w)
        .with_alignment(alignment)
        .with_join(Join::Round);
    let out = stroke_aligned(path, &style, tol);
    assert!(out.is_finite(), "{name}/{alignment:?}: non-finite output");

    let bbox = path.bounding_box().inflate(w * 1.6 + 8.0, w * 1.6 + 8.0);
    let slack = (w * 0.06).max(1.5);
    let mut checked = 0usize;
    for iy in 0..grid {
        for ix in 0..grid {
            let p = Point::new(
                bbox.x0 + (ix as f64 + 0.5) / grid as f64 * bbox.width(),
                bbox.y0 + (iy as f64 + 0.5) / grid as f64 * bbox.height(),
            );
            let d = dist_to_path(path, p);
            // Skip the transition band; near-boundary disagreement is
            // approximation error, not a semantic difference.
            if (d - w).abs() < slack {
                continue;
            }
            let in_fill = path.winding(p) != 0;
            let want = match alignment {
                StrokeAlignment::Inside => in_fill && d <= w,
                StrokeAlignment::Outside => !in_fill && d <= w,
                StrokeAlignment::Center => unreachable!(),
            };
            let got = out.winding(p) != 0;
            assert_eq!(
                got, want,
                "{name}/{alignment:?} w={w} tol={tol} at {p:?}: dist {d:.2}, \
                 in_fill {in_fill} — expected painted={want}, got {got}"
            );
            checked += 1;
        }
    }
    assert!(checked > grid * grid / 4, "{name}: too few samples checked");
}

/// Check a centered stroke against its set definition,
/// `Center(w) = { p : dist(p, path) ≤ w/2 }`, which is independent of the
/// fill.
///
/// Round joins for the reason [`check`] gives. This holds at any width for
/// closed simple contours: past the local thickness the band saturates
/// instead of the inverted inner boundary punching holes. The 2026-08-21
/// report was a circle stroked wider than its diameter, which grew a hole
/// in the middle.
fn check_center(name: &str, path: &BezPath, w: f64, grid: usize) {
    check_center_at(name, path, w, grid, TOL);
}

/// [`check_center`] at tolerance `tol`.
fn check_center_at(name: &str, path: &BezPath, w: f64, grid: usize, tol: f64) {
    let style = StrokeStyle::new(w)
        .with_alignment(StrokeAlignment::Center)
        .with_join(Join::Round);
    let out = stroke_aligned(path, &style, tol);
    assert!(out.is_finite(), "{name}/center: non-finite output");

    let h = 0.5 * w;
    let bbox = path.bounding_box().inflate(h * 1.2 + 8.0, h * 1.2 + 8.0);
    let slack = (h * 0.06).max(1.5);
    let mut checked = 0usize;
    for iy in 0..grid {
        for ix in 0..grid {
            let p = Point::new(
                bbox.x0 + (ix as f64 + 0.5) / grid as f64 * bbox.width(),
                bbox.y0 + (iy as f64 + 0.5) / grid as f64 * bbox.height(),
            );
            let d = dist_to_path(path, p);
            if (d - h).abs() < slack {
                continue;
            }
            let want = d <= h;
            let got = out.winding(p) != 0;
            assert_eq!(
                got, want,
                "{name}/center w={w} tol={tol} at {p:?}: dist {d:.2} — expected \
                 painted={want}, got {got}"
            );
            checked += 1;
        }
    }
    assert!(checked > grid * grid / 4, "{name}: too few samples checked");
}

fn star(points: usize, r_outer: f64, r_inner: f64) -> BezPath {
    let mut p = BezPath::new();
    for i in 0..(points * 2) {
        let r = if i % 2 == 0 { r_outer } else { r_inner };
        let a = core::f64::consts::PI * (i as f64) / points as f64;
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

/// A gear with `teeth` teeth around `(c, c)`: `2 * teeth` vertices,
/// alternating between `r_tip` and `r_root`.
fn gear(teeth: usize, c: f64, r_tip: f64, r_root: f64) -> BezPath {
    let mut p = BezPath::new();
    for i in 0..(teeth * 2) {
        let r = if i % 2 == 0 { r_tip } else { r_root };
        let a = core::f64::consts::PI * (i as f64) / teeth as f64;
        let pt = (c + r * a.cos(), c + r * a.sin());
        if i == 0 {
            p.move_to(pt);
        } else {
            p.line_to(pt);
        }
    }
    p.close_path();
    p
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

fn donut() -> BezPath {
    let mut p = Circle::new((130.0, 130.0), 100.0).to_path(1e-7);
    p.extend(
        Circle::new((130.0, 130.0), 45.0)
            .to_path(1e-7)
            .reverse_subpaths()
            .iter(),
    );
    p
}

fn wedge() -> BezPath {
    let mut p = BezPath::new();
    p.move_to((20.0, 180.0));
    p.line_to((230.0, 160.0));
    p.line_to((20.0, 140.0));
    p.close_path();
    p
}

fn figure_eight() -> BezPath {
    let mut p = BezPath::new();
    p.move_to((120.0, 100.0));
    p.curve_to((150.0, 55.0), (210.0, 55.0), (240.0, 100.0));
    p.curve_to((210.0, 145.0), (150.0, 145.0), (120.0, 100.0));
    p.curve_to((90.0, 55.0), (30.0, 55.0), (0.0, 100.0));
    p.curve_to((30.0, 145.0), (90.0, 145.0), (120.0, 100.0));
    p.close_path();
    p
}

#[test]
fn rect_moderate_and_saturating() {
    let rect = Rect::new(40.0, 80.0, 120.0, 140.0).to_path(1e-9);
    check("rect", &rect, 8.0, 56);
    check("rect", &rect, 35.0, 56); // 2w > short side: saturates
    check("rect", &rect, 90.0, 48); // far beyond: whole shape
}

#[test]
fn circle_moderate_and_saturating() {
    let c = Circle::new((120.0, 120.0), 70.0).to_path(1e-7);
    check("circle", &c, 10.0, 56);
    check("circle", &c, 80.0, 48); // w > radius: saturates
}

#[test]
fn star_concave_corners() {
    let s = star(5, 100.0, 40.0);
    check("star", &s, 10.0, 64);
    check("star", &s, 60.0, 56); // the reported extreme-weight case
}

/// Eroded close to collapse, a gear leaves a ring of short join arcs
/// around its middle, one per root, each crossing its neighbours. Past
/// each crossing an arc dips into its neighbour's band, but less deeply
/// than the pruning slack, so it survived. These overshoots stitched into
/// extra loops that painted the middle. At a coarse tolerance the
/// stitcher's match radius was also longer than the arcs, so every walk
/// closed after its first piece. The grids are odd so that a sample lands
/// on the centre.
#[test]
fn a_gear_eroded_near_collapse_keeps_its_middle() {
    let g = gear(24, 150.0, 110.0, 90.0);
    for w in [60.0, 70.0, 80.0] {
        check_at("gear-24", &g, w, 61, 0.25);
    }
    check_at("gear-24", &g, 80.0, 61, 0.01);
    check_center_at("gear-24", &g, 120.0, 61, 0.25);
}

/// The same at scale: 750 teeth about a third of the width apart, so the
/// erosion is a ring of 750 arcs. The whole gear used to paint. It samples
/// along rays instead of a grid, because the distance oracle walks all 1500
/// edges for each sample.
#[test]
fn a_fine_gear_eroded_past_its_teeth_keeps_its_middle() {
    let g = gear(750, 400.0, 300.0, 290.0);
    let w = 8.0;
    let style = StrokeStyle::new(w)
        .with_alignment(StrokeAlignment::Inside)
        .with_join(Join::Round);
    let out = stroke_aligned(&g, &style, 0.25);
    let slack = 1.5;
    let mut checked = 0;
    for k in 0..48 {
        let a = core::f64::consts::TAU * (k as f64 + 0.3) / 48.0;
        for r in [0.0, 150.0, 260.0, 279.0, 286.0, 289.0] {
            let p = Point::new(400.0 + r * a.cos(), 400.0 + r * a.sin());
            let d = dist_to_path(&g, p);
            if (d - w).abs() < slack {
                continue;
            }
            let want = g.winding(p) != 0 && d <= w;
            assert_eq!(
                out.winding(p) != 0,
                want,
                "gear-750/Inside w={w} at {p:?}: dist {d:.2}"
            );
            checked += 1;
        }
    }
    assert!(checked > 48 * 4, "too few samples checked");
}

#[test]
fn donut_hole_and_saturation() {
    let d = donut();
    check("donut", &d, 12.0, 56);
    check("donut", &d, 100.0, 48); // ring thinner than w: whole ring
}

/// The two inward offsets of the donut coincide at exactly half the ring
/// thickness ((100 − 45) / 2 = 27.5), where the erosion is measure-zero.
/// Just below, a thin annulus must survive; at and beyond, the ring
/// saturates — through the degeneracy, the set contract must hold.
#[test]
fn donut_offset_coincidence() {
    let d = donut();
    check("donut", &d, 27.4, 48);
    check("donut", &d, 27.5, 48);
    check("donut", &d, 27.6, 48);
}

/// The inside offset of a circle collapses to a point at exactly w = r.
#[test]
fn circle_collapse_at_radius() {
    let c = Circle::new((120.0, 120.0), 70.0).to_path(1e-7);
    check("circle", &c, 70.0, 48);
}

#[test]
fn thin_wedge() {
    let w = wedge();
    check("wedge", &w, 8.0, 56);
    check("wedge", &w, 60.0, 56); // saturates
}

/// Centered strokes hold the set contract at any width on closed simple
/// contours: annulus at moderate widths, solid stadium past the fold
/// (2026-08-21 report — the direct band's inverted inner boundary used to
/// cancel interior coverage). Self-intersecting contours keep kurbo's
/// winding-additive centered band and are deliberately absent here.
#[test]
fn centered_set_semantics_all_widths() {
    let c = Circle::new((120.0, 120.0), 70.0).to_path(1e-7);
    check_center("circle", &c, 10.0, 56);
    check_center("circle", &c, 130.0, 48); // annulus, thin hole
    check_center("circle", &c, 145.0, 48); // w/2 > r: hole must vanish
    check_center("circle", &c, 300.0, 40); // far past: solid stadium

    let d = donut();
    check_center("donut", &d, 12.0, 56);
    check_center("donut", &d, 95.0, 48); // hole contour folds (w/2 > 45)
    check_center("donut", &d, 240.0, 40); // everything floods

    let rect = Rect::new(40.0, 80.0, 120.0, 140.0).to_path(1e-9);
    check_center("rect", &rect, 8.0, 56);
    check_center("rect", &rect, 70.0, 48); // w/2 > short half-side
    check_center("rect", &rect, 200.0, 40);

    let w = wedge();
    check_center("wedge", &w, 8.0, 56);
    check_center("wedge", &w, 60.0, 48);

    let s = star(5, 100.0, 40.0);
    check_center("star", &s, 10.0, 56);
    check_center("star", &s, 90.0, 48);
}

#[test]
fn self_intersecting_bowtie() {
    let b = bowtie();
    check("bowtie", &b, 12.0, 64);
    check("bowtie", &b, 32.5, 56);
    check("bowtie", &b, 90.0, 48); // saturates both lobes
}

#[test]
fn self_intersecting_figure_eight() {
    let f = figure_eight();
    check("figure-eight", &f, 8.0, 64);
    check("figure-eight", &f, 40.0, 56);
}

/// The open cubic `M20,150 C200,20 0,20 180,150`, which crosses itself in a
/// small loop.
fn loop_curve() -> BezPath {
    let mut p = BezPath::new();
    p.move_to((20.0, 150.0));
    p.curve_to((200.0, 20.0), (0.0, 20.0), (180.0, 150.0));
    p
}

/// The loop of [`loop_curve`] is cut off as a closed lobe whose interior
/// is filled. The probe that decides this used to land within rounding of
/// the curve and read the loop as empty. The lobe's one-sided strokes then
/// went to the wrong side: outside ones into the loop or nowhere, and a
/// left side, which faces the fill here, nowhere. Even with the side right,
/// a dashed inside stroke painted the loop's interior instead of the band,
/// because the fill mask dropped the lobe, a single curve, as a sliver.
#[test]
fn a_loop_strokes_on_the_side_of_its_fill() {
    use kurbo_se::{DashStyle, StrokeSide};

    let loop_curve = loop_curve();
    // The loop's apex is (100, 52.5), and its radius of curvature there is
    // about 1.15, so the band stays narrower than that. The two points lie
    // 0.5 inside and outside the loop.
    let in_loop = Point::new(100.0, 53.0);
    let above_loop = Point::new(100.0, 52.0);
    let inside = StrokeStyle::new(1.0).with_alignment(StrokeAlignment::Inside);
    let outside = StrokeStyle::new(1.0).with_alignment(StrokeAlignment::Outside);
    // One dash longer than the path: painted like the solid stroke, but
    // through dashing and the fill mask.
    let dash = DashStyle::new(1000.0, 1.0);
    let cases = [
        ("inside", inside.clone(), in_loop, above_loop),
        ("outside", outside.clone(), above_loop, in_loop),
        (
            "inside dashed",
            inside.with_dash(dash.clone()),
            in_loop,
            above_loop,
        ),
        (
            "outside dashed",
            outside.with_dash(dash),
            above_loop,
            in_loop,
        ),
        (
            "left side",
            StrokeStyle::new(1.0).with_side(StrokeSide::Left),
            in_loop,
            above_loop,
        ),
    ];
    for tol in [0.25, 1e-3] {
        for (name, style, painted, clear) in &cases {
            let out = stroke_aligned(&loop_curve, style, tol);
            let at = format!("{name} tol={tol}");
            assert_ne!(out.winding(*painted), 0, "{at}: {painted:?} unpainted");
            assert_eq!(out.winding(*clear), 0, "{at}: {clear:?} painted");
        }
    }
}

/// An inside stroke wider than the loop of [`loop_curve`] fills it, dashed
/// or not. Past the radius of curvature, the dash band's offset edge turns
/// back across the loop, where the band covers both sides of it. The fill
/// mask kept that edge and closed it into a sliver of opposite winding,
/// which left an unpainted line across the loop.
#[test]
fn an_inside_stroke_wider_than_a_loop_fills_it() {
    let loop_curve = loop_curve();
    for w in [3.0, 10.0] {
        let solid = StrokeStyle::new(w).with_alignment(StrokeAlignment::Inside);
        let dashed = solid
            .clone()
            .with_dash(kurbo_se::DashStyle::new(1000.0, 1.0));
        for tol in [0.25, 1e-3] {
            for style in [&solid, &dashed] {
                let out = stroke_aligned(&loop_curve, style, tol);
                // Down the loop's axis, from just below the apex at y = 52.5
                // to y = 66, short of the crossing near y = 67.9.
                for i in 0..=260 {
                    let p = Point::new(100.0, 53.0 + 0.05 * i as f64);
                    assert_ne!(
                        out.winding(p),
                        0,
                        "w={w} tol={tol} dashed={}: {p:?} unpainted",
                        style.dash.is_some()
                    );
                }
            }
        }
    }
}

/// A star around (150, 150), turned by `phase`, with `points` vertices
/// alternating between tips at radius 110 and roots at `r_root`. Each edge
/// is a quad whose control point is pulled into the star by `bulge` times
/// the edge's length. With enough bulge, the two flanks of each tip cross
/// below it, and the path splits into the body and one sliver per tip.
fn bulging_star(points: usize, r_root: f64, bulge: f64, phase: f64) -> BezPath {
    let pts: Vec<Point> = (0..points)
        .map(|i| {
            let r = if i % 2 == 0 { 110.0 } else { r_root };
            let a = core::f64::consts::TAU * (i as f64) / points as f64 + phase;
            Point::new(150.0 + r * a.cos(), 150.0 + r * a.sin())
        })
        .collect();
    let mut p = BezPath::new();
    p.move_to(pts[0]);
    for i in 1..=points {
        let (a, q) = (pts[i - 1], pts[i % points]);
        let d = q - a;
        p.quad_to(a.midpoint(q) + kurbo::Vec2::new(-d.y, d.x) * bulge, q);
    }
    p.close_path();
    p
}

/// The fill probe looks just below a lobe's topmost points, and it used to
/// count among them the points where a curve reaches furthest left or
/// right. The curve runs vertically there, so the point below lies on it
/// to within rounding. Here the body's probe landed 3.5e-10 from a flank
/// and read the fill as empty, so inside and outside strokes swapped sides
/// across the whole body.
#[test]
fn a_lobe_is_not_probed_beside_its_own_flank() {
    let p = bulging_star(24, 60.0, 0.15, 0.1);
    for tol in [0.01, 0.25] {
        check_at("bulging-star", &p, 4.0, 56, tol);
        check_at("bulging-star", &p, 10.0, 56, tol);
    }
}

/// With more bulge, the flanks of a star with 16 vertices cross a long way
/// below each tip, at a shallow angle. The intersection search placed such
/// crossings only to within its accuracy, and a shallow one worse still.
/// The fill probe looks just a millionth of the shape's size below a lobe's
/// top corner, so from the misplaced corner it looked outside the body,
/// and the body was stroked on the wrong side.
#[test]
fn a_lobe_cut_where_its_flanks_cross_reads_its_own_fill() {
    let p = bulging_star(16, 60.0, 0.3, 0.0);
    for tol in [0.01, 0.25] {
        check_at("bulging-star-16", &p, 6.0, 56, tol);
    }
}

/// A square whose closing edge carries a segment far shorter than anything
/// the pipeline resolves, as a boolean operation leaves where it solved a
/// crossing; with a square hole, and a second square beside it so the
/// centered set construction runs across contours.
fn square_with_sub_resolution_segment() -> BezPath {
    let mut p = BezPath::new();
    p.move_to((0.0, 0.0));
    p.line_to((100.0, 0.0));
    p.line_to((100.0, 100.0));
    p.line_to((0.0, 100.0));
    p.line_to((0.0, 1e-13));
    p.close_path();
    p.extend(
        Rect::new(30.0, 30.0, 70.0, 70.0)
            .to_path(1e-9)
            .reverse_subpaths()
            .iter(),
    );
    p.extend(Rect::new(130.0, 0.0, 160.0, 30.0).to_path(1e-9).iter());
    p
}

/// A square with a triangular hole whose apex leans right of the edge below
/// it, which runs straight down: the shape of the counter in Lato's and
/// Poppins' `4`. No point straight below one of the hole's topmost vertices
/// lies inside it.
fn leaning_hole() -> BezPath {
    let mut p = Rect::new(0.0, 0.0, 100.0, 100.0).to_path(1e-9);
    let mut hole = BezPath::new();
    hole.move_to((60.0, 80.0));
    hole.line_to((60.0, 20.0));
    hole.line_to((62.0, 10.0));
    hole.line_to((20.0, 80.0));
    hole.close_path();
    if hole.area().signum() == p.area().signum() {
        hole = hole.reverse_subpaths();
    }
    p.extend(hole.iter());
    p
}

/// Inter's `$` at 256 px, its overlapping S and bar merged into one outline
/// and two holes by `linesweeper` 0.5, as Voyager merges a text's contours
/// before stroking it (reported 2026-10-11). The bar is narrower than twice
/// the width checked, and the merge leaves segments around `1e-13` long
/// where the bar meets the S.
const MERGED_DOLLAR: &str = concat!(
    "M76.25,40.814558236183629 L76.25,15.625 L91.25,15.625 ",
    "L91.250000000001478,40.547558813597533 ",
    "C101.11762911921608,41.138567222814572 109.99262911921551,43.247714284948728 117.875,46.875 ",
    "C127.29166666666663,51.208333333333329 134.72916666666663,57.104166666666671 140.1875,64.5625 ",
    "C145.64583333333326,72.020833333333329 148.54166666666674,80.458333333333329 148.875,89.875 ",
    "L125.625,89.875 ",
    "C124.54166666666663,80.791666666666671 120.20833333333337,73.770833333333329 112.625,68.8125 ",
    "C106.60341596006481,64.875310435427025 99.478415963578527,62.501293475968239 91.250000010541044,61.690449121623629 ",
    "L91.25,124.84563758389262 L98.625,126.875 ",
    "C103.625,128.20833333333334 109.14583333333337,130.04166666666666 115.1875,132.375 ",
    "C121.22916666666663,134.70833333333334 127,137.8125 132.5,141.6875 ",
    "C138,145.5625 142.52083333333326,150.47916666666666 146.0625,156.4375 ",
    "C149.60416666666674,162.39583333333334 151.375,169.70833333333334 151.375,178.375 ",
    "C151.375,188.54166666666666 148.72916666666674,197.70833333333334 143.4375,205.875 ",
    "C138.14583333333326,214.04166666666666 130.45833333333337,220.5 120.375,225.25 ",
    "C112.09838184050056,229.14890276935097 102.3900488461951,231.44765366450693 91.250001017083491,232.14625268546786 ",
    "L91.25,256.375 L76.25,256.375 L76.25,232.13747994526159 ",
    "L76.249999999999659,232.13747994526159 ",
    "C65.945643797751245,231.46747343449425 56.799810464417988,229.37998011940704 48.8125,225.875 ",
    "C38.9375,221.54166666666666 31.208333333333371,215.47916666666666 25.625,207.6875 ",
    "C20.041666666666629,199.89583333333334 16.875,190.79166666666666 16.125,180.375 ",
    "L40.5,180.375 ",
    "C41.166666666666629,187.375 43.541666666666629,193.14583333333334 47.625,197.6875 ",
    "C51.708333333333371,202.22916666666666 56.9375,205.60416666666666 63.3125,207.8125 ",
    "C67.441472301617978,209.24279759467814 71.75397230161775,210.20990623629768 76.249999999999204,210.71382592485867 ",
    "L76.25,210.71382592485867 L76.25,143.46458333333334 L68.875,141.375 ",
    "C53.875,137.125 42.208333333333371,131.04166666666666 33.875,123.125 ",
    "C25.541666666666629,115.20833333333334 21.375,105.08333333333333 21.375,92.75 ",
    "C21.375,82.25 24.208333333333371,73.0625 29.875,65.1875 ",
    "C35.541666666666629,57.3125 43.208333333333371,51.208333333333329 52.875,46.875 ",
    "C60.027880824825729,43.668536181974673 67.819547491492926,41.648388927369211 76.250000000001592,40.814558236183629 ",
    "L76.25,40.814558236183629 Z M76.25,61.900408726538899 ",
    "C71.626369999137523,62.509023759797358 67.397203332470895,63.646387517617725 63.5625,65.3125 ",
    "C57.520833333333371,67.9375 52.833333333333371,71.520833333333329 49.5,76.0625 ",
    "C46.166666666666629,80.604166666666671 44.5,85.791666666666671 44.5,91.625 ",
    "C44.5,98.125 46.520833333333371,103.35416666666667 50.5625,107.3125 ",
    "C54.604166666666629,111.27083333333334 59.395833333333371,114.35416666666666 64.9375,116.5625 ",
    "C68.986357809295896,118.17595461573447 72.757191147566573,119.53362671812556 76.25000001481169,120.63551630717319 ",
    "L76.25,61.900408726538899 Z M91.25,147.71458333333334 L91.25,210.73731712447102 ",
    "L91.250000000000568,210.73731712447102 ",
    "C96.723224689003359,210.15271132218885 101.78572468900325,208.90693894736515 106.4375,207 ",
    "C113.14583333333337,204.25 118.45833333333337,200.375 122.375,195.375 ",
    "C126.29166666666663,190.375 128.25,184.58333333333334 128.25,178 ",
    "C128.25,172 126.5625,167.125 123.1875,163.375 ",
    "C119.8125,159.625 115.33333333333337,156.54166666666666 109.75,154.125 ",
    "C104.16666666666663,151.70833333333334 98.041666666666629,149.58333333333334 91.375,147.75 ",
    "L91.25,147.71458333333334 Z",
);

/// A segment shorter than the pipeline resolves is noise, not geometry. Its
/// two ends used to read as the contour revisiting a vertex, so a simple
/// contour was split into lobes there, and the centered construction left
/// it out of the fill the hole was judged against: the hole lost its band.
#[test]
fn a_sub_resolution_segment_does_not_split_a_contour() {
    let p = square_with_sub_resolution_segment();
    check("sub-resolution", &p, 8.0, 64);
    check_center("sub-resolution", &p, 8.0, 64);
}

/// The fill probe looks just below a hole's topmost points. When none of
/// them works it used to fall back to the hole's own signed area, which
/// takes a hole for a filled region: the inside stroke painted the whole
/// hole and the centered stroke left its edge bare.
#[test]
fn a_hole_whose_apex_leans_is_still_a_hole() {
    let p = leaning_hole();
    check("leaning-hole", &p, 4.0, 72);
    check_center("leaning-hole", &p, 4.0, 72);
}

/// Where the bar crosses the S, the bands either side of it meet across a
/// gap of twice the width less the bar's. At fine tolerances the stitcher's
/// match radius fell below that gap, the walk dead-ended and closed with a
/// chord down the bar; at coarse ones the radius outgrew the band and the
/// walk handed over to unrelated pieces. Both painted the fill through the
/// inside stroke.
#[test]
fn a_bar_narrower_than_the_band_keeps_its_crossings() {
    let p = BezPath::from_svg(MERGED_DOLLAR).expect("valid path data");
    for tol in [1.0 / 16.0, 0.25, 1.0] {
        check_at("merged-dollar", &p, 8.0, 96, tol);
        check_center_at("merged-dollar", &p, 8.0, 96, tol);
    }
}

/// Check every alignment with `join` against the set definition, away
/// from the corners.
///
/// A join only covers the wedge between its two edges' normals at a
/// corner. A bevel reaches as far from the corner as the band does, and a
/// miter reaches that far times the miter limit. Farther out, every join
/// must paint exactly what the definition says, as round joins do
/// everywhere.
fn check_away_from_corners(name: &str, path: &BezPath, w: f64, join: Join, miter_angle: f64) {
    let grid = 64;
    let corners: Vec<Point> = path
        .elements()
        .iter()
        .filter_map(|el| el.end_point())
        .collect();
    let limit = match join {
        Join::Miter => kurbo_se::miter_angle_to_limit(miter_angle),
        _ => 1.0,
    };
    for alignment in [
        StrokeAlignment::Center,
        StrokeAlignment::Inside,
        StrokeAlignment::Outside,
    ] {
        let style = StrokeStyle::new(w)
            .with_alignment(alignment)
            .with_join(join)
            .with_miter_angle(miter_angle);
        let out = stroke_aligned(path, &style, TOL);
        assert!(out.is_finite(), "{name}/{alignment:?}: non-finite output");
        let reach = match alignment {
            StrokeAlignment::Center => 0.5 * w,
            _ => w,
        };
        let near = reach * limit + 1.5;
        let bbox = path.bounding_box().inflate(reach + 8.0, reach + 8.0);
        let slack = (reach * 0.06).max(1.5);
        let mut checked = 0usize;
        for iy in 0..grid {
            for ix in 0..grid {
                let p = Point::new(
                    bbox.x0 + (ix as f64 + 0.5) / grid as f64 * bbox.width(),
                    bbox.y0 + (iy as f64 + 0.5) / grid as f64 * bbox.height(),
                );
                if corners.iter().any(|c| (*c - p).hypot() < near) {
                    continue;
                }
                let d = dist_to_path(path, p);
                if (d - reach).abs() < slack {
                    continue;
                }
                let in_fill = path.winding(p) != 0;
                let want = match alignment {
                    StrokeAlignment::Center => d <= reach,
                    StrokeAlignment::Inside => in_fill && d <= reach,
                    StrokeAlignment::Outside => !in_fill && d <= reach,
                };
                let got = out.winding(p) != 0;
                assert_eq!(
                    got, want,
                    "{name}/{alignment:?}/{join:?} w={w} at {p:?}: dist {d:.2}, \
                     in_fill {in_fill} — expected painted={want}, got {got}"
                );
                checked += 1;
            }
        }
        assert!(
            checked > grid * grid / 10,
            "{name}: too few samples checked"
        );
    }
}

/// Bevel joins, and miters that fall back to bevels, at widths where most
/// of the band's inner boundary is bevel chords. A star stroked centered
/// at weight 32, reported on 2026-10-20, came out with five triangular
/// holes and its middle painted.
///
/// Pruning used to judge every piece of the shrunken outline by plain
/// distance to the path, which treats every corner as round. A bevel chord
/// sits closer to its corner than the width, so it was pruned although it
/// is the true boundary. Where the boundary is mostly chords, as across a
/// star's arms or between a gear's teeth, the region tore apart and painted
/// what it should have left open.
#[test]
fn bevels_paint_the_set_away_from_corners() {
    // A miter angle this wide bevels every corner of these shapes.
    for (join, angle) in [(Join::Bevel, 28.96), (Join::Miter, 150.0)] {
        let s5 = star(5, 100.0, 40.0);
        check_away_from_corners("star", &s5, 16.0, join, angle);
        check_away_from_corners("star", &s5, 32.0, join, angle);
        let s7 = star(7, 100.0, 55.0);
        check_away_from_corners("7-point star", &s7, 32.0, join, angle);
        check_away_from_corners("7-point star", &s7, 64.0, join, angle);
        let g = gear(24, 150.0, 110.0, 90.0);
        for w in [20.0, 32.0, 40.0] {
            check_away_from_corners("gear", &g, w, join, angle);
        }
    }
}

fn polygon(points: &[(f64, f64)]) -> BezPath {
    let mut p = BezPath::new();
    p.move_to(points[0]);
    for &pt in &points[1..] {
        p.line_to(pt);
    }
    p.close_path();
    p
}

/// A one-sided band counts only the joins on its own side.
///
/// At the default miter angle, a corner's miter on the side the band is
/// not built on can reach four times the width. When it was counted, it
/// reached through a thin wall and pruned real boundary that no raw offset
/// replaces. An outside stroke lost the miter tip under a bump, and an
/// inside stroke painted past its width.
#[test]
fn joins_count_only_on_the_side_being_stroked() {
    // A bar 30 tall with a notch from the top and a bump below, 5 apart.
    let notch = polygon(&[
        (0.0, 0.0),
        (93.0, 0.0),
        (100.0, 25.0),
        (107.0, 0.0),
        (200.0, 0.0),
        (200.0, 30.0),
        (115.0, 30.0),
        (100.0, 40.0),
        (85.0, 30.0),
        (0.0, 30.0),
    ]);
    let style = StrokeStyle::new(20.0)
        .with_alignment(StrokeAlignment::Outside)
        .with_join(Join::Miter);
    let out = stroke_aligned(&notch, &style, 0.01);
    // The bump's miter tip reaches about 64.
    assert!(
        out.winding(Point::new(100.0, 60.0)) != 0,
        "outside miter tip under the bump was cut off"
    );
    assert!(out.bounding_box().y1 > 63.0);

    // A spike from above points at a notch from below.
    let spike = polygon(&[
        (0.0, 0.0),
        (200.0, 0.0),
        (200.0, 50.0),
        (138.3, 50.0),
        (130.0, 80.0),
        (121.7, 50.0),
        (60.0, 50.0),
        (60.0, 100.0),
        (115.0, 100.0),
        (130.0, 85.0),
        (145.0, 100.0),
        (200.0, 100.0),
        (200.0, 150.0),
        (0.0, 150.0),
    ]);
    let style = StrokeStyle::new(20.0)
        .with_alignment(StrokeAlignment::Inside)
        .with_join(Join::Miter);
    let out = stroke_aligned(&spike, &style, 0.01);
    // More than 20 from the outline and outside the notch corners' miters.
    for p in [
        (130.0, 115.0),
        (130.0, 116.0),
        (128.0, 117.0),
        (132.0, 117.0),
    ] {
        assert_eq!(
            out.winding(p.into()),
            0,
            "inside miter painted {p:?}, past its width"
        );
    }
}

/// Real miters, not only ones that fall back to bevels, on corners closer
/// together than the stroke width. At the default miter angle, an outside
/// stroke on the gear used to paint its whole fill.
#[test]
fn miters_on_dense_corners_stay_outside() {
    let g = gear(24, 150.0, 110.0, 90.0);
    let bbox = g.bounding_box();
    for w in [48.0, 100.0, 250.0] {
        let style = StrokeStyle::new(w)
            .with_alignment(StrokeAlignment::Outside)
            .with_join(Join::Miter);
        let out = stroke_aligned(&g, &style, 0.25);
        assert!(out.is_finite());
        for iy in 0..48 {
            for ix in 0..48 {
                let p = Point::new(
                    bbox.x0 + (ix as f64 + 0.5) / 48.0 * bbox.width(),
                    bbox.y0 + (iy as f64 + 0.5) / 48.0 * bbox.height(),
                );
                if g.winding(p) != 0 && dist_to_path(&g, p) > 0.5 {
                    assert_eq!(out.winding(p), 0, "gear w={w}: outside miter at {p:?}");
                }
            }
        }
    }
}

/// Inter's `4` at 200 px, as the font draws it: the stem is a separate
/// contour overlapping the diagonal and the bar.
const INTER_FOUR: &str = concat!(
    "M31.647727272727273,190.17045454545453 ",
    "L31.647727272727273,175.6818181818182 ",
    "L95.56818181818183,74.54545454545453 ",
    "L106.07954545454547,74.54545454545453 ",
    "L106.07954545454547,96.98863636363636 ",
    "L98.97727272727273,96.98863636363636 ",
    "L50.68181818181819,173.4090909090909 ",
    "L50.68181818181819,174.54545454545453 ",
    "L136.76136363636363,174.54545454545453 ",
    "L136.76136363636363,190.17045454545453 ",
    "L31.647727272727273,190.17045454545453 Z ",
    "M100.11363636363637,220 L100.11363636363637,185.76704545454544 ",
    "L100.11363636363637,179.01988636363637 ",
    "L100.11363636363637,74.54545454545453 ",
    "L116.87500000000001,74.54545454545453 L116.87500000000001,220 ",
    "L100.11363636363637,220 Z",
);

/// Where contours overlap, an edge can run through another contour's fill,
/// so its band extends past it on both sides. A piece of offset that
/// enters the band can then leave it across the edge itself, where nothing
/// cuts it. Judged by the crossing where it entered, such a piece used to
/// be dropped, and the centered stroke on Inter's `4` came apart where the
/// stem crosses the bar.
#[test]
fn overlapping_contours_keep_their_bands() {
    let p = BezPath::from_svg(INTER_FOUR).expect("valid path data");
    for tol in [1.0 / 16.0, 0.25] {
        check_center_at("inter-4", &p, 2.0, 96, tol);
    }
}

/// A `#` drawn as one outline with a counter. Its bars are `t` thick, and
/// the upright ones slant.
fn thin_hash(t: f64) -> BezPath {
    // The upright bars lean right by 0.3 per unit of height.
    let x = |x: f64, y: f64| (x + 0.3 * (140.0 - y), y);
    let (l, r) = (40.0, 80.0); // left edges of the two upright bars
    let (u, d) = (60.0, 100.0); // top edges of the two cross bars
    let (top, bottom, left, right) = (20.0, 140.0, 10.0, 120.0);
    let outline = [
        x(l, top),
        x(l + t, top),
        x(l + t, u),
        x(r, u),
        x(r, top),
        x(r + t, top),
        x(r + t, u),
        (right, u),
        (right, u + t),
        x(r + t, u + t),
        x(r + t, d),
        (right, d),
        (right, d + t),
        x(r + t, d + t),
        x(r + t, bottom),
        x(r, bottom),
        x(r, d + t),
        x(l + t, d + t),
        x(l + t, bottom),
        x(l, bottom),
        x(l, d + t),
        (left, d + t),
        (left, d),
        x(l, d),
        x(l, u + t),
        (left, u + t),
        (left, u),
        x(l, u),
    ];
    let counter = [x(l + t, u + t), x(l + t, d), x(r, d), x(r, u + t)];
    let mut p = BezPath::new();
    for pts in [&outline[..], &counter[..]] {
        p.move_to(pts[0]);
        for &q in &pts[1..] {
            p.line_to(q);
        }
        p.close_path();
    }
    p
}

/// Bars a little thinner than twice the width, so the inside stroke covers
/// them whole. What survives of the inner offsets there passes the
/// distance test by less than the pruning slack, on both sides of each
/// bar. The two sides cancel only while both remain. Dropping one side's
/// pieces at the crossings where the bars meet left the other side's,
/// which stitched into loops that punched holes through the bars.
#[test]
fn bars_thinner_than_the_band_fill_solid() {
    for (t, w) in [(3.8, 2.0), (7.8, 4.0)] {
        check_side_at(
            "thin-hash",
            &thin_hash(t),
            w,
            96,
            0.25,
            StrokeAlignment::Inside,
        );
    }
}
