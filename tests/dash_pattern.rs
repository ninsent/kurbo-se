// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dash pattern semantics, pinned to the Figma/SVG model: entries at even
//! indices are dash lengths, entries at odd indices are gaps, and an
//! odd-length pattern reads as if doubled — `(2, 7, 4)` behaves exactly like
//! `(2, 7, 4, 2, 7, 4)`, so parity never flips as the pattern repeats.

use kurbo::{BezPath, PathEl, Shape};
use kurbo_se::{Cap, DashStyle, StrokeStyle, stroke_aligned};

fn hline(len: f64) -> BezPath {
    let mut p = BezPath::new();
    p.move_to((0.0, 0.0));
    p.line_to((len, 0.0));
    p
}

fn count_subpaths(p: &BezPath) -> usize {
    p.elements()
        .iter()
        .filter(|el| matches!(el, PathEl::MoveTo(_)))
        .count()
}

/// Pattern `(2, 4, 6, 8)` on a 30-long line paints `[0,2] [6,12] [20,22]
/// [26,30]`: four dashes, 14 units of length — even entries dash, odd gap.
#[test]
fn even_entries_dash_odd_entries_gap() {
    let line = hline(30.0);
    let style = StrokeStyle::new(2.0).with_dash(DashStyle::from_pattern([2.0, 4.0, 6.0, 8.0]));
    let out = stroke_aligned(&line, &style, 1e-4);
    assert_eq!(count_subpaths(&out), 4, "expected four dashes");
    let area = out.area().abs();
    assert!(
        (area - 28.0).abs() < 1e-6,
        "painted area {area}, want 28 (14 units x width 2)"
    );
}

/// An odd-length pattern is read twice, byte for byte: `(2, 7, 4)` produces
/// the identical outline to `(2, 7, 4, 2, 7, 4)` at any offset.
#[test]
fn odd_pattern_reads_doubled() {
    let line = hline(100.0);
    for offset in [0.0, 5.0, -13.0] {
        let odd = StrokeStyle::new(3.0)
            .with_dash(DashStyle::from_pattern([2.0, 7.0, 4.0]).with_offset(offset));
        let even = StrokeStyle::new(3.0)
            .with_dash(DashStyle::from_pattern([2.0, 7.0, 4.0, 2.0, 7.0, 4.0]).with_offset(offset));
        let a = stroke_aligned(&line, &odd, 1e-4);
        let b = stroke_aligned(&line, &even, 1e-4);
        assert_eq!(a.elements(), b.elements(), "offset {offset}");
    }
}

/// A zero-length "on" entry inside a longer pattern becomes a dot at its
/// pattern position (round dash cap), without disturbing the real dashes.
#[test]
fn zero_dash_in_long_pattern_makes_dots() {
    let line = hline(30.0);
    let style = StrokeStyle::new(2.0)
        .with_dash(DashStyle::from_pattern([0.0, 10.0, 6.0, 4.0]).with_cap(Cap::Round));
    let out = stroke_aligned(&line, &style, 1e-4);
    // Period 20: dots at s = 0 and 20, one dash [10,16]; s = 30 lands mid-gap.
    // Area = 6x2 for the dash + its two round half-caps (pi) + two dots (2pi).
    let area = out.area().abs();
    let want = 12.0 + 3.0 * std::f64::consts::PI;
    assert!((area - want).abs() < 0.05, "area {area}, want {want}");
}
