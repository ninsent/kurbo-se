// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Golden characterization tests: the exact `BezPath` output of
//! [`stroke_aligned`] for a matrix of shapes and styles, pinned in
//! `tests/golden.txt`.
//!
//! The semantic suites (`set_semantics`, `properties`, …) verify what the
//! output *means*; this suite verifies it hasn't *moved*. Any refactor that
//! is supposed to be behavior-preserving must leave these untouched.
//!
//! Comparison is per-coordinate with a `1e-9` absolute tolerance rather than
//! byte-exact: transcendental functions (`sin`, `atan2`, …) differ in the
//! last bits across platform libms, which is noise; genuine behavior drift
//! is either exactly zero or far larger. That slack cannot absorb discrete
//! decisions, though — a last-bit difference can flip a subdivision choice
//! in the offsetter and change the segment count. The fixture therefore
//! records the OS it was generated on and the test skips on any other
//! (the CI matrix still runs it on a matching runner).
//!
//! The fixture lives in the repository but is excluded from the crates.io
//! package (it is a refactoring net, not a shipped artifact); when it is
//! absent the test skips. Regenerate after an *intended* output change with:
//!
//! ```text
//! KURBO_SE_GOLDEN=write cargo test --test golden
//! ```

// kurbo's SVG serialization (`to_svg`/`from_svg`) is std-only; so is this
// net — the `libm` CI job builds without it.
#![cfg(feature = "std")]

use std::fmt::Write as _;
use std::path::PathBuf;

use kurbo::{BezPath, Cap, Circle, Join, PathEl, Rect, Shape};
use kurbo_se::{DashStyle, StrokeAlignment, StrokeSide, StrokeStyle, stroke_aligned};

/// Absolute per-coordinate slack for cross-platform libm differences.
const COORD_EPS: f64 = 1e-9;

fn star(points: usize, r_outer: f64, r_inner: f64, phase: f64) -> BezPath {
    let mut p = BezPath::new();
    for i in 0..(points * 2) {
        let r = if i % 2 == 0 { r_outer } else { r_inner };
        let a = std::f64::consts::PI * (i as f64) / points as f64 + phase;
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

fn shapes() -> Vec<(&'static str, BezPath)> {
    let circle = Circle::new((120.0, 120.0), 70.0).to_path(1e-6);
    let rect = Rect::new(40.0, 80.0, 160.0, 140.0).to_path(1e-9);

    let mut donut = Circle::new((130.0, 130.0), 100.0).to_path(1e-6);
    donut.extend(
        Circle::new((130.0, 130.0), 45.0)
            .to_path(1e-6)
            .reverse_subpaths()
            .iter(),
    );

    let mut bowtie = BezPath::new();
    bowtie.move_to((20.0, 20.0));
    bowtie.line_to((220.0, 180.0));
    bowtie.line_to((220.0, 20.0));
    bowtie.line_to((20.0, 180.0));
    bowtie.close_path();

    let mut figure_eight = BezPath::new();
    figure_eight.move_to((120.0, 100.0));
    figure_eight.curve_to((150.0, 55.0), (210.0, 55.0), (240.0, 100.0));
    figure_eight.curve_to((210.0, 145.0), (150.0, 145.0), (120.0, 100.0));
    figure_eight.curve_to((90.0, 55.0), (30.0, 55.0), (0.0, 100.0));
    figure_eight.curve_to((30.0, 145.0), (90.0, 145.0), (120.0, 100.0));
    figure_eight.close_path();

    let mut wedge = BezPath::new();
    wedge.move_to((20.0, 180.0));
    wedge.line_to((230.0, 160.0));
    wedge.line_to((20.0, 140.0));
    wedge.close_path();

    let mut polyline = BezPath::new();
    polyline.move_to((20.0, 40.0));
    polyline.line_to((120.0, 20.0));
    polyline.line_to((220.0, 40.0));
    polyline.line_to((180.0, 120.0));

    let mut loop_curve = BezPath::new();
    loop_curve.move_to((20.0, 150.0));
    loop_curve.curve_to((200.0, 20.0), (0.0, 20.0), (180.0, 150.0));

    let mut mixed = BezPath::new();
    mixed.move_to((20.0, 40.0));
    mixed.line_to((120.0, 20.0));
    mixed.line_to((220.0, 40.0));
    mixed.extend(Rect::new(40.0, 80.0, 120.0, 140.0).to_path(1e-9).iter());
    mixed.extend(
        Circle::new((190.0, 110.0), 35.0)
            .to_path(1e-6)
            .reverse_subpaths()
            .iter(),
    );

    vec![
        ("circle", circle),
        ("rect", rect),
        ("star", star(5, 100.0, 40.0, -0.3)),
        ("donut", donut),
        ("bowtie", bowtie),
        ("figure-eight", figure_eight),
        ("wedge", wedge),
        ("polyline", polyline),
        ("loop-curve", loop_curve),
        ("mixed", mixed),
    ]
}

fn styles() -> Vec<(&'static str, StrokeStyle)> {
    vec![
        (
            "inside-solid-miter",
            StrokeStyle::new(12.0).with_alignment(StrokeAlignment::Inside),
        ),
        (
            "outside-solid-round",
            StrokeStyle::new(10.0)
                .with_alignment(StrokeAlignment::Outside)
                .with_join(Join::Round)
                .with_caps(Cap::Round),
        ),
        (
            "center-solid-bevel",
            StrokeStyle::new(14.0)
                .with_join(Join::Bevel)
                .with_caps(Cap::Square),
        ),
        (
            "inside-extreme-saturating",
            StrokeStyle::new(60.0).with_alignment(StrokeAlignment::Inside),
        ),
        (
            "inside-dashed-roundcap",
            StrokeStyle::new(10.0)
                .with_alignment(StrokeAlignment::Inside)
                .with_dash(DashStyle::from_pattern([20.0, 29.0]).with_cap(Cap::Round)),
        ),
        (
            "center-dashed-offset-square",
            StrokeStyle::new(8.0).with_join(Join::Round).with_dash(
                DashStyle::from_pattern([10.0, 5.0])
                    .with_offset(3.0)
                    .with_cap(Cap::Square),
            ),
        ),
        (
            "side-left-solid",
            StrokeStyle::new(9.0)
                .with_side(StrokeSide::Left)
                .with_miter_angle(60.0),
        ),
        (
            "outside-dotted",
            StrokeStyle::new(11.0)
                .with_alignment(StrokeAlignment::Outside)
                .with_dash(DashStyle::from_pattern([0.0, 24.0]).with_cap(Cap::Round)),
        ),
    ]
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden.txt")
}

/// Render the whole matrix to the fixture text format.
fn generate() -> String {
    let mut out = String::new();
    writeln!(out, "# platform: {}", std::env::consts::OS).unwrap();
    // Display tolerance keeps the fixture compact; two representative cases
    // run finer below to cover tolerance-dependent code paths.
    let tol = 0.25;
    for (shape_name, shape) in shapes() {
        for (style_name, style) in styles() {
            let result = stroke_aligned(&shape, &style, tol);
            writeln!(out, "== {shape_name}/{style_name}").unwrap();
            writeln!(out, "{}", result.to_svg()).unwrap();
        }
    }
    for (shape_name, shape) in shapes().into_iter().take(2) {
        let style = StrokeStyle::new(12.0).with_alignment(StrokeAlignment::Inside);
        let result = stroke_aligned(&shape, &style, 1e-3);
        writeln!(out, "== {shape_name}/inside-solid-miter/fine").unwrap();
        writeln!(out, "{}", result.to_svg()).unwrap();
    }
    out
}

fn parse_fixture(text: &str) -> Vec<(String, String)> {
    let mut cases = Vec::new();
    let mut name: Option<String> = None;
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        if let Some(n) = line.strip_prefix("== ") {
            name = Some(n.to_string());
        } else if let Some(n) = name.take() {
            cases.push((n, line.to_string()));
        }
    }
    cases
}

/// The `# platform:` header of a fixture, if present.
fn fixture_platform(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|l| l.strip_prefix("# platform: "))
        .map(str::trim)
}

/// Element-kind discriminant for sequence comparison.
fn kind(el: &PathEl) -> u8 {
    match el {
        PathEl::MoveTo(_) => 0,
        PathEl::LineTo(_) => 1,
        PathEl::QuadTo(..) => 2,
        PathEl::CurveTo(..) => 3,
        PathEl::ClosePath => 4,
    }
}

fn coords(el: &PathEl) -> Vec<f64> {
    match el {
        PathEl::MoveTo(p) | PathEl::LineTo(p) => vec![p.x, p.y],
        PathEl::QuadTo(p1, p2) => vec![p1.x, p1.y, p2.x, p2.y],
        PathEl::CurveTo(p1, p2, p3) => vec![p1.x, p1.y, p2.x, p2.y, p3.x, p3.y],
        PathEl::ClosePath => Vec::new(),
    }
}

fn assert_same(name: &str, expected: &BezPath, actual: &BezPath) {
    assert_eq!(
        expected.elements().len(),
        actual.elements().len(),
        "{name}: element count changed ({} -> {})",
        expected.elements().len(),
        actual.elements().len()
    );
    for (i, (e, a)) in expected
        .elements()
        .iter()
        .zip(actual.elements())
        .enumerate()
    {
        assert_eq!(
            kind(e),
            kind(a),
            "{name}: element {i} kind changed ({e:?} -> {a:?})"
        );
        for (ec, ac) in coords(e).iter().zip(coords(a)) {
            assert!(
                (ec - ac).abs() <= COORD_EPS,
                "{name}: element {i} moved by {:.3e} ({e:?} -> {a:?})",
                (ec - ac).abs()
            );
        }
    }
}

#[test]
fn golden_matrix_unchanged() {
    let path = fixture_path();
    let generated = generate();

    if std::env::var("KURBO_SE_GOLDEN").as_deref() == Ok("write") {
        std::fs::write(&path, &generated).expect("write golden fixture");
        eprintln!(
            "golden: wrote {} ({} bytes)",
            path.display(),
            generated.len()
        );
        return;
    }

    let Ok(fixture) = std::fs::read_to_string(&path) else {
        // Packaged crates exclude the fixture; the net only guards the repo.
        eprintln!("golden: fixture missing, skipping (repo-only test)");
        return;
    };

    if let Some(p) = fixture_platform(&fixture) {
        if p != std::env::consts::OS {
            // A different libm can flip subdivision decisions (see the
            // module docs); exactness only holds on the generating OS.
            eprintln!(
                "golden: fixture generated on {p}, running on {}; skipping",
                std::env::consts::OS
            );
            return;
        }
    }

    let expected = parse_fixture(&fixture);
    let actual = parse_fixture(&generated);
    assert_eq!(
        expected.len(),
        actual.len(),
        "golden case list changed; regenerate deliberately with KURBO_SE_GOLDEN=write"
    );
    for ((en, esvg), (an, asvg)) in expected.iter().zip(&actual) {
        assert_eq!(en, an, "golden case order changed");
        let e = BezPath::from_svg(esvg).expect("fixture SVG parses");
        let a = BezPath::from_svg(asvg).expect("generated SVG parses");
        assert_same(en, &e, &a);
    }
}
