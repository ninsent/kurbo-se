// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The degenerate-input matrix: no panics, no NaN in the output, and
//! deterministic behavior. Every case runs at all alignments and joins.

use kurbo::{BezPath, Cap, Join, PathEl, Shape};
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

/// Zero-length segments must not disturb dashing (2026-08-21 report).
///
/// `QuadBez::arclen` returns `NaN` for a fully degenerate quad, which used to
/// poison the arc-length accumulator behind the dash phase: every dash past
/// such an element came back as one uninterrupted band. Consecutive
/// degenerate segments also read as the path revisiting a vertex, which split
/// dashes there for no reason. The dashed result must match the same path
/// with the degenerate elements removed by hand.
#[test]
fn zero_length_segments_do_not_break_dashing() {
    let mut degenerate = BezPath::new();
    degenerate.move_to((20.0, 100.0));
    degenerate.line_to((20.0, 100.0));
    degenerate.line_to((100.0, 40.0));
    degenerate.line_to((100.0, 40.0));
    degenerate.quad_to((100.0, 40.0), (100.0, 40.0));
    degenerate.line_to((180.0, 100.0));
    degenerate.curve_to((180.0, 100.0), (180.0, 100.0), (180.0, 100.0));
    degenerate.line_to((100.0, 160.0));

    // A quad whose control deltas underflow is finite input, is *not*
    // point-coincident, and still has a NaN arc length upstream.
    let mut subnormal = BezPath::new();
    subnormal.move_to((20.0, 100.0));
    subnormal.quad_to((20.0 + 1e-300, 100.0), (20.0 + 2e-300, 100.0));
    subnormal.line_to((100.0, 40.0));
    subnormal.line_to((180.0, 100.0));
    subnormal.line_to((100.0, 160.0));

    let mut clean = BezPath::new();
    clean.move_to((20.0, 100.0));
    clean.line_to((100.0, 40.0));
    clean.line_to((180.0, 100.0));
    clean.line_to((100.0, 160.0));

    let count_subpaths = |p: &BezPath| {
        p.elements()
            .iter()
            .filter(|el| matches!(el, PathEl::MoveTo(_)))
            .count()
    };

    for alignment in [
        StrokeAlignment::Center,
        StrokeAlignment::Inside,
        StrokeAlignment::Outside,
    ] {
        for cap in [Cap::Round, Cap::Butt, Cap::Square] {
            let style = StrokeStyle::new(6.5)
                .with_alignment(alignment)
                .with_dash(DashStyle::from_pattern([13.0, 17.0]).with_cap(cap));
            let b = stroke_aligned(&clean, &style, 0.01);
            for (label, src) in [("zero-length", &degenerate), ("subnormal", &subnormal)] {
                let a = stroke_aligned(src, &style, 0.01);
                assert!(
                    a.is_finite(),
                    "{label}/{alignment:?}/{cap:?}: non-finite output"
                );
                assert_eq!(
                    count_subpaths(&a),
                    count_subpaths(&b),
                    "{label}/{alignment:?}/{cap:?}: degenerate segments changed the dash count"
                );
                // Same painted area, so no dash silently became a solid band.
                assert!(
                    (a.area().abs() - b.area().abs()).abs() < 1.0,
                    "{label}/{alignment:?}/{cap:?}: area {} vs clean {}",
                    a.area().abs(),
                    b.area().abs()
                );
            }
        }
    }
}

/// Subnormal-length segments must not poison *solid* strokes (2026-08-21
/// audit).
///
/// A delta of ~1e-300 passes an exact `p1 != p0` check but its squared
/// length underflows to zero, so normalizing the tangent divided by zero and
/// the whole outline went non-finite. Dashing and splitting already dropped
/// such segments; the plain band expansion now applies the same gate. The
/// output must match the clean path with the segment removed by hand.
#[test]
fn subnormal_segments_solid_strokes_stay_finite() {
    // Open polyline with a subnormal line segment.
    let mut open_sub = BezPath::new();
    open_sub.move_to((0.0, 0.0));
    open_sub.line_to((1e-300, 0.0));
    open_sub.line_to((10.0, 0.0));
    open_sub.line_to((10.0, 10.0));
    let mut open_clean = BezPath::new();
    open_clean.move_to((0.0, 0.0));
    open_clean.line_to((10.0, 0.0));
    open_clean.line_to((10.0, 10.0));

    // Closed rect with a subnormal segment spliced in.
    let mut closed_sub = BezPath::new();
    closed_sub.move_to((0.0, 0.0));
    closed_sub.line_to((1e-300, 0.0));
    closed_sub.line_to((10.0, 0.0));
    closed_sub.line_to((10.0, 10.0));
    closed_sub.line_to((0.0, 10.0));
    closed_sub.close_path();
    let closed_clean = kurbo::Rect::new(0.0, 0.0, 10.0, 10.0).to_path(1e-9);

    // Closed triangle whose *closing chord* is subnormal.
    let mut seam_sub = BezPath::new();
    seam_sub.move_to((0.0, 0.0));
    seam_sub.line_to((10.0, 0.0));
    seam_sub.line_to((10.0, 10.0));
    seam_sub.line_to((1e-300, 0.0));
    seam_sub.close_path();

    matrix(&open_sub, 4.0, "open subnormal");
    matrix(&closed_sub, 4.0, "closed subnormal");
    matrix(&seam_sub, 4.0, "subnormal seam");

    for alignment in ALIGNMENTS {
        let style = StrokeStyle::new(4.0).with_alignment(alignment);
        for (label, sub, clean) in [
            ("open", &open_sub, &open_clean),
            ("closed", &closed_sub, &closed_clean),
        ] {
            let a = stroke_aligned(sub, &style, 1e-3);
            let b = stroke_aligned(clean, &style, 1e-3);
            assert!(a.is_finite(), "{label}/{alignment:?}: non-finite output");
            assert!(
                (a.area().abs() - b.area().abs()).abs() < 1e-6,
                "{label}/{alignment:?}: area {} differs from clean {}",
                a.area().abs(),
                b.area().abs()
            );
        }
    }
}

/// A subpath whose entire extent is subnormal behaves like a zero-length
/// one: a dot for centered round caps, nothing otherwise — not NaN.
#[test]
fn subnormal_subpath_is_a_dot() {
    let mut p = BezPath::new();
    p.move_to((5.0, 5.0));
    p.line_to((5.0 + 1e-300, 5.0));

    let round = StrokeStyle::new(4.0).with_caps(Cap::Round);
    let out = stroke_aligned(&p, &round, 1e-4);
    assert!(
        (out.area().abs() - std::f64::consts::PI * 4.0).abs() < 0.01,
        "expected a radius-2 dot, area {}",
        out.area()
    );

    let butt = StrokeStyle::new(4.0);
    assert!(stroke_aligned(&p, &butt, 1e-4).is_empty());
    let sided = round.clone().with_alignment(StrokeAlignment::Inside);
    assert!(stroke_aligned(&p, &sided, 1e-4).is_empty());
}

/// Square dash caps on a curve must not shatter a dash (2026-08-21 report).
///
/// A square cap overshoots the fill on a convex curve, so the fill mask runs.
/// A source piece straddling the end of the band's shared edge was dropped
/// whole as "coincident", stranding that edge as its own sliver: dashes came
/// out as wedges plus slivers, twice as many subpaths as dashes.
#[test]
fn square_dash_caps_on_curves_stay_whole() {
    let circle = kurbo::Circle::new((190.0, 110.0), 35.0).to_path(1e-6);
    let count_subpaths = |p: &BezPath| {
        p.elements()
            .iter()
            .filter(|el| matches!(el, PathEl::MoveTo(_)))
            .count()
    };
    let dashes_for = |cap: Cap| {
        let style = StrokeStyle::new(6.5)
            .with_alignment(StrokeAlignment::Inside)
            .with_dash(DashStyle::from_pattern([13.0, 17.0]).with_cap(cap));
        let out = stroke_aligned(&circle, &style, 0.01);
        (count_subpaths(&out), out.area().abs())
    };
    let (round_n, round_area) = dashes_for(Cap::Round);
    let (square_n, square_area) = dashes_for(Cap::Square);
    assert_eq!(
        square_n, round_n,
        "square caps produced {square_n} subpaths against {round_n} for round"
    );
    // Square caps add a little area per dash, never a lot: the wedge failure
    // both lost and gained large slices.
    let extra = square_area - round_area;
    assert!(
        extra > 0.0 && extra < 0.35 * round_area,
        "square-cap area {square_area:.1} vs round {round_area:.1}"
    );
}
