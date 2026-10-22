// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A reused [`AlignedStrokeCtx`] keeps working storage from call to call:
//! indexes, cut lists and dash buffers. None of it may leak into a result.
//! Every case here strokes through one context that other shapes and
//! styles have already filled. Its output must match a fresh context's,
//! element for element.

use kurbo::{BezPath, Circle, Point, Rect, Shape, Vec2};
use kurbo_se::{
    AlignedStrokeCtx, Cap, DashStyle, Join, StrokeAlignment, StrokeStyle, stroke_aligned,
    stroke_aligned_with,
};

fn star(c: Point, tips: usize, ro: f64, ri: f64) -> BezPath {
    let mut p = BezPath::new();
    for i in 0..2 * tips {
        let r = if i % 2 == 0 { ro } else { ri };
        let pt = c + r * Vec2::from_angle(core::f64::consts::PI * i as f64 / tips as f64);
        if i == 0 {
            p.move_to(pt);
        } else {
            p.line_to(pt);
        }
    }
    p.close_path();
    p
}

fn shapes() -> Vec<(&'static str, BezPath)> {
    let mut donut = Circle::new((100.0, 100.0), 80.0).to_path(0.1);
    donut.extend(
        Circle::new((100.0, 100.0), 35.0)
            .to_path(0.1)
            .reverse_subpaths()
            .iter(),
    );
    let mut plate = Rect::new(0.0, 0.0, 200.0, 120.0).to_path(0.1);
    for i in 0..12 {
        let c = Point::new(25.0 + 30.0 * (i % 6) as f64, 35.0 + 50.0 * (i / 6) as f64);
        plate.extend(Circle::new(c, 9.0).to_path(0.1).reverse_subpaths().iter());
    }
    let mut wavy = BezPath::new();
    wavy.move_to((0.0, 50.0));
    for i in 0..10 {
        let x = 40.0 * i as f64;
        wavy.curve_to((x + 13.0, 0.0), (x + 27.0, 100.0), (x + 40.0, 50.0));
    }
    let mut figure_eight = BezPath::new();
    figure_eight.move_to((0.0, 0.0));
    figure_eight.curve_to((120.0, 120.0), (120.0, -120.0), (0.0, 0.0));
    figure_eight.curve_to((-120.0, 120.0), (-120.0, -120.0), (0.0, 0.0));
    figure_eight.close_path();
    let mut not_finite = BezPath::new();
    not_finite.move_to((0.0, 0.0));
    not_finite.line_to((f64::NAN, 10.0));
    vec![
        ("rect", Rect::new(0.0, 0.0, 120.0, 40.0).to_path(0.1)),
        ("star", star(Point::new(150.0, 150.0), 5, 100.0, 40.0)),
        ("circle", Circle::new((0.0, 0.0), 50.0).to_path(0.1)),
        ("donut", donut),
        ("plate", plate),
        ("wavy", wavy),
        ("figure-eight", figure_eight),
        ("tiny", star(Point::new(1.0, 1.0), 6, 0.02, 0.008)),
        ("not-finite", not_finite),
        ("empty", BezPath::new()),
    ]
}

fn styles() -> Vec<StrokeStyle> {
    let mut out = Vec::new();
    for alignment in [
        StrokeAlignment::Inside,
        StrokeAlignment::Center,
        StrokeAlignment::Outside,
    ] {
        for width in [1.5, 9.0] {
            // Each kind of join stores different corner data in the index.
            for join in [Join::Round, Join::Miter, Join::Bevel] {
                out.push(
                    StrokeStyle::new(width)
                        .with_alignment(alignment)
                        .with_join(join)
                        .with_caps(Cap::Square),
                );
            }
            let dashed = StrokeStyle::new(width).with_alignment(alignment).with_dash(
                DashStyle::new(10.0, 6.0)
                    .with_offset(3.5)
                    .with_cap(Cap::Round),
            );
            out.push(dashed);
            // Dots: zero-length dashes, drawn by their round caps.
            out.push(
                StrokeStyle::new(width)
                    .with_alignment(alignment)
                    .with_dash(DashStyle::from_pattern([0.0, 7.0]).with_cap(Cap::Round)),
            );
            // One dash longer than any outline here. Each closed contour
            // comes out as a ring with no gap: two nested loops, which the
            // dash mask keeps together.
            out.push(
                StrokeStyle::new(width)
                    .with_alignment(alignment)
                    .with_dash(DashStyle::new(1e5, 1.0)),
            );
        }
    }
    out
}

/// Runs every case through one context, forwards and then backwards, so
/// each case sees storage left behind by both larger and smaller jobs.
#[test]
fn reused_context_matches_a_fresh_one() {
    let cases: Vec<(String, BezPath, StrokeStyle)> = shapes()
        .into_iter()
        .flat_map(|(name, path)| {
            styles()
                .into_iter()
                .enumerate()
                .map(move |(k, style)| (format!("{name}/style{k}"), path.clone(), style))
        })
        .collect();
    let fresh: Vec<BezPath> = cases
        .iter()
        .map(|(_, path, style)| stroke_aligned(path, style, 0.25))
        .collect();
    let mut ctx = AlignedStrokeCtx::new();
    let forwards = cases.iter().zip(&fresh);
    for ((name, path, style), want) in forwards.clone().chain(forwards.rev()) {
        // Debug output tells every finite value apart, the sign of zero
        // included, so this compares bit for bit.
        let got = stroke_aligned_with(path, style, 0.25, &mut ctx);
        assert_eq!(
            format!("{:?}", got.elements()),
            format!("{:?}", want.elements()),
            "{name}"
        );
    }
}
