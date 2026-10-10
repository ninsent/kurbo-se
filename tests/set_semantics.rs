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
