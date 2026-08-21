// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Regression ports: adversarial inputs from upstream kurbo and vello
//! reports, run through the full pipeline at every alignment.
//!
//! Sources:
//! - kurbo #344 (still open upstream against the old offset pipeline)
//! - kurbo #230 comment cubics (todo!-panic / stack overflow in old solver)
//! - vello #388 `broken_strokes` set (kurbo's own test suite)
//! - kurbo's `pathological_stroke` cusp-at-endpoint case

use kurbo::{BezPath, CubicBez, Shape};
use kurbo_se::{StrokeAlignment, StrokeStyle, stroke_aligned};

const ALIGNMENTS: [StrokeAlignment; 3] = [
    StrokeAlignment::Center,
    StrokeAlignment::Inside,
    StrokeAlignment::Outside,
];

fn assert_finite_all_alignments(path: &BezPath, width: f64, tolerance: f64, label: &str) {
    for alignment in ALIGNMENTS {
        let style = StrokeStyle::new(width).with_alignment(alignment);
        let out = stroke_aligned(path, &style, tolerance);
        assert!(
            out.is_finite(),
            "{label} @ {alignment:?}: non-finite output"
        );
    }
}

/// kurbo #344: endless loop in the *old* CubicOffset/fit pipeline.
/// Original report: offset −8.0, accuracy 0.01.
#[test]
fn kurbo_344_cubic() {
    let c = CubicBez::new(
        (51.0, 0.0),
        (-0.0859375, 161.640625),
        (0.0, 164.0),
        (0.0, 164.0),
    );
    let path = c.into_path(1e-9);
    assert_finite_all_alignments(&path, 16.0, 0.01, "#344");
}

/// kurbo #230 comments: adversarial cubics vs the old quartic solver
/// (offset ±0.02, tolerance 1e-3 in the original report).
#[test]
fn kurbo_230_cubics() {
    let cubics = [
        CubicBez::new(
            (0.36071499208860763, 0.4284909018987342),
            (0.40765427215189876, 0.2682357594936709),
            (0.3130191851265823, 0.6503065664556962),
            (0.27402590981012653, 0.7838162579113924),
        ),
        CubicBez::new(
            (0.20421023447401776, 0.6660408745247148),
            (0.4, 0.4),
            (0.20270516476552597, 0.42017585551330794),
            (0.39073193916349824, 0.7806846086818757),
        ),
    ];
    for (i, c) in cubics.iter().enumerate() {
        let path = c.into_path(1e-9);
        assert_finite_all_alignments(&path, 0.04, 1e-3, &format!("#230[{i}]"));
    }
}

/// vello #388 / kurbo `broken_strokes`: degenerate and near-cusp cubics.
#[test]
fn vello_388_broken_strokes() {
    let broken_cubics: [[(f64, f64); 4]; 10] = [
        [
            (465.24423, 107.11105),
            (475.50754, 107.11105),
            (475.50754, 107.11105),
            (475.50754, 107.11105),
        ],
        [(0., -0.01), (128., 128.001), (128., -0.01), (0., 128.001)],
        [(0., 0.), (0., -10.), (0., -10.), (0., 10.)],
        [(10., 0.), (0., 0.), (20., 0.), (10., 0.)],
        [(39., -39.), (40., -40.), (40., -40.), (0., 0.)],
        [(40., 40.), (0., 0.), (200., 200.), (0., 0.)],
        [(0., 0.), (1e-2, 0.), (-1e-2, 0.), (0., 0.)],
        [
            (400.75, 100.05),
            (400.75, 100.05),
            (100.05, 300.95),
            (100.05, 300.95),
        ],
        [(0.5, 0.), (0., 0.), (20., 0.), (10., 0.)],
        [(10., 0.), (0., 0.), (10., 0.), (10., 0.)],
    ];
    for (i, pts) in broken_cubics.iter().enumerate() {
        let c = CubicBez::new(pts[0], pts[1], pts[2], pts[3]);
        let path = c.into_path(0.1);
        assert_finite_all_alignments(&path, 30.0, 1e-3, &format!("broken[{i}]"));
    }
}

/// kurbo `pathological_stroke`: cusp at the endpoint.
#[test]
fn kurbo_pathological_stroke() {
    let c = CubicBez::new(
        (602.469, 286.585),
        (641.975, 286.585),
        (562.963, 286.585),
        (562.963, 286.585),
    );
    let path = c.into_path(0.1);
    assert_finite_all_alignments(&path, 1.0, 0.001, "pathological");
}

/// kurbo's huge-offset pathological curve (offset.rs regression).
#[test]
fn kurbo_huge_offset_curve() {
    let c = CubicBez::new(
        (-1236.3746269978635, 152.17981429574826),
        (-1175.18662093517, 108.04721798590596),
        (-1152.142883879584, 105.76260301083356),
        (-1151.842639804639, 105.73040758939104),
    );
    let path = c.into_path(1e-9);
    // Full width 2×3603.7 exercises the same offset distance as the report.
    assert_finite_all_alignments(&path, 2.0 * 3603.7267536453924, 0.1, "huge-offset");
}
