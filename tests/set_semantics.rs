// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The alignment contract, checked directly against its set definition:
//!
//! ```text
//! Inside(w)  = { p ∈ F : dist(p, path) ≤ w }
//! Outside(w) = { p ∉ F : dist(p, path) ≤ w }
//! ```
//!
//! Sampled on a grid, with a band of `±slack` around `dist = w` excluded
//! (approximation error near the boundary is expected and harmless).

use kurbo::{BezPath, Circle, Join, ParamCurveNearest, Point, Rect, Shape};
use kurbo_se::{StrokeAlignment, StrokeStyle, stroke_aligned};

const TOL: f64 = 1e-3;

fn dist_to_path(path: &BezPath, p: Point) -> f64 {
    path.segments()
        .map(|s| s.nearest(p, 1e-6).distance_sq)
        .fold(f64::INFINITY, f64::min)
        .sqrt()
}

/// Check both alignments of `path` at width `w` against the set definition.
///
/// Round joins are used throughout: the set definition is the Minkowski sum
/// with a disc, which is exactly what a round join produces. Miter and bevel
/// joins deliberately deviate at convex corners (a miter spike reaches past
/// `w`), so they are covered by the geometry tests instead.
fn check(name: &str, path: &BezPath, w: f64, grid: usize) {
    for alignment in [StrokeAlignment::Inside, StrokeAlignment::Outside] {
        let style = StrokeStyle::new(w)
            .with_alignment(alignment)
            .with_join(Join::Round);
        let out = stroke_aligned(path, &style, TOL);
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
                    "{name}/{alignment:?} w={w} at {p:?}: dist {d:.2}, in_fill {in_fill} \
                     — expected painted={want}, got {got}"
                );
                checked += 1;
            }
        }
        assert!(checked > grid * grid / 4, "{name}: too few samples checked");
    }
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
