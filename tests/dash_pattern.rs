// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dash pattern semantics, pinned to the Figma/SVG model: entries at even
//! indices are dash lengths, entries at odd indices are gaps, and an
//! odd-length pattern reads as if doubled — `(2, 7, 4)` behaves exactly like
//! `(2, 7, 4, 2, 7, 4)`, so parity never flips as the pattern repeats.

use kurbo::{
    BezPath, ParamCurve, ParamCurveDeriv, ParamCurveNearest, PathEl, PathSeg, Point, Shape, Vec2,
};
use kurbo_se::{Cap, DashStyle, StrokeSide, StrokeStyle, stroke_aligned};

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

/// A dash that runs to the end of an open path gets the path's end cap,
/// not the dash cap. kurbo ends this dash by evaluating the line at
/// `t = 1`, which lands an ulp short of `x = 7.3`, so an exact endpoint
/// match would take the end for a dash edge.
#[test]
fn a_dash_reaching_the_end_keeps_the_end_cap() {
    let mut line = BezPath::new();
    line.move_to((1.1, 0.0));
    line.line_to((7.3, 0.0));
    let solid = StrokeStyle::new(2.0).with_end_cap(Cap::Round);
    // One dash, longer than the line.
    let dashed = solid.clone().with_dash(DashStyle::new(100.0, 1.0));
    let want = stroke_aligned(&line, &solid, 1e-4).area().abs();
    let area = stroke_aligned(&line, &dashed, 1e-4).area().abs();
    assert!(
        (area - want).abs() < 1e-9,
        "painted area {area}, want {want} (6.2 x 2 plus a round end cap)"
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

fn subpath_starts(els: impl Iterator<Item = PathEl>) -> Vec<Point> {
    els.filter_map(|el| match el {
        PathEl::MoveTo(p) => Some(p),
        _ => None,
    })
    .collect()
}

/// Where `kurbo::dash` starts each dash of nonzero length. When the offset
/// lands exactly where a dash ends, kurbo also reports a zero-length dash
/// at the start. With butt caps it draws nothing, so it is left out.
fn dash_starts(path: &BezPath, offset: f64, pattern: &[f64]) -> Vec<Point> {
    let mut starts = Vec::new();
    let mut current: Option<(Point, f64)> = None;
    let mut last = Point::ORIGIN;
    for el in kurbo::dash(path.iter(), offset, pattern).chain([PathEl::MoveTo(Point::ORIGIN)]) {
        if let PathEl::MoveTo(p) = el {
            if let Some((start, len)) = current.take() {
                if len > 1e-9 {
                    starts.push(start);
                }
            }
            current = Some((p, 0.0));
        } else if let (Some((_, len)), Some(p)) = (&mut current, el.end_point()) {
            *len += (p - last).hypot();
        }
        last = el.end_point().unwrap_or(last);
    }
    starts
}

/// Distance from `p` to the nearest of `points`.
fn nearest(points: &[Point], p: Point) -> f64 {
    points
        .iter()
        .map(|q| (*q - p).hypot())
        .fold(f64::INFINITY, f64::min)
}

/// An open polyline of three lines, the first and last crossing at (50, 50).
fn crossing_polyline() -> BezPath {
    let mut p = BezPath::new();
    p.move_to((0.0, 0.0));
    p.line_to((100.0, 100.0));
    p.line_to((100.0, 0.0));
    p.line_to((0.0, 100.0));
    p
}

/// One cubic crossing itself, near (100, 67.89).
fn loop_curve() -> BezPath {
    let mut p = BezPath::new();
    p.move_to((20.0, 150.0));
    p.curve_to((200.0, 20.0), (0.0, 20.0), (180.0, 150.0));
    p
}

/// The golden suite's bowtie, crossing at (120, 100), started from a corner
/// or from the crossing.
fn bowtie(from_crossing: bool) -> BezPath {
    let mut p = BezPath::new();
    if from_crossing {
        p.move_to((120.0, 100.0));
        p.line_to((220.0, 180.0));
        p.line_to((220.0, 20.0));
        p.line_to((120.0, 100.0));
        p.line_to((20.0, 180.0));
        p.line_to((20.0, 20.0));
    } else {
        p.move_to((20.0, 20.0));
        p.line_to((220.0, 180.0));
        p.line_to((220.0, 20.0));
        p.line_to((20.0, 180.0));
    }
    p.close_path();
    p
}

/// A bowtie of whole-number edges, 50, 30, 50 and 30 long, crossing at
/// (20, 15). It starts at a corner 55 past its second pass through the
/// crossing. Its perimeter of 160 is a whole number of periods of 10, so at
/// offset 0 a zero-length dash lands exactly on the start point, reached
/// from both sides. Started from the crossing instead, the pattern restarts
/// at the crossing.
fn whole_bowtie(from_crossing: bool) -> BezPath {
    let mut p = BezPath::new();
    if from_crossing {
        p.move_to((20.0, 15.0));
        p.line_to((40.0, 30.0));
        p.line_to((40.0, 0.0));
        p.line_to((20.0, 15.0));
        p.line_to((0.0, 30.0));
        p.line_to((0.0, 0.0));
    } else {
        p.move_to((0.0, 0.0));
        p.line_to((40.0, 30.0));
        p.line_to((40.0, 0.0));
        p.line_to((0.0, 30.0));
    }
    p.close_path();
    p
}

/// The golden suite's figure-eight, crossing at (120, 100), started from
/// its right end instead of the crossing.
fn figure_eight() -> BezPath {
    let mut p = BezPath::new();
    p.move_to((240.0, 100.0));
    p.curve_to((210.0, 145.0), (150.0, 145.0), (120.0, 100.0));
    p.curve_to((90.0, 55.0), (30.0, 55.0), (0.0, 100.0));
    p.curve_to((30.0, 145.0), (90.0, 145.0), (120.0, 100.0));
    p.curve_to((150.0, 55.0), (210.0, 55.0), (240.0, 100.0));
    p.close_path();
    p
}

/// A dashed band of width 2 on the right of the path.
fn right_dashed(pattern: &[f64], offset: f64, cap: Cap) -> StrokeStyle {
    StrokeStyle::new(2.0)
        .with_side(StrokeSide::Right)
        .with_dash(
            DashStyle::from_pattern(pattern.iter().copied())
                .with_offset(offset)
                .with_cap(cap),
        )
}

/// Dashes on a self-intersecting path land where `kurbo::dash` puts them
/// on the uncut path (2026-10-20 report).
///
/// A one-sided style cuts the path at its crossings and dashes each piece
/// on its own, starting the pattern at the piece's arc length along the
/// whole path. The cases cover crossings on lines and inside a curve, and
/// closed paths, where kurbo starts the pattern again at the start point.
/// There kurbo joins the dash running into the start point to its first
/// dash, which it emits last, but only when the pattern is on at the start.
/// A dash that starts at the start point right after a gap, as with some
/// dash-dot offsets or a zero-length gap, stays apart.
///
/// The band is on the right, so its left edge is the centerline and each
/// dash's outline starts exactly at the dash's start point. Every start
/// kurbo reports must be one of those outline starts. Where no corner is
/// sharp enough for the fill mask to trim a dash, the reverse holds too: no
/// dash starts anywhere else, except at the crossing, where a piece
/// boundary cuts a dash in two. That catches a dash split at the start
/// point instead of joined.
#[test]
fn dash_phase_continues_across_self_intersections() {
    let cases = [
        ("polyline", crossing_polyline(), (50.0, 50.0), true),
        ("loop curve", loop_curve(), (100.0, 67.8947), true),
        ("bowtie", bowtie(false), (120.0, 100.0), false),
        (
            "bowtie from its crossing",
            bowtie(true),
            (120.0, 100.0),
            false,
        ),
        ("figure-eight", figure_eight(), (120.0, 100.0), true),
        ("whole bowtie", whole_bowtie(false), (20.0, 15.0), false),
    ];
    for (name, path, crossing, exact) in cases {
        let crossing = Point::from(crossing);
        for pattern in [
            &[15.0, 15.0][..],
            &[9.0, 7.0],
            &[4.0, 2.0, 1.0, 2.0],
            &[6.0, 3.0, 2.0],
            &[5.0, 0.0, 5.0, 10.0],
        ] {
            for offset in [0.0, 4.0, -11.0, 5.0, 6.0, 9.0] {
                let style = right_dashed(pattern, offset, Cap::Butt);
                let got = subpath_starts(stroke_aligned(&path, &style, 1e-4).iter());
                let want = dash_starts(&path, offset, pattern);
                let case = format!("{name}, pattern {pattern:?}, offset {offset}");
                assert!(!want.is_empty(), "{case}: no dashes");
                for &w in &want {
                    let miss = nearest(&got, w);
                    assert!(
                        miss < 1e-3,
                        "{case}: no dash starts at {w:?}, nearest is {miss:.3} away"
                    );
                }
                if exact {
                    for &g in &got {
                        assert!(
                            nearest(&want, g).min((g - crossing).hypot()) < 1e-3,
                            "{case}: a dash starts at {g:?}, where kurbo's does not"
                        );
                    }
                }
            }
        }
    }
}

/// The centre of each subpath's bounding box: one per dot.
fn dot_centers(out: &BezPath) -> Vec<Point> {
    let mut centers = Vec::new();
    let mut dot = BezPath::new();
    for el in out.iter().chain([PathEl::MoveTo(Point::ORIGIN)]) {
        if matches!(el, PathEl::MoveTo(_)) && !dot.elements().is_empty() {
            centers.push(dot.bounding_box().center());
            dot.truncate(0);
        }
        dot.push(el);
    }
    centers
}

/// Where kurbo puts the zero-length dashes: each is a `MoveTo p` followed by
/// a drawing element that ends at `p` again.
fn zero_dashes(path: &BezPath, offset: f64, pattern: &[f64]) -> Vec<Point> {
    let els: Vec<PathEl> = kurbo::dash(path.iter(), offset, pattern).collect();
    els.windows(2)
        .filter_map(|pair| match pair {
            [PathEl::MoveTo(p), el] if !matches!(el, PathEl::MoveTo(_)) => el
                .end_point()
                .filter(|q| (*q - *p).hypot() < 1e-9)
                .map(|_| *p),
            _ => None,
        })
        .collect()
}

fn tangent(seg: &PathSeg, t: f64) -> Vec2 {
    match seg {
        PathSeg::Line(l) => l.p1 - l.p0,
        PathSeg::Quad(q) => q.deriv().eval(t).to_vec2(),
        PathSeg::Cubic(c) => c.deriv().eval(t).to_vec2(),
    }
}

/// Dots keep the same phase as dashes: there are as many dots as kurbo has
/// zero-length dashes, each in the middle of the band beside one of them.
///
/// The dots are made separately from the dash stream, so they need their
/// own check. Near a corner or the crossing the fill mask can trim a dot and
/// move its centre, so the position of those dots is not checked.
#[test]
fn dots_keep_phase_across_self_intersections() {
    let figure_eight_corners = [(240.0, 100.0), (120.0, 100.0), (0.0, 100.0)];
    let whole_bowtie_corners = [
        (0.0, 0.0),
        (40.0, 30.0),
        (40.0, 0.0),
        (0.0, 30.0),
        (20.0, 15.0),
    ];
    let whole_bowtie_corners = &whole_bowtie_corners[..];
    let cases = [
        ("polyline", crossing_polyline(), [0.0, 7.0], &[][..]),
        ("loop curve", loop_curve(), [0.0, 5.0], &[][..]),
        (
            "figure-eight",
            figure_eight(),
            [0.0, 7.0],
            &figure_eight_corners[..],
        ),
        (
            "whole bowtie",
            whole_bowtie(false),
            [0.0, 10.0],
            whole_bowtie_corners,
        ),
        (
            "whole bowtie from its crossing",
            whole_bowtie(true),
            [0.0, 7.0],
            whole_bowtie_corners,
        ),
    ];
    for (name, path, pattern, corners) in cases {
        for offset in [0.0, 4.0, -11.0, 1.0] {
            let style = right_dashed(&pattern, offset, Cap::Round);
            let centers = dot_centers(&stroke_aligned(&path, &style, 1e-4));
            let want = zero_dashes(&path, offset, &pattern);
            assert!(!want.is_empty(), "{name}, offset {offset}: no dots");
            assert_eq!(
                centers.len(),
                want.len(),
                "{name}, offset {offset}: dot count"
            );
            for p in want {
                if corners.iter().any(|&c| (Point::from(c) - p).hypot() < 3.0) {
                    continue;
                }
                // The band is 2 wide, so its middle is 1 to the right.
                let beside = path.segments().any(|seg| {
                    let near = seg.nearest(p, 1e-12);
                    let tan = tangent(&seg, near.t);
                    near.distance_sq < 1e-12
                        && nearest(&centers, p + tan.turn_90() * (1.0 / tan.hypot())) < 1e-3
                });
                assert!(beside, "{name}, offset {offset}: no dot beside {p:?}");
            }
        }
    }
}

/// A hexagon of whole-number edges, 10, 39, 34, 39, 10 and 34 long, that
/// crosses itself and starts away from its crossing.
fn whole_hexagon() -> BezPath {
    let mut p = BezPath::new();
    p.move_to((100.0, 100.0));
    p.line_to((108.0, 94.0));
    p.line_to((123.0, 130.0));
    p.line_to((93.0, 146.0));
    p.line_to((78.0, 110.0));
    p.line_to((70.0, 116.0));
    p.close_path();
    p
}

/// Where the pattern restarts at a closed path's start point, rounding must
/// not leave a dot or a dash that kurbo does not draw (2026-10-20 review).
///
/// The piece through the start point is dashed in two parts. The part that
/// runs up to the start point gets a phase summed from arc lengths that are
/// not whole numbers, so on whole-number geometry it ends about 1e-14 off a
/// pattern boundary. That can repeat the dot already drawn at arc length 0,
/// or make kurbo begin a dash about 1e-14 long whose round caps draw a disc.
#[test]
fn no_stray_dot_where_the_pattern_restarts() {
    let path = whole_hexagon();
    let start = Point::new(100.0, 100.0);
    for (pattern, offset) in [([0.0, 5.0], 14.0), ([1.0, 6.0], 2.0)] {
        let style = right_dashed(&pattern, offset, Cap::Round);
        let out = stroke_aligned(&path, &style, 1e-4);
        let near_start = dot_centers(&out)
            .into_iter()
            .filter(|c| (*c - start).hypot() < 2.0)
            .count();
        // A zero-length dash draws a dot 1 to the side of it.
        let want = zero_dashes(&path, offset, &pattern)
            .into_iter()
            .filter(|p| (*p - start).hypot() < 1.0)
            .count();
        assert_eq!(
            near_start, want,
            "pattern {pattern:?}, offset {offset}: dots and dashes at the start point"
        );
    }
}

/// A zero-length pattern entry that lands exactly on a segment end must not
/// turn the output non-finite (2026-10-20 review).
///
/// kurbo's `dash` returns `NaN` there: rounding leaves a sliver of the
/// segment while its parameter has reached 1, and the inverse arc length
/// of the empty remainder is `0 / 0`. Correctly phased pieces of
/// whole-number paths hit this often.
#[test]
fn zero_entries_on_segment_ends_stay_finite() {
    let mut open = BezPath::new();
    open.move_to((100.0, 100.0));
    for p in [
        (124.0, 90.0),
        (100.0, 80.0),
        (116.0, 50.0),
        (104.0, 45.0),
        (104.0, 70.0),
        (119.0, 78.0),
        (107.0, 69.0),
        (117.0, 69.0),
    ] {
        open.line_to(p);
    }
    let mut closed = BezPath::new();
    closed.move_to((100.0, 100.0));
    for p in [
        (136.0, 115.0),
        (146.0, 115.0),
        (110.0, 100.0),
        (115.0, 112.0),
        (105.0, 112.0),
    ] {
        closed.line_to(p);
    }
    closed.close_path();
    for (name, path, pattern, offset) in [
        ("open", &open, [0.0, 7.0], 12.0),
        ("closed", &closed, [6.0, 0.0], 3.0),
    ] {
        for alignment in [
            kurbo_se::StrokeAlignment::Inside,
            kurbo_se::StrokeAlignment::Outside,
        ] {
            for cap in [Cap::Round, Cap::Butt] {
                let style = StrokeStyle::new(2.0).with_alignment(alignment).with_dash(
                    DashStyle::from_pattern(pattern)
                        .with_offset(offset)
                        .with_cap(cap),
                );
                let out = stroke_aligned(path, &style, 1e-4);
                assert!(
                    out.is_finite(),
                    "{name}/{alignment:?}/{cap:?}: non-finite output"
                );
            }
        }
    }
}
