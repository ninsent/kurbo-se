// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Workload benchmarks: representative inputs, adversarial inputs and
//! scaling series.
//!
//! `expand.rs` pins a handful of fixed cases. This file covers more
//! ground, so an optimization can be shown to help across the board and
//! not just on a few shapes. It also measures how cost grows with input
//! size:
//!
//! - Representative groups model what an editor or renderer strokes day
//!   to day: `ui`, `long_dashed`, `holes`, `self_intersecting`, `organic`,
//!   `animated_dash` and `batch`.
//! - `adversarial_*` groups are stress cases, named as such: degenerate
//!   and extreme widths, inputs made of thousands of tiny segments, and
//!   dash periods far shorter than the path.
//! - `scaling_*` groups grow one aspect of the input over a range of about
//!   16x: segments, holes, dashes or self-crossings. The size is the
//!   benchmark parameter.
//!
//! Ids read `group/shape/style`. The style label is generated from the
//! [`StrokeStyle`]: `inside-w8-dash12-6-round` is an inside stroke of
//! width 8, dashed 12 on and 6 off, with round dash caps. Scaling ids read
//! `group/shape-style/size`. All inputs are built outside the timed loop.
//! Unless noted, circles are four cubic arcs. That is what kurbo emits at
//! display tolerance, and what design tools and SVG imports supply.
//!
//! The whole file runs in four to five minutes:
//!
//! ```text
//! cargo bench --bench workloads
//! ```

#![allow(missing_docs)] // criterion macros generate undocumented items

use std::f64::consts::{PI, TAU};
use std::fmt::{Display, Write as _};
use std::hint::black_box;
use std::time::Duration;

use criterion::measurement::WallTime;
use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, SamplingMode, criterion_group, criterion_main,
};
use kurbo::{BezPath, Circle, Point, Rect, RoundedRect, Shape, Vec2};
use kurbo_se::{
    AlignedStrokeCtx, Cap, DashStyle, Join, StrokeAlignment, StrokeSide, StrokeStyle,
    stroke_aligned, stroke_aligned_with,
};

/// The display tolerance a renderer passes at 1:1 zoom.
const TOL: f64 = 0.25;

// ---- group configuration --------------------------------------------------

/// A group for cases that take up to about a millisecond per iteration.
fn fast_group<'a>(c: &'a mut Criterion, name: &str) -> BenchmarkGroup<'a, WallTime> {
    let mut g = c.benchmark_group(name);
    g.warm_up_time(Duration::from_millis(800))
        .measurement_time(Duration::from_secs(2))
        .sample_size(40);
    g
}

/// A group for cases that take up to about 100 ms per iteration, and for
/// scaling series whose sizes span that range.
///
/// Flat sampling runs the same number of iterations in every sample. So
/// even the slowest case fits its samples into the measurement time, and
/// every point of a series is measured the same way.
fn slow_group<'a>(c: &'a mut Criterion, name: &str) -> BenchmarkGroup<'a, WallTime> {
    let mut g = c.benchmark_group(name);
    g.warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .sample_size(20)
        .sampling_mode(SamplingMode::Flat);
    g
}

// ---- styles ---------------------------------------------------------------

/// A centered solid stroke with Figma's defaults: miter join, butt caps.
fn center(width: f64) -> StrokeStyle {
    StrokeStyle::new(width)
}

/// An inside-aligned solid stroke.
fn inside(width: f64) -> StrokeStyle {
    StrokeStyle::new(width).with_alignment(StrokeAlignment::Inside)
}

/// An outside-aligned solid stroke.
fn outside(width: f64) -> StrokeStyle {
    StrokeStyle::new(width).with_alignment(StrokeAlignment::Outside)
}

/// `style` dashed `dash` on, `gap` off, with dash caps `cap`.
fn dashed(style: StrokeStyle, dash: f64, gap: f64, cap: Cap) -> StrokeStyle {
    style.with_dash(DashStyle::new(dash, gap).with_cap(cap))
}

/// The id label for `style`: placement and width, then whatever differs
/// from the [`StrokeStyle::new`] defaults among join, caps and dash, as in
/// `inside-w8-dash12-6-round`. Dash offsets and miter angles are left out,
/// since no two benchmarks here differ only in those.
fn style_label(style: &StrokeStyle) -> String {
    let placement = match (style.side, style.alignment) {
        (Some(StrokeSide::Left), _) => "left",
        (Some(StrokeSide::Right), _) => "right",
        (Some(StrokeSide::Center), _) | (None, StrokeAlignment::Center) => "center",
        (None, StrokeAlignment::Inside) => "inside",
        (None, StrokeAlignment::Outside) => "outside",
    };
    let mut label = format!("{placement}-w{}", style.width);
    if style.join != Join::Miter {
        write!(label, "-{:?}-join", style.join).unwrap();
    }
    if (style.start_cap, style.end_cap) != (Cap::Butt, Cap::Butt) {
        write!(label, "-caps-{:?}-{:?}", style.start_cap, style.end_cap).unwrap();
    }
    if let Some(dash) = &style.dash {
        label.push_str("-dash");
        for (i, length) in dash.pattern.iter().enumerate() {
            let sep = if i == 0 { "" } else { "-" };
            write!(label, "{sep}{length}").unwrap();
        }
        if dash.cap != Cap::Butt {
            write!(label, "-{:?}", dash.cap).unwrap();
        }
    }
    label.make_ascii_lowercase();
    label
}

// ---- benchmark helpers ----------------------------------------------------

/// Registers `group/shape/style`, timing one [`stroke_aligned`] call on
/// `path` at display tolerance.
fn bench_stroke(
    g: &mut BenchmarkGroup<'_, WallTime>,
    shape: &str,
    path: &BezPath,
    style: &StrokeStyle,
) {
    let id = BenchmarkId::new(shape, style_label(style));
    bench_stroke_with_id(g, id, path, style);
}

/// Registers `group/shape-style/size`, one point of a scaling series.
fn bench_scaling(
    g: &mut BenchmarkGroup<'_, WallTime>,
    shape: &str,
    size: impl Display,
    path: &BezPath,
    style: &StrokeStyle,
) {
    let id = BenchmarkId::new(format!("{shape}-{}", style_label(style)), size);
    bench_stroke_with_id(g, id, path, style);
}

/// Checks that `path` paints, then times one [`stroke_aligned`] call as `id`.
fn bench_stroke_with_id(
    g: &mut BenchmarkGroup<'_, WallTime>,
    id: BenchmarkId,
    path: &BezPath,
    style: &StrokeStyle,
) {
    assert_paints(path, style);
    g.bench_function(id, |b| {
        b.iter(|| black_box(stroke_aligned(black_box(path), black_box(style), TOL)));
    });
}

/// Panics unless stroking `path` with `style` gives a non-empty outline.
///
/// Every benchmark calls this once before it registers, so a broken shape
/// builder panics instead of timing an empty path. All the checks together
/// take a fraction of a second.
fn assert_paints(path: &BezPath, style: &StrokeStyle) {
    let out = stroke_aligned(path, style, TOL);
    assert!(
        !out.elements().is_empty(),
        "benchmark input strokes to an empty outline: {style:?}"
    );
}

/// Registers `group/shape/style` for an animation. Every iteration moves
/// the dash offset on by a fixed step and strokes again through one reused
/// [`AlignedStrokeCtx`], which is how the crate docs say to animate a dash.
///
/// The offset wraps at the pattern period, so iterations do similar work
/// however many of them criterion runs.
fn bench_animated_dash(
    g: &mut BenchmarkGroup<'_, WallTime>,
    shape: &str,
    path: &BezPath,
    style: &StrokeStyle,
) {
    const STEP: f64 = 0.7;
    let dash = style
        .dash
        .as_ref()
        .expect("animated benchmarks need a dash");
    let period: f64 = dash.pattern.iter().sum();
    assert_paints(path, style);
    g.bench_function(BenchmarkId::new(shape, style_label(style)), |b| {
        let mut ctx = AlignedStrokeCtx::new();
        let mut style = style.clone();
        b.iter(|| {
            if let Some(dash) = &mut style.dash {
                dash.offset = (dash.offset + STEP) % period;
            }
            black_box(stroke_aligned_with(black_box(path), &style, TOL, &mut ctx));
        });
    });
}

// ---- shape builders -------------------------------------------------------

/// A pseudo-random value in `[-1, 1)` for index `i`, from the `SplitMix64`
/// finalizer. It is deterministic, so inputs are the same on every run,
/// and it needs no `rand` dependency.
fn noise(i: u64) -> f64 {
    let mut z = i.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z >> 11) as f64 / (1_u64 << 52) as f64 - 1.0
}

/// A polyline through `points`, closed into a polygon when `closed`.
fn polyline(points: impl IntoIterator<Item = Point>, closed: bool) -> BezPath {
    let mut p = BezPath::new();
    for (i, pt) in points.into_iter().enumerate() {
        if i == 0 {
            p.move_to(pt);
        } else {
            p.line_to(pt);
        }
    }
    if closed {
        p.close_path();
    }
    p
}

/// Fits `segments` cubics through the smooth parametric curve `f` over
/// `t ∈ [0, 1]`, the way a drawing tool fits freehand input. Each cubic
/// matches `f` in position and tangent at both of its knots: cubic Hermite
/// interpolation. Tangents come from a central difference. A closed curve
/// must have `f(0) = f(1)`, and its last knot reuses the first exactly.
fn hermite_fit(segments: usize, closed: bool, f: impl Fn(f64) -> Point) -> BezPath {
    const H: f64 = 1e-6;
    let dt = 1.0 / segments as f64;
    let knot = |i: usize| {
        let i = if closed { i % segments } else { i };
        let t = i as f64 * dt;
        let tangent = (f(t + H) - f(t - H)) / (2.0 * H);
        (f(t), tangent * (dt / 3.0))
    };
    let mut p = BezPath::new();
    let (mut p0, mut d0) = knot(0);
    p.move_to(p0);
    for i in 1..=segments {
        let (p1, d1) = knot(i);
        p.curve_to(p0 + d0, p1 - d1, p1);
        (p0, d0) = (p1, d1);
    }
    if closed {
        p.close_path();
    }
    p
}

/// A circle as kurbo emits it at display tolerance: four cubic arcs.
fn circle(center: Point, r: f64) -> BezPath {
    Circle::new(center, r).to_path(TOL)
}

/// An annulus: the outer circle plus the inner one reversed, so the hole
/// has winding zero under the nonzero rule.
fn donut(center: Point, r_outer: f64, r_inner: f64) -> BezPath {
    let mut p = circle(center, r_outer);
    p.extend(circle(center, r_inner).reverse_subpaths().iter());
    p
}

/// A star with `tips` tips. Its vertices alternate between `r_outer` and
/// `r_inner`, and the first tip points right.
fn star(center: Point, tips: usize, r_outer: f64, r_inner: f64) -> BezPath {
    polyline(
        (0..2 * tips).map(|i| {
            let r = if i % 2 == 0 { r_outer } else { r_inner };
            center + r * Vec2::from_angle(PI * i as f64 / tips as f64)
        }),
        true,
    )
}

/// A regular `n`-gon inscribed in a circle of radius `r`: a circle as
/// flattening or a polygon import leaves it.
fn regular_polygon(center: Point, r: f64, n: usize) -> BezPath {
    polyline(
        (0..n).map(|i| center + r * Vec2::from_angle(TAU * i as f64 / n as f64)),
        true,
    )
}

/// A sawtooth gear, built like the one in `expand.rs`: a star with one tip
/// per tooth.
fn gear(center: Point, teeth: usize, r_tip: f64, r_root: f64) -> BezPath {
    star(center, teeth, r_tip, r_root)
}

/// The star polygon `{n/step}`: `n` points on a circle, each joined to the
/// one `step` places further on, closed. `{5/2}` is the pentagram.
fn star_polygon(center: Point, r: f64, n: usize, step: usize) -> BezPath {
    polyline(
        (0..n).map(|i| {
            let a = TAU * (i * step) as f64 / n as f64 - PI / 2.0;
            center + r * Vec2::from_angle(a)
        }),
        true,
    )
}

/// The squiggle from `expand.rs`, like a spell-check underline: `n` cubics,
/// each 10 units wide, with handles 15 units up and down.
fn wavy(n: usize) -> BezPath {
    let mut p = BezPath::new();
    p.move_to((0.0, 0.0));
    for i in 0..n {
        let x = i as f64 * 10.0;
        p.curve_to((x + 3.0, 15.0), (x + 7.0, -15.0), (x + 10.0, 0.0));
    }
    p
}

/// A closed organic blob of `segments` cubics: a circle of radius `r` whose
/// radius wobbles by up to 23% either way, from three sine harmonics. The
/// radius stays positive, so the outline does not cross itself.
fn blob(center: Point, r: f64, segments: usize) -> BezPath {
    hermite_fit(segments, true, |t| {
        let a = TAU * t;
        let wobble =
            0.12 * (3.0 * a + 0.4).sin() + 0.07 * (5.0 * a + 1.3).sin() + 0.04 * (11.0 * a).sin();
        center + r * (1.0 + wobble) * Vec2::from_angle(a)
    })
}

/// An open brush stroke of `segments` cubics meandering left to right over
/// about 900 units, like a fitted freehand pen stroke.
fn brush_stroke(segments: usize) -> BezPath {
    hermite_fit(segments, false, |t| {
        let a = TAU * t;
        Point::new(
            900.0 * t + 10.0 * (9.0 * a).sin(),
            100.0 * (2.0 * a).sin() + 25.0 * (5.0 * a + 1.0).sin(),
        )
    })
}

/// A 24-unit heart icon: six cubics, a notch at the top and a point at the
/// bottom.
fn heart_icon() -> BezPath {
    let mut p = BezPath::new();
    p.move_to((12.0, 20.5));
    p.curve_to((5.0, 15.5), (2.0, 12.0), (2.0, 8.5));
    p.curve_to((2.0, 5.5), (4.3, 3.5), (7.0, 3.5));
    p.curve_to((9.0, 3.5), (11.0, 4.8), (12.0, 6.5));
    p.curve_to((13.0, 4.8), (15.0, 3.5), (17.0, 3.5));
    p.curve_to((19.7, 3.5), (22.0, 5.5), (22.0, 8.5));
    p.curve_to((22.0, 12.0), (19.0, 15.5), (12.0, 20.5));
    p.close_path();
    p
}

/// A five-point sparkline, 100 × 26, whose sharp corners get miter joins.
fn sparkline() -> BezPath {
    let points = [
        (0.0, 30.0),
        (25.0, 10.0),
        (50.0, 24.0),
        (75.0, 4.0),
        (100.0, 18.0),
    ];
    polyline(points.map(Point::from), false)
}

/// A line chart of `points` samples, 3 units apart: two slow waves plus
/// measurement noise.
fn chart_line(points: usize) -> BezPath {
    polyline(
        (0..points).map(|i| {
            let x = 3.0 * i as f64;
            let y = 150.0 + 60.0 * (x / 120.0).sin() + 25.0 * (x / 31.0).sin();
            Point::new(x, y + 6.0 * noise(i as u64))
        }),
        false,
    )
}

/// Raw freehand input: `samples` pointer positions 2 units apart in x,
/// along a gentle meander. Digitizer jitter moves each one by up to 0.6
/// units on each axis.
fn freehand_trace(samples: usize) -> BezPath {
    polyline(
        (0..samples).map(|i| {
            let s = i as f64;
            let base = Point::new(2.0 * s, 80.0 * (s / 90.0).sin() + 30.0 * (s / 37.0).sin());
            let jitter = Vec2::new(noise(2 * i as u64), noise(2 * i as u64 + 1));
            base + 0.6 * jitter
        }),
        false,
    )
}

/// A rectangular plate with rounded corners and a `cols × rows` grid of
/// round holes, one per `pitch`-sized cell. The holes wind against the
/// plate, as a design tool's boolean subtraction leaves them.
fn perforated_plate(cols: usize, rows: usize, pitch: Vec2, hole_r: f64) -> BezPath {
    let size = Vec2::new(cols as f64 * pitch.x, rows as f64 * pitch.y);
    let mut p = RoundedRect::new(0.0, 0.0, size.x, size.y, 12.0).to_path(TOL);
    for row in 0..rows {
        for col in 0..cols {
            let c = Point::new((col as f64 + 0.5) * pitch.x, (row as f64 + 0.5) * pitch.y);
            p.extend(circle(c, hole_r).reverse_subpaths().iter());
        }
    }
    p
}

/// The bowtie from the golden tests: two triangular lobes wound in opposite
/// directions, one crossing.
fn bowtie() -> BezPath {
    polyline(
        [(20.0, 20.0), (220.0, 180.0), (220.0, 20.0), (20.0, 180.0)].map(Point::from),
        true,
    )
}

/// The figure-eight from the golden tests: two cubic lobes, one crossing.
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

/// A pretzel: the trefoil knot drawn as one closed curve of 24 cubics. Its
/// three crossings interleave along the curve rather than nest, and its
/// middle winds twice.
fn pretzel() -> BezPath {
    hermite_fit(24, true, |t| {
        let a = TAU * t;
        Point::new(
            150.0 + 40.0 * (a.sin() + 2.0 * (2.0 * a).sin()),
            150.0 + 40.0 * (a.cos() - 2.0 * (2.0 * a).cos()),
        )
    })
}

/// The open cubic with a loop from the golden tests.
fn loop_cubic() -> BezPath {
    let mut p = BezPath::new();
    p.move_to((20.0, 150.0));
    p.curve_to((200.0, 20.0), (0.0, 20.0), (180.0, 150.0));
    p
}

/// A braid border: a sine strand and its mirror image, joined at both ends
/// into one closed polygon that crosses itself `crossings` times. Between
/// crossings lie lens-shaped lobes, 40 long and 30 tall. They alternate
/// direction, so the fill winds at most once anywhere.
///
/// The polygon has 998 edges whatever the crossing count, so only the
/// crossings vary. Each strand has 499 edges, a prime, so no vertex lands
/// exactly on a crossing at any count used here.
fn braid(crossings: usize) -> BezPath {
    const EDGES_PER_STRAND: usize = 499;
    const LOBE_LENGTH: f64 = 40.0;
    let length = (crossings + 1) as f64 * LOBE_LENGTH;
    let strand = |i: usize, sign: f64| {
        let x = length * i as f64 / EDGES_PER_STRAND as f64;
        Point::new(x, sign * 15.0 * (PI * x / LOBE_LENGTH).sin())
    };
    let forward = (0..=EDGES_PER_STRAND).map(|i| strand(i, 1.0));
    let back = (1..EDGES_PER_STRAND).rev().map(|i| strand(i, -1.0));
    polyline(forward.chain(back), true)
}

/// 200 small shapes, the kind a canvas holds many of: rounded rects, circles
/// and stars about 20 units across, on a 20 × 10 grid.
fn small_shapes() -> Vec<BezPath> {
    (0..200)
        .map(|i| {
            let c = Point::new(30.0 * (i % 20) as f64 + 15.0, 30.0 * (i / 20) as f64 + 15.0);
            match i % 3 {
                0 => Rect::from_center_size(c, (20.0, 14.0))
                    .to_rounded_rect(3.0)
                    .to_path(TOL),
                1 => circle(c, 10.0),
                _ => star(c, 5, 10.0, 4.0),
            }
        })
        .collect()
}

// ---- representative workloads ---------------------------------------------

/// Small UI elements: buttons, cards, sparklines, icons.
fn bench_ui(c: &mut Criterion) {
    let mut g = fast_group(c, "ui");
    let rect = Rect::new(0.0, 0.0, 120.0, 40.0).to_path(TOL);
    let rounded = RoundedRect::new(0.0, 0.0, 120.0, 40.0, 8.0).to_path(TOL);
    for (shape, path) in [("rect-120x40", &rect), ("rounded-rect-120x40-r8", &rounded)] {
        for style in [inside(1.0), inside(2.0), outside(1.0), center(1.0)] {
            bench_stroke(&mut g, shape, path, &style);
        }
    }
    let sparkline = sparkline();
    bench_stroke(&mut g, "polyline-5", &sparkline, &center(2.0));
    bench_stroke(&mut g, "polyline-5", &sparkline, &inside(2.0));
    let heart = heart_icon();
    bench_stroke(&mut g, "heart-icon", &heart, &center(1.5));
    bench_stroke(&mut g, "heart-icon", &heart, &inside(1.5));
    g.finish();
}

/// Long dashed paths: hundreds of dashes per stroke.
fn bench_long_dashed(c: &mut Criterion) {
    let mut g = fast_group(c, "long_dashed");
    let center_dashes = dashed(center(4.0), 12.0, 6.0, Cap::Butt);
    let inside_dashes = dashed(inside(4.0), 12.0, 6.0, Cap::Butt);
    let wavy400 = wavy(400);
    bench_stroke(&mut g, "wavy-400", &wavy400, &center_dashes);
    bench_stroke(&mut g, "wavy-400", &wavy400, &inside_dashes);
    let chart = chart_line(1000);
    let chart_dashes = dashed(center(2.0), 12.0, 6.0, Cap::Butt);
    bench_stroke(&mut g, "chart-line-1000", &chart, &chart_dashes);
    let blob200 = blob(Point::new(300.0, 300.0), 200.0, 200);
    bench_stroke(&mut g, "blob-200", &blob200, &inside_dashes);
    g.finish();
}

/// Compound paths with many holes: a perforated plate, 400 × 300.
fn bench_holes(c: &mut Criterion) {
    let mut g = fast_group(c, "holes");
    let plate16 = perforated_plate(4, 4, Vec2::new(100.0, 75.0), 22.5);
    let plate64 = perforated_plate(8, 8, Vec2::new(50.0, 37.5), 11.25);
    for (shape, plate) in [("plate-16-holes", &plate16), ("plate-64-holes", &plate64)] {
        bench_stroke(&mut g, shape, plate, &inside(4.0));
        bench_stroke(&mut g, shape, plate, &outside(4.0));
    }
    let dashes = dashed(inside(4.0), 12.0, 6.0, Cap::Butt);
    bench_stroke(&mut g, "plate-64-holes", &plate64, &dashes);
    g.finish();
}

/// Paths that cross themselves, split into lobes and resolved per lobe.
fn bench_self_intersecting(c: &mut Criterion) {
    let mut g = fast_group(c, "self_intersecting");
    let pentagram = star_polygon(Point::new(150.0, 150.0), 120.0, 5, 2);
    let shapes = [
        ("bowtie", bowtie()),
        ("figure-eight", figure_eight()),
        ("pretzel", pretzel()),
        ("pentagram", pentagram.clone()),
        ("loop-cubic", loop_cubic()),
    ];
    for (shape, path) in &shapes {
        bench_stroke(&mut g, shape, path, &inside(8.0));
        bench_stroke(&mut g, shape, path, &outside(8.0));
    }
    let dashes = dashed(inside(8.0), 12.0, 6.0, Cap::Round);
    bench_stroke(&mut g, "pentagram", &pentagram, &dashes);
    g.finish();
}

/// Smooth freeform outlines and strokes.
fn bench_organic(c: &mut Criterion) {
    let mut g = fast_group(c, "organic");
    let blob200 = blob(Point::new(300.0, 300.0), 200.0, 200);
    for style in [inside(8.0), outside(8.0), center(8.0)] {
        bench_stroke(&mut g, "blob-200", &blob200, &style);
    }
    let brush = brush_stroke(100);
    let right = StrokeStyle::new(8.0).with_side(StrokeSide::Right);
    bench_stroke(&mut g, "brush-100", &brush, &right);
    g.finish();
}

/// Marching ants: the dash offset advances every frame.
fn bench_animated_dash_group(c: &mut Criterion) {
    let mut g = fast_group(c, "animated_dash");
    let ring = donut(Point::new(130.0, 130.0), 100.0, 45.0);
    let ring_dashes = dashed(inside(8.0), 12.0, 6.0, Cap::Butt);
    bench_animated_dash(&mut g, "donut-100-45", &ring, &ring_dashes);
    let star5 = star(Point::new(150.0, 150.0), 5, 100.0, 40.0);
    let star_dashes = dashed(outside(8.0), 10.0, 8.0, Cap::Round);
    bench_animated_dash(&mut g, "star-5", &star5, &star_dashes);
    g.finish();
}

/// A canvas of 200 small shapes stroked one after another. Times all of
/// them, once with a fresh context per call and once with one context
/// reused throughout.
fn bench_batch(c: &mut Criterion) {
    let mut g = fast_group(c, "batch");
    let shapes = small_shapes();
    let style = inside(1.5);
    for shape in &shapes {
        assert_paints(shape, &style);
    }
    let label = style_label(&style);
    g.bench_function(BenchmarkId::new("small-shapes-200", &label), |b| {
        b.iter(|| {
            for shape in &shapes {
                black_box(stroke_aligned(black_box(shape), &style, TOL));
            }
        });
    });
    let reuse_id = BenchmarkId::new("small-shapes-200", format!("{label}-ctx-reuse"));
    g.bench_function(reuse_id, |b| {
        let mut ctx = AlignedStrokeCtx::new();
        b.iter(|| {
            for shape in &shapes {
                black_box(stroke_aligned_with(black_box(shape), &style, TOL, &mut ctx));
            }
        });
    });
    g.finish();
}

// ---- adversarial workloads ------------------------------------------------

/// Widths at which an offset degenerates: it collapses to a point, two
/// offsets coincide, or opposite sides meet on a line.
fn bench_degenerate_widths(c: &mut Criterion) {
    let mut g = fast_group(c, "adversarial_degenerate_width");
    let disc = circle(Point::new(120.0, 120.0), 70.0);
    bench_stroke(&mut g, "circle-r70", &disc, &inside(70.0));
    // The same circle at a fine tolerance, as an export or a CAD import may
    // supply it: 31 cubics. At `w = r` their inward offsets meet in a tight
    // cluster at the centre.
    let precise_disc = Circle::new((120.0, 120.0), 70.0).to_path(1e-7);
    bench_stroke(&mut g, "circle-r70-precise", &precise_disc, &inside(70.0));
    // Half the ring thickness, (100 - 45) / 2: the inward offsets coincide.
    let ring = donut(Point::new(130.0, 130.0), 100.0, 45.0);
    bench_stroke(&mut g, "donut-100-45", &ring, &inside(27.5));
    // Half the short side: the long sides' offsets meet on the midline.
    let rect = Rect::new(0.0, 0.0, 120.0, 40.0).to_path(TOL);
    bench_stroke(&mut g, "rect-120x40", &rect, &inside(20.0));
    g.finish();
}

/// Inputs made of thousands of tiny segments, as flattening or raw pointer
/// capture produce them.
fn bench_tiny_segments(c: &mut Criterion) {
    let mut g = slow_group(c, "adversarial_tiny_segments");
    let n_gon = regular_polygon(Point::new(250.0, 250.0), 200.0, 4000);
    bench_stroke(&mut g, "circle-4000-lines", &n_gon, &inside(4.0));
    bench_stroke(&mut g, "circle-4000-lines", &n_gon, &center(4.0));
    let trace = freehand_trace(2000);
    bench_stroke(&mut g, "freehand-2000", &trace, &center(3.0));
    bench_stroke(&mut g, "freehand-2000", &trace, &inside(3.0));
    g.finish();
}

/// Widths far beyond the shape's own size.
fn bench_extreme_widths(c: &mut Criterion) {
    let mut g = fast_group(c, "adversarial_extreme_width");
    let star5 = star(Point::new(150.0, 150.0), 5, 100.0, 40.0);
    bench_stroke(&mut g, "star-5", &star5, &inside(500.0));
    let gear24 = gear(Point::new(150.0, 150.0), 24, 110.0, 90.0);
    bench_stroke(&mut g, "gear-24", &gear24, &outside(200.0));
    let ring = donut(Point::new(130.0, 130.0), 100.0, 45.0);
    bench_stroke(&mut g, "donut-100-45", &ring, &outside(300.0));
    g.finish();
}

/// Dash periods far shorter than the path: thousands of dashes.
fn bench_short_dashes(c: &mut Criterion) {
    let mut g = slow_group(c, "adversarial_short_dash");
    let wavy200 = wavy(200);
    for style in [center(4.0), inside(4.0)] {
        let style = dashed(style, 0.5, 0.5, Cap::Butt);
        bench_stroke(&mut g, "wavy-200", &wavy200, &style);
    }
    let line = polyline([Point::ZERO, Point::new(20_000.0, 0.0)], false);
    for style in [center(2.0), inside(2.0)] {
        let style = dashed(style, 1.0, 1.0, Cap::Butt);
        bench_stroke(&mut g, "line-20000", &line, &style);
    }
    g.finish();
}

// ---- scaling series -------------------------------------------------------

/// Segment count, with no crossings or holes.
///
/// The gears keep the 30:29 tip-to-root ratio and the 2.5-unit tooth pitch
/// of the 750-tooth gear in `expand.rs`. So at every size the teeth sit
/// closer together than the width-8 band, as they do there. The radius
/// grows with the tooth count, from 10 to 160. At 10 the band leaves only
/// a small core. The blobs keep their shape and grow with the cubic count,
/// so every cubic spans the same arc length.
fn bench_scaling_segments(c: &mut Criterion) {
    let mut g = slow_group(c, "scaling_segments");
    for teeth in [25, 100, 400] {
        let r_tip = 0.4 * teeth as f64;
        let path = gear(Point::new(r_tip, r_tip), teeth, r_tip, r_tip * 29.0 / 30.0);
        bench_scaling(&mut g, "gear", teeth, &path, &inside(8.0));
    }
    for cubics in [50, 200, 800] {
        let r = 2.0 * cubics as f64;
        let path = blob(Point::new(r, r), r, cubics);
        bench_scaling(&mut g, "blob", cubics, &path, &inside(8.0));
    }
    g.finish();
}

/// Hole count, at a fixed 50-unit pitch: the plate grows with the grid.
fn bench_scaling_holes(c: &mut Criterion) {
    let mut g = slow_group(c, "scaling_holes");
    for side in [2, 4, 8] {
        let path = perforated_plate(side, side, Vec2::new(50.0, 50.0), 15.0);
        bench_scaling(&mut g, "plate", side * side, &path, &inside(4.0));
    }
    g.finish();
}

/// Dash count on a fixed path, set by the pattern period. Each period is
/// two thirds on and one third off, like `12, 6`. The id's size is the
/// period.
fn bench_scaling_dashes(c: &mut Criterion) {
    let mut g = slow_group(c, "scaling_dashes");
    let path = wavy(100);
    for period in [40.0, 10.0, 2.5] {
        let style = dashed(inside(4.0), period * 2.0 / 3.0, period / 3.0, Cap::Butt);
        let id = BenchmarkId::new("wavy-100-inside-w4-dash-period", period);
        bench_stroke_with_id(&mut g, id, &path, &style);
    }
    g.finish();
}

/// Self-crossing count, on a braid polygon whose edge count stays fixed.
fn bench_scaling_crossings(c: &mut Criterion) {
    let mut g = slow_group(c, "scaling_crossings");
    for crossings in [11, 41, 161] {
        let path = braid(crossings);
        bench_scaling(&mut g, "braid", crossings, &path, &outside(6.0));
    }
    g.finish();
}

criterion_group!(
    representative,
    bench_ui,
    bench_long_dashed,
    bench_holes,
    bench_self_intersecting,
    bench_organic,
    bench_animated_dash_group,
    bench_batch,
);
criterion_group!(
    adversarial,
    bench_degenerate_widths,
    bench_tiny_segments,
    bench_extreme_widths,
    bench_short_dashes,
);
criterion_group!(
    scaling,
    bench_scaling_segments,
    bench_scaling_holes,
    bench_scaling_dashes,
    bench_scaling_crossings,
);
criterion_main!(representative, adversarial, scaling);
