// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Winding-based region properties, sampled numerically rather than judged
//! by eye:
//!
//! - an inside-aligned stroke never leaves the source fill, and an
//!   outside-aligned one never enters it;
//! - inside(w) ∪ outside(w) ≈ center(2w) as regions;
//! - reversing subpath winding leaves Inside/Outside output unchanged;
//! - randomized paths produce no panics, no NaN, and no hangs. The
//!   generator is a deterministic xorshift, and each case runs under a
//!   watchdog, since upstream offset code has a history of infinite loops.

use std::sync::mpsc;
use std::time::Duration;

use kurbo::{BezPath, Point, Rect, Shape};
use kurbo_se::{StrokeAlignment, StrokeStyle, stroke_aligned};

/// Sample winding-nonzero membership over an `n × n` grid on `bbox`.
fn winding_grid(path: &BezPath, bbox: Rect, n: usize) -> Vec<bool> {
    let mut out = Vec::with_capacity(n * n);
    for iy in 0..n {
        for ix in 0..n {
            let x = bbox.x0 + (ix as f64 + 0.5) / n as f64 * bbox.width();
            let y = bbox.y0 + (iy as f64 + 0.5) / n as f64 * bbox.height();
            out.push(path.winding(Point::new(x, y)) != 0);
        }
    }
    out
}

/// Distance-to-boundary fudge: mismatches are tolerated only for cells whose
/// center could plausibly sit near a region boundary. Instead of computing
/// true distances (expensive), allow a bounded mismatch count proportional
/// to the perimeter/cell ratio.
fn assert_grids_agree(a: &[bool], b: &[bool], n: usize, max_mismatch_frac: f64, label: &str) {
    let mismatches = a.iter().zip(b).filter(|(x, y)| x != y).count();
    let max = ((n * n) as f64 * max_mismatch_frac).ceil() as usize;
    assert!(
        mismatches <= max,
        "{label}: {mismatches} grid mismatches (allowed {max})"
    );
}

fn star() -> BezPath {
    let mut p = BezPath::new();
    let n = 5;
    for i in 0..(2 * n) {
        let r = if i % 2 == 0 { 50.0 } else { 22.0 };
        let a = core::f64::consts::PI * (i as f64) / (n as f64) - 0.3;
        let (s, c) = a.sin_cos();
        let pt = (100.0 + r * c, 100.0 + r * s);
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
    vec![
        (
            "circle",
            kurbo::Circle::new((100.0, 100.0), 50.0).to_path(1e-9),
        ),
        ("rect", Rect::new(50.0, 60.0, 150.0, 140.0).to_path(1e-9)),
        ("star", star()),
    ]
}

/// Inside stroke ⊆ source fill.
#[test]
fn inside_stays_inside() {
    for (name, src) in shapes() {
        let style = StrokeStyle::new(8.0).with_alignment(StrokeAlignment::Inside);
        let out = stroke_aligned(&src, &style, 1e-4);
        let bbox = src.bounding_box().inflate(4.0, 4.0);
        let n = 64;
        let out_grid = winding_grid(&out, bbox, n);
        let src_grid = winding_grid(&src, bbox, n);
        let escapes = out_grid
            .iter()
            .zip(&src_grid)
            .filter(|(o, s)| **o && !**s)
            .count();
        // The exact-side construction makes escapes impossible except for
        // cells straddling the shared boundary; sampling at cell centers,
        // none should escape.
        assert_eq!(escapes, 0, "{name}: inside stroke escaped the fill");
    }
}

/// Outside stroke ∩ source fill = ∅.
#[test]
fn outside_stays_outside() {
    for (name, src) in shapes() {
        let style = StrokeStyle::new(8.0).with_alignment(StrokeAlignment::Outside);
        let out = stroke_aligned(&src, &style, 1e-4);
        let bbox = src.bounding_box().inflate(12.0, 12.0);
        let n = 64;
        let out_grid = winding_grid(&out, bbox, n);
        let src_grid = winding_grid(&src, bbox, n);
        let intrusions = out_grid
            .iter()
            .zip(&src_grid)
            .filter(|(o, s)| **o && **s)
            .count();
        assert_eq!(intrusions, 0, "{name}: outside stroke entered the fill");
    }
}

/// inside(w) ∪ outside(w) ≈ center(2w) as regions.
#[test]
fn inside_union_outside_is_center_doubled() {
    for (name, src) in shapes() {
        let w = 8.0;
        let inside = stroke_aligned(
            &src,
            &StrokeStyle::new(w).with_alignment(StrokeAlignment::Inside),
            1e-4,
        );
        let outside = stroke_aligned(
            &src,
            &StrokeStyle::new(w).with_alignment(StrokeAlignment::Outside),
            1e-4,
        );
        let center = stroke_aligned(
            &src,
            &StrokeStyle::new(2.0 * w).with_alignment(StrokeAlignment::Center),
            1e-4,
        );
        let bbox = src.bounding_box().inflate(12.0, 12.0);
        let n = 72;
        let in_grid = winding_grid(&inside, bbox, n);
        let out_grid = winding_grid(&outside, bbox, n);
        let center_grid = winding_grid(&center, bbox, n);
        let union: Vec<bool> = in_grid
            .iter()
            .zip(&out_grid)
            .map(|(a, b)| *a || *b)
            .collect();
        // Joins differ slightly between the halves and the doubled center at
        // sharp corners; allow a small boundary fringe.
        assert_grids_agree(&union, &center_grid, n, 0.01, name);
    }
}

/// Winding reversal changes nothing, as regions.
#[test]
fn reversal_invariance_as_regions() {
    for (name, src) in shapes() {
        let rev = src.reverse_subpaths();
        for alignment in [StrokeAlignment::Inside, StrokeAlignment::Outside] {
            let style = StrokeStyle::new(8.0).with_alignment(alignment);
            let a = stroke_aligned(&src, &style, 1e-4);
            let b = stroke_aligned(&rev, &style, 1e-4);
            let bbox = src.bounding_box().inflate(12.0, 12.0);
            let n = 64;
            let ga = winding_grid(&a, bbox, n);
            let gb = winding_grid(&b, bbox, n);
            assert_grids_agree(&ga, &gb, n, 0.0, &format!("{name}/{alignment:?}"));
        }
    }
}

/// Deterministic fuzz with a watchdog. Each case runs on its own thread
/// under a hard timeout: upstream offset code has a history of infinite
/// loops (kurbo #344), and "no hang" is part of the contract.
#[test]
fn randomized_paths_no_panic_no_hang() {
    let mut rng = 0x2545F4914F6CDD1Du64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 11) as f64 / (1u64 << 53) as f64
    };

    for case in 0..120 {
        let mut p = BezPath::new();
        let subpaths = 1 + (next() * 2.0) as usize;
        for _ in 0..subpaths {
            let n = 1 + (next() * 5.0) as usize;
            p.move_to((next() * 200.0, next() * 200.0));
            for _ in 0..n {
                match (next() * 3.0) as usize {
                    0 => p.line_to((next() * 200.0, next() * 200.0)),
                    1 => p.quad_to(
                        (next() * 200.0, next() * 200.0),
                        (next() * 200.0, next() * 200.0),
                    ),
                    _ => p.curve_to(
                        (next() * 200.0, next() * 200.0),
                        (next() * 200.0, next() * 200.0),
                        (next() * 200.0, next() * 200.0),
                    ),
                }
            }
            if next() > 0.5 {
                p.close_path();
            }
        }
        // Occasionally inject degenerate elements.
        if case % 7 == 0 {
            let pt = p.elements()[0].end_point().unwrap();
            p.line_to(pt);
            p.curve_to(pt, pt, pt);
        }
        let width = 0.1 + next() * 40.0;
        let alignment = ALIGNMENTS[case % 3];

        let (tx, rx) = mpsc::channel();
        let path = p.clone();
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(move || {
                let style = StrokeStyle::new(width).with_alignment(alignment);
                let out = stroke_aligned(&path, &style, 1e-3);
                out.is_finite()
            });
            let _ = tx.send(r);
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(finite)) => assert!(finite, "case {case}: non-finite output\npath: {p:?}"),
            Ok(Err(_)) => panic!("case {case}: panicked\npath: {p:?}"),
            Err(_) => panic!("case {case}: HANG (>10s)\npath: {p:?}"),
        }
    }
}

const ALIGNMENTS: [StrokeAlignment; 3] = [
    StrokeAlignment::Center,
    StrokeAlignment::Inside,
    StrokeAlignment::Outside,
];

/// Coordinates a fuzzer would reach for: exact zeros, values that underflow
/// when squared, values that overflow when squared, neighbours one ULP
/// apart, and ordinary magnitudes to mix them with.
const HOSTILE: [f64; 14] = [
    0.0,
    -0.0,
    1.0,
    -1.0,
    1e-300,
    -1e-300,
    1e300,
    -1e300,
    f64::MIN_POSITIVE,
    1.0 + f64::EPSILON,
    1.0 - f64::EPSILON / 2.0,
    100.0,
    -100.0,
    1e8,
];

/// Paths built from hostile coordinates, at every alignment, dashed and
/// solid.
///
/// The contract is narrow and absolute: finite input yields finite output,
/// and nothing panics or hangs. The hand-written matrix covers degenerate
/// *shapes*; this covers degenerate *numbers*, which is where the
/// interaction between the parametric epsilons and the pruning fuzz is
/// hardest to reason about.
#[test]
fn hostile_coordinates_stay_finite() {
    let mut rng = 0x9E3779B97F4A7C15u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    for case in 0..200 {
        let mut pick = || HOSTILE[(next() % HOSTILE.len() as u64) as usize];
        let mut p = BezPath::new();
        p.move_to((pick(), pick()));
        for _ in 0..(2 + case % 4) {
            match case % 3 {
                0 => p.line_to((pick(), pick())),
                1 => p.quad_to((pick(), pick()), (pick(), pick())),
                _ => p.curve_to((pick(), pick()), (pick(), pick()), (pick(), pick())),
            }
        }
        if case % 2 == 0 {
            p.close_path();
        }
        let finite_input = p.is_finite();
        let width = [0.0125, 1.0, 40.0][case % 3];
        let alignment = ALIGNMENTS[case % 3];
        let mut style = StrokeStyle::new(width).with_alignment(alignment);
        if case % 5 == 0 {
            // Scale the pattern to the path. The number of dashes is path
            // length over pattern period, so a fixed pattern on a path
            // spanning 1e8 asks for tens of millions of them — real work,
            // correctly performed, and not what this test is about.
            let unit = (p.control_box().size().max_side() / 50.0).max(1e-6);
            style = style.with_dash(kurbo_se::DashStyle::new(0.7 * unit, 0.3 * unit));
        }

        let (tx, rx) = mpsc::channel();
        let path = p.clone();
        std::thread::spawn(move || {
            let r =
                std::panic::catch_unwind(move || stroke_aligned(&path, &style, 1e-3).is_finite());
            let _ = tx.send(r);
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(out_finite)) => assert!(
                out_finite || !finite_input,
                "case {case}: finite input produced non-finite output\npath: {p:?}"
            ),
            Ok(Err(_)) => panic!("case {case}: panicked\npath: {p:?}"),
            Err(_) => panic!("case {case}: HANG (>10s)\npath: {p:?}"),
        }
    }
}
