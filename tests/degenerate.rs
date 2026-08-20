// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The degenerate-input matrix: no panics, no NaN in output,
//! deterministic behavior. Every case runs at all alignments and joins.

use kurbo::{BezPath, Cap, Join, Shape};
use kurbo_se::{DashStyle, StrokeAlignment, StrokeStyle, stroke_aligned};

const ALIGNMENTS: [StrokeAlignment; 3] = [
    StrokeAlignment::Center,
    StrokeAlignment::Inside,
    StrokeAlignment::Outside,
];
const JOINS: [Join; 3] = [Join::Miter, Join::Bevel, Join::Round];
const CAPS: [Cap; 3] = [Cap::Butt, Cap::Round, Cap::Square];

/// Run one path through the whole style matrix; assert finite output.
fn matrix(path: &BezPath, width: f64, label: &str) {
    for alignment in ALIGNMENTS {
        for join in JOINS {
            for cap in CAPS {
                let style = StrokeStyle::new(width)
                    .with_alignment(alignment)
                    .with_join(join)
                    .with_caps(cap);
                let out = stroke_aligned(path, &style, 1e-3);
                assert!(
                    out.is_finite(),
                    "{label} @ {alignment:?}/{join:?}/{cap:?}: non-finite"
                );
                // Dashed variant.
                let style = style.with_dash(DashStyle::new(3.0, 2.0).with_cap(cap));
                let out = stroke_aligned(path, &style, 1e-3);
                assert!(
                    out.is_finite(),
                    "{label} dashed @ {alignment:?}/{join:?}/{cap:?}: non-finite"
                );
            }
        }
    }
}

#[test]
fn single_point_subpath() {
    let mut p = BezPath::new();
    p.move_to((5.0, 5.0));
    matrix(&p, 4.0, "bare moveto");
    p.line_to((5.0, 5.0));
    matrix(&p, 4.0, "zero-length line");
    p.close_path();
    matrix(&p, 4.0, "zero-length closed");
}

#[test]
fn repeated_identical_points() {
    let mut p = BezPath::new();
    p.move_to((0.0, 0.0));
    for _ in 0..5 {
        p.line_to((0.0, 0.0));
    }
    p.line_to((10.0, 0.0));
    for _ in 0..5 {
        p.line_to((10.0, 0.0));
    }
    p.curve_to((10.0, 0.0), (10.0, 0.0), (10.0, 0.0));
    matrix(&p, 4.0, "repeated points");
}

#[test]
fn zero_length_segments_interleaved() {
    let mut p = BezPath::new();
    p.move_to((0.0, 0.0));
    p.line_to((10.0, 0.0));
    p.line_to((10.0, 0.0));
    p.quad_to((10.0, 0.0), (10.0, 0.0));
    p.line_to((10.0, 10.0));
    matrix(&p, 4.0, "interleaved zero segs");
}

#[test]
fn coincident_endpoints_without_closepath() {
    let mut p = BezPath::new();
    p.move_to((0.0, 0.0));
    p.line_to((10.0, 0.0));
    p.line_to((10.0, 10.0));
    p.line_to((0.0, 0.0)); // back to start, but NOT closed
    matrix(&p, 4.0, "coincident endpoints");
    // Open semantics: the path stays open (caps, no seam join), so the
    // outline differs from the explicitly closed variant of the same
    // geometry — with bevel joins, closing shrinks nothing, but the open
    // version carries cap geometry the closed one lacks.
    let style = StrokeStyle::new(4.0)
        .with_caps(Cap::Square)
        .with_join(Join::Bevel);
    let open_out = stroke_aligned(&p, &style, 1e-3);
    let mut closed = p.clone();
    closed.close_path();
    let closed_out = stroke_aligned(&closed, &style, 1e-3);
    assert_ne!(open_out.elements(), closed_out.elements());
    // A closed stroke is two contours (ring); the open one is a single
    // capped outline.
    let count_moves = |b: &BezPath| {
        b.elements()
            .iter()
            .filter(|el| matches!(el, kurbo::PathEl::MoveTo(_)))
            .count()
    };
    assert_eq!(count_moves(&open_out), 1);
    assert_eq!(count_moves(&closed_out), 2);
}

/// Collinear control points (`offset_cubic`'s documented precondition) —
/// straight, reversing, and with interior cusps.
#[test]
fn collinear_control_points() {
    // Plain degenerate-linear cubic.
    let mut p = BezPath::new();
    p.move_to((0.0, 0.0));
    p.curve_to((10.0, 0.0), (20.0, 0.0), (30.0, 0.0));
    matrix(&p, 4.0, "collinear straight");

    // Interior cusp: goes out and comes back along the same line.
    let mut p = BezPath::new();
    p.move_to((0.0, 0.0));
    p.curve_to((40.0, 0.0), (-10.0, 0.0), (30.0, 0.0));
    matrix(&p, 4.0, "collinear cusped");

    // Vertical, tiny.
    let mut p = BezPath::new();
    p.move_to((0.0, 0.0));
    p.curve_to((0.0, 1e-4), (0.0, 2e-4), (0.0, 3e-4));
    matrix(&p, 0.5, "collinear tiny");
}

#[test]
fn exact_180_degree_turns() {
    let mut p = BezPath::new();
    p.move_to((0.0, 0.0));
    p.line_to((10.0, 0.0));
    p.line_to((0.0, 0.0));
    p.line_to((10.0, 0.0));
    matrix(&p, 4.0, "polyline 180s");
}

#[test]
fn width_larger_than_shape() {
    let p = kurbo::Rect::new(0.0, 0.0, 10.0, 6.0).to_path(1e-9);
    matrix(&p, 40.0, "width >> shape");
}

#[test]
fn nan_and_inf_inputs() {
    let style = StrokeStyle::new(4.0);
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((bad, 5.0));
        p.line_to((10.0, 10.0));
        assert!(
            stroke_aligned(&p, &style, 1e-3).is_empty(),
            "non-finite coord {bad} must yield empty output"
        );
    }
}

#[test]
fn tolerance_extremes() {
    let p = kurbo::Circle::new((50.0, 50.0), 20.0).to_path(1e-9);
    for tol in [1e-12, 1e-6, 0.25, 10.0, 1e6] {
        let style = StrokeStyle::new(8.0).with_alignment(StrokeAlignment::Inside);
        let out = stroke_aligned(&p, &style, tol);
        assert!(out.is_finite(), "tolerance {tol}: non-finite");
        assert!(!out.is_empty(), "tolerance {tol}: empty");
        // Bounded output: offset recursion is depth-capped upstream, so even
        // absurd tolerances terminate with sane sizes.
        assert!(
            out.elements().len() < 100_000,
            "tolerance {tol}: {} elements",
            out.elements().len()
        );
    }
}

/// Full closed-path matrix on a star with sharp concave corners.
#[test]
fn concave_star_matrix() {
    let mut p = BezPath::new();
    let n = 5;
    for i in 0..(2 * n) {
        let r = if i % 2 == 0 { 50.0 } else { 20.0 };
        let a = core::f64::consts::PI * (i as f64) / (n as f64);
        let (s, c) = a.sin_cos();
        let pt = (100.0 + r * c, 100.0 + r * s);
        if i == 0 {
            p.move_to(pt);
        } else {
            p.line_to(pt);
        }
    }
    p.close_path();
    matrix(&p, 6.0, "star");
    // Inside stays inside.
    let style = StrokeStyle::new(6.0).with_alignment(StrokeAlignment::Inside);
    let out = stroke_aligned(&p, &style, 1e-3);
    let src_bb = p.bounding_box();
    let out_bb = out.bounding_box();
    assert!(
        src_bb.contains_rect(out_bb),
        "inside star stroke escaped: {out_bb:?} vs {src_bb:?}"
    );
}
