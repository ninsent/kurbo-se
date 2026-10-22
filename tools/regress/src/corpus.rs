// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The corpus: shapes, styles and tolerances, combined into named cases.
//!
//! The cases are chosen, not a full Cartesian product. Every shape gets
//! one of a few style sets: `full`, `lite` or `glyph`. So each shape sees
//! every alignment at several joins and widths, plus dashed variants, and
//! every style feature runs on many shapes. On top of that come the cases
//! the changelog's fixes were found on, seeded random paths, and subsets
//! at the finer tolerances `0.05` and `1e-3`.
//!
//! Case names are stable and built from the parameters, so a name always
//! says exactly what ran. They take the form
//! `shape/alignment/join/w<width>[/caps-<start>-<end>][/dash<pattern>]/tol<tol>`.
//! Scaled shapes, `-tiny` and `-huge`, scale every length of the style and
//! the tolerance with them, and their names show the scaled values.

use std::collections::HashSet;
use std::f64::consts::{PI, TAU};

use kurbo::{
    Affine, Arc, BezPath, Cap, Circle, CubicBez, Ellipse, Join, PathEl, Point, Rect, RoundedRect,
    Shape, Vec2,
};

use crate::glyphs;

/// Figma's default miter angle, which `StrokeStyle::new` also uses.
pub const DEFAULT_MITER_ANGLE: f64 = 28.96;

/// Where the band sits: an alignment, or an explicit side override.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    /// `StrokeAlignment::Inside`.
    Inside,
    /// `StrokeAlignment::Center`.
    Center,
    /// `StrokeAlignment::Outside`.
    Outside,
    /// `StrokeSide::Left`.
    Left,
    /// `StrokeSide::Right`.
    Right,
    /// `StrokeSide::Center`.
    SideCenter,
}

impl Align {
    fn label(self) -> &'static str {
        match self {
            Align::Inside => "inside",
            Align::Center => "center",
            Align::Outside => "outside",
            Align::Left => "side-left",
            Align::Right => "side-right",
            Align::SideCenter => "side-center",
        }
    }
}

/// A dash pattern, offset and dash cap.
#[derive(Clone, Debug)]
pub struct Dash {
    /// Alternating dash and gap lengths.
    pub pattern: Vec<f64>,
    /// Phase shift.
    pub offset: f64,
    /// Cap on the ends that dashing creates.
    pub cap: Cap,
}

impl Dash {
    fn new(pattern: &[f64], offset: f64, cap: Cap) -> Self {
        Dash {
            pattern: pattern.to_vec(),
            offset,
            cap,
        }
    }
}

/// A stroke style, independent of the crate version it is handed to.
#[derive(Clone, Debug)]
pub struct Style {
    /// Total band width.
    pub width: f64,
    /// Alignment or side.
    pub align: Align,
    /// Join.
    pub join: Join,
    /// Miter angle in degrees.
    pub miter_angle: f64,
    /// Cap at the start of open subpaths.
    pub start_cap: Cap,
    /// Cap at the end of open subpaths.
    pub end_cap: Cap,
    /// Dashing, if any.
    pub dash: Option<Dash>,
}

/// Build either version's `StrokeStyle` from a [`Style`]. The two versions
/// have the same builder API but distinct types, so this is a macro.
macro_rules! stroke_style {
    ($krate:ident, $s:expr) => {{
        let s: &Style = $s;
        let mut out = $krate::StrokeStyle::new(s.width)
            .with_join(s.join)
            .with_miter_angle(s.miter_angle)
            .with_start_cap(s.start_cap)
            .with_end_cap(s.end_cap);
        out = match s.align {
            Align::Inside => out.with_alignment($krate::StrokeAlignment::Inside),
            Align::Center => out.with_alignment($krate::StrokeAlignment::Center),
            Align::Outside => out.with_alignment($krate::StrokeAlignment::Outside),
            Align::Left => out.with_side($krate::StrokeSide::Left),
            Align::Right => out.with_side($krate::StrokeSide::Right),
            Align::SideCenter => out.with_side($krate::StrokeSide::Center),
        };
        if let Some(d) = &s.dash {
            out = out.with_dash(
                $krate::DashStyle::from_pattern(d.pattern.iter().copied())
                    .with_offset(d.offset)
                    .with_cap(d.cap),
            );
        }
        out
    }};
}

impl Style {
    /// A solid style with butt caps and the default miter angle.
    pub fn new(align: Align, join: Join, width: f64) -> Self {
        Style {
            width,
            align,
            join,
            miter_angle: DEFAULT_MITER_ANGLE,
            start_cap: Cap::Butt,
            end_cap: Cap::Butt,
            dash: None,
        }
    }

    fn miter(mut self, degrees: f64) -> Self {
        self.miter_angle = degrees;
        self
    }

    fn caps(mut self, start: Cap, end: Cap) -> Self {
        self.start_cap = start;
        self.end_cap = end;
        self
    }

    fn dash(mut self, dash: &Dash) -> Self {
        self.dash = Some(dash.clone());
        self
    }

    /// A copy with every length scaled by `k`.
    fn scaled(&self, k: f64) -> Self {
        let mut s = self.clone();
        s.width *= k;
        if let Some(d) = &mut s.dash {
            d.pattern.iter_mut().for_each(|v| *v *= k);
            d.offset *= k;
        }
        s
    }

    /// The style for the candidate crate.
    pub fn candidate(&self) -> kurbo_se::StrokeStyle {
        stroke_style!(kurbo_se, self)
    }

    /// The style for the reference crate.
    pub fn reference(&self) -> kurbo_se_ref::StrokeStyle {
        stroke_style!(kurbo_se_ref, self)
    }

    /// The style's part of a case name,
    /// `alignment/join/w<width>[/caps-..][/dash..]`.
    pub fn name(&self) -> String {
        let join = match self.join {
            Join::Miter if self.miter_angle == DEFAULT_MITER_ANGLE => "miter".to_string(),
            Join::Miter => format!("miter{}", num(self.miter_angle)),
            Join::Bevel => "bevel".to_string(),
            Join::Round => "round".to_string(),
        };
        let mut name = format!("{}/{join}/w{}", self.align.label(), num(self.width));
        if (self.start_cap, self.end_cap) != (Cap::Butt, Cap::Butt) {
            name += &format!("/caps-{}-{}", cap(self.start_cap), cap(self.end_cap));
        }
        if let Some(d) = &self.dash {
            let pattern: Vec<String> = d.pattern.iter().map(|&v| num(v)).collect();
            name += &format!("/dash{}", pattern.join("-"));
            if d.offset != 0.0 {
                name += &format!("-off{}", num(d.offset));
            }
            if d.cap != Cap::Butt {
                name += &format!("-{}", cap(d.cap));
            }
        }
        name
    }
}

fn cap(c: Cap) -> &'static str {
    match c {
        Cap::Butt => "butt",
        Cap::Round => "round",
        Cap::Square => "square",
    }
}

/// A number for a case name, rounded to ten significant digits so scaled
/// values read cleanly: `0.00005`, not `0.000050000000000000004`.
pub fn num(v: f64) -> String {
    let rounded: f64 = format!("{v:.9e}").parse().unwrap_or(v);
    format!("{rounded}")
}

/// A named input path.
pub struct ShapeDef {
    /// Short name, the first component of case names.
    pub name: String,
    /// The path to stroke.
    pub path: BezPath,
}

/// One stroke to run through both versions and compare.
pub struct Case {
    /// Stable, descriptive name.
    pub name: String,
    /// Index into [`Corpus::shapes`].
    pub shape: usize,
    /// The style, already scaled to the shape.
    pub style: Style,
    /// The tolerance for both the stroke and the comparison.
    pub tol: f64,
}

/// All shapes and cases.
pub struct Corpus {
    /// The input paths.
    pub shapes: Vec<ShapeDef>,
    /// The cases, in a fixed order.
    pub cases: Vec<Case>,
    /// Problems met while building the corpus, such as a missing font.
    pub warnings: Vec<String>,
}

/// Collects shapes and cases. It scales each style to its shape, and drops
/// a case whose name is taken: the same name means the same parameters.
struct Builder {
    shapes: Vec<ShapeDef>,
    scales: Vec<f64>,
    cases: Vec<Case>,
    names: HashSet<String>,
}

impl Builder {
    fn shape(&mut self, name: &str, path: BezPath) -> usize {
        self.shape_scaled(name, path, 1.0)
    }

    /// A shape whose style lengths and tolerances scale by `scale`.
    fn shape_scaled(&mut self, name: &str, path: BezPath, scale: f64) -> usize {
        assert!(
            self.shapes.iter().all(|s| s.name != name),
            "duplicate shape {name}"
        );
        self.shapes.push(ShapeDef {
            name: name.to_string(),
            path,
        });
        self.scales.push(scale);
        self.shapes.len() - 1
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.shapes.iter().position(|s| s.name == name)
    }

    fn add(&mut self, shape: usize, style: &Style, tol: f64) {
        let k = self.scales[shape];
        let style = style.scaled(k);
        let tol = tol * k;
        let name = format!(
            "{}/{}/tol{}",
            self.shapes[shape].name,
            style.name(),
            num(tol)
        );
        if self.names.insert(name.clone()) {
            self.cases.push(Case {
                name,
                shape,
                style,
                tol,
            });
        }
    }

    fn add_all(&mut self, shape: usize, styles: &[Style], tol: f64) {
        for s in styles {
            self.add(shape, s, tol);
        }
    }

    /// Add `styles` at `tol` to each named shape that exists. A name with no
    /// shape is skipped, as the glyphs are when the font is missing.
    fn add_to(&mut self, shapes: &[&str], styles: &[Style], tol: f64) {
        for name in shapes {
            if let Some(i) = self.find(name) {
                self.add_all(i, styles, tol);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Style sets
// ---------------------------------------------------------------------------

const ALIGNS: [Align; 3] = [Align::Inside, Align::Center, Align::Outside];
const JOINS: [Join; 3] = [Join::Miter, Join::Bevel, Join::Round];
const CAP_PAIRS: [(Cap, Cap); 4] = [
    (Cap::Round, Cap::Square),
    (Cap::Square, Cap::Round),
    (Cap::Round, Cap::Round),
    (Cap::Square, Cap::Butt),
];

/// The general dash patterns: a two-entry pattern with round caps, an
/// offset with square caps, an odd-length pattern, round and square dots
/// made of zero-length dashes, a zero gap, a huge and a negative offset,
/// and one dash longer than any perimeter here.
fn dashes() -> Vec<Dash> {
    vec![
        Dash::new(&[20.0, 29.0], 0.0, Cap::Round),
        Dash::new(&[10.0, 5.0], 3.0, Cap::Square),
        Dash::new(&[5.0, 3.0, 1.0], 0.0, Cap::Butt),
        Dash::new(&[0.0, 24.0], 0.0, Cap::Round),
        Dash::new(&[0.0, 10.0], 0.0, Cap::Square),
        Dash::new(&[7.0, 0.0, 3.0, 2.0], 0.0, Cap::Butt),
        Dash::new(&[20.0, 29.0], 1e4 + 0.3, Cap::Round),
        Dash::new(&[20.0, 29.0], -17.5, Cap::Butt),
        Dash::new(&[1e5, 10.0], 0.0, Cap::Butt),
    ]
}

/// A very short period, for long paths.
fn short_period() -> Dash {
    Dash::new(&[0.7, 0.6], 0.0, Cap::Butt)
}

/// The full style set: every alignment with every join at four widths,
/// extreme widths, miter angles, side overrides, and every general dash
/// pattern at every alignment. When `open`, it also has caps.
fn full_styles(open: bool) -> Vec<Style> {
    use Align::*;
    let mut v = Vec::new();
    for align in ALIGNS {
        for join in JOINS {
            for w in [1.0, 4.0, 12.0, 60.0] {
                v.push(Style::new(align, join, w));
            }
        }
        v.push(Style::new(align, Join::Miter, 0.05));
        v.push(Style::new(align, Join::Round, 200.0));
    }
    for (align, angle, w) in [
        (Center, 10.0, 12.0),
        (Inside, 60.0, 12.0),
        (Outside, 120.0, 12.0),
        (Inside, 10.0, 4.0),
        (Outside, 60.0, 4.0),
        (Center, 120.0, 4.0),
    ] {
        v.push(Style::new(align, Join::Miter, w).miter(angle));
    }
    for (side, join, w) in [
        (Left, Join::Miter, 4.0),
        (Left, Join::Round, 12.0),
        (Right, Join::Bevel, 4.0),
        (Right, Join::Miter, 12.0),
        (SideCenter, Join::Round, 4.0),
        (SideCenter, Join::Bevel, 12.0),
    ] {
        v.push(Style::new(side, join, w));
    }
    if open {
        for (start, end) in CAP_PAIRS {
            for align in ALIGNS {
                let join = if align == Center {
                    Join::Round
                } else {
                    Join::Miter
                };
                v.push(Style::new(align, join, 8.0).caps(start, end));
            }
            v.push(Style::new(Center, Join::Miter, 60.0).caps(start, end));
        }
    }
    let dashes = dashes();
    for d in &dashes {
        for align in ALIGNS {
            v.push(Style::new(align, Join::Miter, 4.0).dash(d));
        }
    }
    if open {
        // The path's own ends keep the start and end caps. The ends a dash
        // makes get the dash cap.
        v.push(
            Style::new(Center, Join::Round, 6.0)
                .caps(Cap::Square, Cap::Round)
                .dash(&dashes[0]),
        );
        v.push(
            Style::new(Inside, Join::Miter, 6.0)
                .caps(Cap::Round, Cap::Square)
                .dash(&dashes[1]),
        );
    }
    v
}

/// A smaller set, for shapes that repeat a feature of a full-set shape.
fn lite_styles(open: bool) -> Vec<Style> {
    use Align::*;
    let mut v = Vec::new();
    for align in ALIGNS {
        for join in [Join::Miter, Join::Round] {
            for w in [4.0, 12.0, 60.0] {
                v.push(Style::new(align, join, w));
            }
        }
    }
    let dashes = dashes();
    for align in ALIGNS {
        v.push(Style::new(align, Join::Miter, 4.0).dash(&dashes[0]));
        v.push(Style::new(align, Join::Miter, 4.0).dash(&dashes[4]));
    }
    v.push(Style::new(Left, Join::Bevel, 6.0));
    v.push(Style::new(Right, Join::Miter, 6.0));
    v.push(Style::new(Inside, Join::Miter, 0.05));
    v.push(Style::new(Center, Join::Bevel, 12.0));
    if open {
        v.push(Style::new(Center, Join::Round, 8.0).caps(Cap::Round, Cap::Square));
        v.push(Style::new(Inside, Join::Miter, 8.0).caps(Cap::Square, Cap::Round));
    }
    v
}

/// The set for glyphs, at text-like widths.
fn glyph_styles() -> Vec<Style> {
    use Align::*;
    let mut v = Vec::new();
    for align in ALIGNS {
        for join in [Join::Miter, Join::Round] {
            for w in [2.0, 12.0] {
                v.push(Style::new(align, join, w));
            }
        }
    }
    let dashes = dashes();
    for align in ALIGNS {
        v.push(Style::new(align, Join::Miter, 4.0).dash(&dashes[0]));
    }
    v.push(Style::new(Center, Join::Miter, 4.0).dash(&dashes[4]));
    v.push(Style::new(Inside, Join::Round, 60.0));
    v.push(Style::new(Center, Join::Round, 60.0));
    v.push(Style::new(Left, Join::Miter, 4.0));
    v.push(Style::new(Outside, Join::Bevel, 30.0));
    v
}

/// The styles of `tests/golden.rs`.
fn golden_styles() -> Vec<Style> {
    use Align::*;
    vec![
        Style::new(Inside, Join::Miter, 12.0),
        Style::new(Outside, Join::Round, 10.0).caps(Cap::Round, Cap::Round),
        Style::new(Center, Join::Bevel, 14.0).caps(Cap::Square, Cap::Square),
        Style::new(Inside, Join::Miter, 60.0),
        Style::new(Inside, Join::Miter, 10.0).dash(&Dash::new(&[20.0, 29.0], 0.0, Cap::Round)),
        Style::new(Center, Join::Round, 8.0).dash(&Dash::new(&[10.0, 5.0], 3.0, Cap::Square)),
        Style::new(Left, Join::Miter, 9.0).miter(60.0),
        Style::new(Outside, Join::Miter, 11.0).dash(&Dash::new(&[0.0, 24.0], 0.0, Cap::Round)),
    ]
}

/// The styles of the finer-tolerance subsets.
fn fine_styles() -> Vec<Style> {
    use Align::*;
    let d = dashes();
    vec![
        Style::new(Inside, Join::Miter, 12.0),
        Style::new(Outside, Join::Round, 10.0),
        Style::new(Center, Join::Bevel, 14.0),
        Style::new(Inside, Join::Round, 60.0),
        Style::new(Center, Join::Miter, 0.05),
        Style::new(Inside, Join::Miter, 4.0).dash(&d[0]),
        Style::new(Outside, Join::Miter, 11.0).dash(&d[3]),
        Style::new(Center, Join::Round, 8.0).dash(&d[1]),
        Style::new(Right, Join::Miter, 2.0).dash(&d[2]),
        Style::new(Outside, Join::Bevel, 40.0),
        Style::new(Inside, Join::Bevel, 32.0),
        Style::new(Center, Join::Round, 120.0),
    ]
}

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

fn polyline(points: &[(f64, f64)]) -> BezPath {
    let mut p = BezPath::new();
    p.move_to(points[0]);
    for &q in &points[1..] {
        p.line_to(q);
    }
    p
}

fn polygon(points: &[(f64, f64)]) -> BezPath {
    let mut p = polyline(points);
    p.close_path();
    p
}

fn svg(d: &str) -> BezPath {
    BezPath::from_svg(d).expect("valid path data")
}

/// `2·points` vertices around (150, 150), alternating between the radii.
fn star(points: usize, r_outer: f64, r_inner: f64, phase: f64) -> BezPath {
    let pts: Vec<(f64, f64)> = (0..points * 2)
        .map(|i| {
            let r = if i % 2 == 0 { r_outer } else { r_inner };
            let a = PI * i as f64 / points as f64 + phase;
            (150.0 + r * a.cos(), 150.0 + r * a.sin())
        })
        .collect();
    polygon(&pts)
}

/// `2·teeth` vertices around (c, c), alternating tip and root radius.
fn gear(teeth: usize, c: f64, r_tip: f64, r_root: f64) -> BezPath {
    let pts: Vec<(f64, f64)> = (0..teeth * 2)
        .map(|i| {
            let r = if i % 2 == 0 { r_tip } else { r_root };
            let a = PI * i as f64 / teeth as f64;
            (c + r * a.cos(), c + r * a.sin())
        })
        .collect();
    polygon(&pts)
}

/// The bulging star of `tests/set_semantics.rs`. Its edges are quads that
/// bulge by `bulge` times their length. With enough bulge, the two flanks
/// of each tip cross.
fn bulging_star(points: usize, r_root: f64, bulge: f64, phase: f64) -> BezPath {
    let pts: Vec<Point> = (0..points)
        .map(|i| {
            let r = if i % 2 == 0 { 110.0 } else { r_root };
            let a = TAU * i as f64 / points as f64 + phase;
            Point::new(150.0 + r * a.cos(), 150.0 + r * a.sin())
        })
        .collect();
    let mut p = BezPath::new();
    p.move_to(pts[0]);
    for i in 1..=points {
        let (a, q) = (pts[i - 1], pts[i % points]);
        let d = q - a;
        p.quad_to(a.midpoint(q) + Vec2::new(-d.y, d.x) * bulge, q);
    }
    p.close_path();
    p
}

/// The `#` of `tests/set_semantics.rs`: one outline with a counter, its
/// slanted bars `t` thick.
fn thin_hash(t: f64) -> BezPath {
    let x = |x: f64, y: f64| (x + 0.3 * (140.0 - y), y);
    let (l, r) = (40.0, 80.0);
    let (u, d) = (60.0, 100.0);
    let (top, bottom, left, right) = (20.0, 140.0, 10.0, 120.0);
    let outline = [
        x(l, top),
        x(l + t, top),
        x(l + t, u),
        x(r, u),
        x(r, top),
        x(r + t, top),
        x(r + t, u),
        (right, u),
        (right, u + t),
        x(r + t, u + t),
        x(r + t, d),
        (right, d),
        (right, d + t),
        x(r + t, d + t),
        x(r + t, bottom),
        x(r, bottom),
        x(r, d + t),
        x(l + t, d + t),
        x(l + t, bottom),
        x(l, bottom),
        x(l, d + t),
        (left, d + t),
        (left, d),
        x(l, d),
        x(l, u + t),
        (left, u + t),
        (left, u),
        x(l, u),
    ];
    let counter = [x(l + t, u + t), x(l + t, d), x(r, d), x(r, u + t)];
    let mut p = polygon(&outline);
    p.extend(&polygon(&counter));
    p
}

/// `outer` with `holes` wound opposite to it.
fn with_holes(outer: BezPath, holes: impl IntoIterator<Item = BezPath>) -> BezPath {
    let sign = outer.area().signum();
    let mut p = outer;
    for h in holes {
        let h = if h.area().signum() == sign {
            h.reverse_subpaths()
        } else {
            h
        };
        p.extend(&h);
    }
    p
}

fn donut() -> BezPath {
    with_holes(
        Circle::new((130.0, 130.0), 100.0).to_path(1e-6),
        [Circle::new((130.0, 130.0), 45.0).to_path(1e-6)],
    )
}

/// A plate with six round and three rectangular holes.
fn plate() -> BezPath {
    let mut holes: Vec<BezPath> = [60.0, 110.0, 160.0]
        .into_iter()
        .flat_map(|x| [60.0, 180.0].map(|y| Circle::new((x, y), 15.0).to_path(1e-6)))
        .collect();
    for y in [40.0, 100.0, 160.0] {
        holes.push(Rect::new(210.0, y, 290.0, y + 35.0).to_path(1e-9));
    }
    with_holes(Rect::new(20.0, 20.0, 320.0, 220.0).to_path(1e-9), holes)
}

/// A closed Catmull-Rom spline through `pts`, as cubics.
fn closed_spline(pts: &[Point]) -> BezPath {
    let n = pts.len();
    let at = |i: isize| pts[i.rem_euclid(n as isize) as usize];
    let mut p = BezPath::new();
    p.move_to(pts[0]);
    for i in 0..n as isize {
        let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        p.curve_to(p1 + (p2 - p0) / 6.0, p2 - (p3 - p1) / 6.0, p2);
    }
    p.close_path();
    p
}

/// A trefoil: one closed curve whose three crossings interleave.
fn pretzel() -> BezPath {
    let pts: Vec<Point> = (0..36)
        .map(|i| {
            let t = TAU * f64::from(i) / 36.0;
            let (x, y) = (t.sin() + 2.0 * (2.0 * t).sin(), t.cos() - 2.0 * (2.0 * t).cos());
            Point::new(150.0 + 45.0 * x, 150.0 + 45.0 * y)
        })
        .collect();
    closed_spline(&pts)
}

/// The star polygon {5/2}: its middle pentagon winds twice.
fn pentagram() -> BezPath {
    let pts: Vec<(f64, f64)> = [0, 2, 4, 1, 3]
        .into_iter()
        .map(|k| {
            let a = -PI / 2.0 + TAU * f64::from(k) / 5.0;
            (150.0 + 120.0 * a.cos(), 150.0 + 120.0 * a.sin())
        })
        .collect();
    polygon(&pts)
}

/// Zero-length segments interleaved with real ones, duplicate points,
/// zero-length subpaths such as `M x L x` and `M x Z`, and a lone `MoveTo`.
fn zero_length() -> BezPath {
    svg(concat!(
        "M50,50 L50,50 L150,50 L150,50 L150,150 C150,150 150,150 150,150 ",
        "L50,150 L50,150 L50,50 Z ",
        "M200,50 L200,50 ",
        "M220,80 Z ",
        "M200,100 L260,100 L260,100 L260,180 L200,180 Z ",
        "M30,200 L30,200 L30,200 ",
        "M20,250 L100,250 L100,250 C100,250 140,250 180,270 L180,270 ",
        "M240,240"
    ))
}

fn wavy(n: usize) -> BezPath {
    let mut p = BezPath::new();
    p.move_to((0.0, 0.0));
    for i in 0..n {
        let x = i as f64 * 10.0;
        p.curve_to((x + 3.0, 15.0), (x + 7.0, -15.0), (x + 10.0, 0.0));
    }
    p
}

fn scaled(path: &BezPath, k: f64) -> BezPath {
    Affine::scale(k) * path.clone()
}

/// The compound path of `tests/golden.rs`: an open polyline, a rectangle
/// and a reversed circle.
fn mixed() -> BezPath {
    let mut p = polyline(&[(20.0, 40.0), (120.0, 20.0), (220.0, 40.0)]);
    p.extend(&Rect::new(40.0, 80.0, 120.0, 140.0).to_path(1e-9));
    p.extend(
        &Circle::new((190.0, 110.0), 35.0)
            .to_path(1e-6)
            .reverse_subpaths(),
    );
    p
}

fn bowtie() -> BezPath {
    polygon(&[(20.0, 20.0), (220.0, 180.0), (220.0, 20.0), (20.0, 180.0)])
}

/// The figure-eight of `tests/golden.rs`, which starts at its crossing.
fn figure_eight() -> BezPath {
    svg(concat!(
        "M120,100 C150,55 210,55 240,100 C210,145 150,145 120,100 ",
        "C90,55 30,55 0,100 C30,145 90,145 120,100 Z"
    ))
}

/// Inter's `$` from `tests/set_semantics.rs`, its S and bar merged by a
/// boolean operation. Where they meet, it has segments about `1e-13` long.
const MERGED_DOLLAR: &str = concat!(
    "M76.25,40.814558236183629 L76.25,15.625 L91.25,15.625 ",
    "L91.250000000001478,40.547558813597533 ",
    "C101.11762911921608,41.138567222814572 109.99262911921551,43.247714284948728 117.875,46.875 ",
    "C127.29166666666663,51.208333333333329 134.72916666666663,57.104166666666671 140.1875,64.5625 ",
    "C145.64583333333326,72.020833333333329 148.54166666666674,80.458333333333329 148.875,89.875 ",
    "L125.625,89.875 ",
    "C124.54166666666663,80.791666666666671 120.20833333333337,73.770833333333329 112.625,68.8125 ",
    "C106.60341596006481,64.875310435427025 99.478415963578527,62.501293475968239 91.250000010541044,61.690449121623629 ",
    "L91.25,124.84563758389262 L98.625,126.875 ",
    "C103.625,128.20833333333334 109.14583333333337,130.04166666666666 115.1875,132.375 ",
    "C121.22916666666663,134.70833333333334 127,137.8125 132.5,141.6875 ",
    "C138,145.5625 142.52083333333326,150.47916666666666 146.0625,156.4375 ",
    "C149.60416666666674,162.39583333333334 151.375,169.70833333333334 151.375,178.375 ",
    "C151.375,188.54166666666666 148.72916666666674,197.70833333333334 143.4375,205.875 ",
    "C138.14583333333326,214.04166666666666 130.45833333333337,220.5 120.375,225.25 ",
    "C112.09838184050056,229.14890276935097 102.3900488461951,231.44765366450693 91.250001017083491,232.14625268546786 ",
    "L91.25,256.375 L76.25,256.375 L76.25,232.13747994526159 ",
    "L76.249999999999659,232.13747994526159 ",
    "C65.945643797751245,231.46747343449425 56.799810464417988,229.37998011940704 48.8125,225.875 ",
    "C38.9375,221.54166666666666 31.208333333333371,215.47916666666666 25.625,207.6875 ",
    "C20.041666666666629,199.89583333333334 16.875,190.79166666666666 16.125,180.375 ",
    "L40.5,180.375 ",
    "C41.166666666666629,187.375 43.541666666666629,193.14583333333334 47.625,197.6875 ",
    "C51.708333333333371,202.22916666666666 56.9375,205.60416666666666 63.3125,207.8125 ",
    "C67.441472301617978,209.24279759467814 71.75397230161775,210.20990623629768 76.249999999999204,210.71382592485867 ",
    "L76.25,210.71382592485867 L76.25,143.46458333333334 L68.875,141.375 ",
    "C53.875,137.125 42.208333333333371,131.04166666666666 33.875,123.125 ",
    "C25.541666666666629,115.20833333333334 21.375,105.08333333333333 21.375,92.75 ",
    "C21.375,82.25 24.208333333333371,73.0625 29.875,65.1875 ",
    "C35.541666666666629,57.3125 43.208333333333371,51.208333333333329 52.875,46.875 ",
    "C60.027880824825729,43.668536181974673 67.819547491492926,41.648388927369211 76.250000000001592,40.814558236183629 ",
    "L76.25,40.814558236183629 Z M76.25,61.900408726538899 ",
    "C71.626369999137523,62.509023759797358 67.397203332470895,63.646387517617725 63.5625,65.3125 ",
    "C57.520833333333371,67.9375 52.833333333333371,71.520833333333329 49.5,76.0625 ",
    "C46.166666666666629,80.604166666666671 44.5,85.791666666666671 44.5,91.625 ",
    "C44.5,98.125 46.520833333333371,103.35416666666667 50.5625,107.3125 ",
    "C54.604166666666629,111.27083333333334 59.395833333333371,114.35416666666666 64.9375,116.5625 ",
    "C68.986357809295896,118.17595461573447 72.757191147566573,119.53362671812556 76.25000001481169,120.63551630717319 ",
    "L76.25,61.900408726538899 Z M91.25,147.71458333333334 L91.25,210.73731712447102 ",
    "L91.250000000000568,210.73731712447102 ",
    "C96.723224689003359,210.15271132218885 101.78572468900325,208.90693894736515 106.4375,207 ",
    "C113.14583333333337,204.25 118.45833333333337,200.375 122.375,195.375 ",
    "C126.29166666666663,190.375 128.25,184.58333333333334 128.25,178 ",
    "C128.25,172 126.5625,167.125 123.1875,163.375 ",
    "C119.8125,159.625 115.33333333333337,156.54166666666666 109.75,154.125 ",
    "C104.16666666666663,151.70833333333334 98.041666666666629,149.58333333333334 91.375,147.75 ",
    "L91.25,147.71458333333334 Z",
);

/// The cubics of `tests/regressions.rs`: degenerate and near-cusp cubics
/// from vello #388 and kurbo's own suite, plus kurbo #344.
fn broken_cubics() -> Vec<BezPath> {
    let cubics: [[(f64, f64); 4]; 11] = [
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
        [(51.0, 0.0), (-0.0859375, 161.640625), (0.0, 164.0), (0.0, 164.0)],
    ];
    cubics
        .iter()
        .map(|c| CubicBez::new(c[0], c[1], c[2], c[3]).into_path(0.1))
        .collect()
}

/// Whether any subpath of `path` is open.
fn has_open_subpath(path: &BezPath) -> bool {
    let mut open = false;
    let mut in_subpath = false;
    for el in path.elements() {
        match el {
            PathEl::MoveTo(_) => {
                open |= in_subpath;
                in_subpath = false;
            }
            PathEl::ClosePath => in_subpath = false,
            _ => in_subpath = true,
        }
    }
    open || in_subpath
}

// ---------------------------------------------------------------------------
// Random paths
// ---------------------------------------------------------------------------

/// A xorshift64* generator with no dependencies. It is deterministic, so
/// the random cases are the same on every run.
pub struct XorShift(u64);

impl XorShift {
    /// A generator from any seed. The state is made odd, so it is never
    /// zero: xorshift never leaves zero.
    pub fn new(seed: u64) -> Self {
        XorShift(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `[lo, hi)`.
    pub fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        lo + (hi - lo) * unit
    }

    /// Uniform in `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// A uniformly chosen element.
    pub fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

/// A random path of one of six kinds: a polygon (often self-crossing), a
/// simple radial polygon, a closed curve path, an open polyline, an open
/// curve path, or two overlapping radial polygons.
fn random_path(rng: &mut XorShift, kind: usize) -> BezPath {
    let point = |rng: &mut XorShift| Point::new(rng.uniform(20.0, 280.0), rng.uniform(20.0, 280.0));
    let radial = |rng: &mut XorShift, c: Point, n: usize| {
        let pts: Vec<(f64, f64)> = (0..n)
            .map(|i| {
                let a = TAU * (i as f64 + rng.uniform(0.0, 0.8)) / n as f64;
                let r = rng.uniform(40.0, 130.0);
                (c.x + r * a.cos(), c.y + r * a.sin())
            })
            .collect();
        polygon(&pts)
    };
    let curves = |rng: &mut XorShift, n: usize| {
        let mut p = BezPath::new();
        p.move_to(point(rng));
        for _ in 0..n {
            if rng.below(2) == 0 {
                p.quad_to(point(rng), point(rng));
            } else {
                p.curve_to(point(rng), point(rng), point(rng));
            }
        }
        p
    };
    match kind {
        0 => {
            let n = 3 + rng.below(8);
            let pts: Vec<(f64, f64)> = (0..n).map(|_| point(rng).into()).collect();
            polygon(&pts)
        }
        1 => {
            let n = 5 + rng.below(12);
            radial(rng, Point::new(150.0, 150.0), n)
        }
        2 => {
            let n = 2 + rng.below(5);
            let mut p = curves(rng, n);
            p.close_path();
            p
        }
        3 => {
            let n = 2 + rng.below(7);
            let pts: Vec<(f64, f64)> = (0..n).map(|_| point(rng).into()).collect();
            polyline(&pts)
        }
        4 => {
            let n = 1 + rng.below(4);
            curves(rng, n)
        }
        _ => {
            let (n, m) = (5 + rng.below(6), 5 + rng.below(6));
            let mut p = radial(rng, Point::new(120.0, 140.0), n);
            let q = radial(rng, Point::new(180.0, 160.0), m);
            p.extend(&if rng.below(2) == 0 {
                q
            } else {
                q.reverse_subpaths()
            });
            p
        }
    }
}

fn random_style(rng: &mut XorShift, open: bool) -> Style {
    use Align::*;
    let align = rng.pick(&[Inside, Inside, Center, Center, Outside, Outside, Left, Right]);
    let join = rng.pick(&JOINS);
    let width = rng.pick(&[1.0, 3.0, 8.0, 20.0, 50.0]);
    let mut s = Style::new(align, join, width);
    if join == Join::Miter && rng.below(3) == 0 {
        s = s.miter(rng.pick(&[10.0, 60.0, 120.0]));
    }
    if open {
        let caps = [Cap::Butt, Cap::Round, Cap::Square];
        s = s.caps(rng.pick(&caps), rng.pick(&caps));
    }
    if rng.below(5) < 2 {
        let dashes = dashes();
        s = s.dash(&dashes[rng.below(dashes.len())]);
    }
    s
}

// ---------------------------------------------------------------------------
// The corpus
// ---------------------------------------------------------------------------

impl Corpus {
    /// Build every shape and case. Glyph cases need the font from
    /// [`glyphs::font_path`]. Without it, they are skipped with a warning.
    pub fn build() -> Corpus {
        use Align::*;
        let mut b = Builder {
            shapes: Vec::new(),
            scales: Vec::new(),
            cases: Vec::new(),
            names: HashSet::new(),
        };
        let mut warnings = Vec::new();
        let tol = 0.25;

        // Shapes with the full style set.
        let full: Vec<(&str, BezPath)> = vec![
            ("line", polyline(&[(20.0, 30.0), (280.0, 170.0)])),
            (
                "polyline",
                polyline(&[(20.0, 40.0), (120.0, 20.0), (220.0, 40.0), (180.0, 120.0)]),
            ),
            (
                "zigzag",
                polyline(&[
                    (20.0, 180.0),
                    (40.0, 20.0),
                    (60.0, 180.0),
                    (80.0, 20.0),
                    (100.0, 180.0),
                    (120.0, 20.0),
                    (140.0, 180.0),
                ]),
            ),
            (
                "u-turns",
                polyline(&[
                    (20.0, 60.0),
                    (260.0, 60.0),
                    (60.0, 60.0),
                    (60.0, 160.0),
                    (60.0, 100.0),
                    (240.0, 160.0),
                ]),
            ),
            ("cubic-loop", svg("M20,150 C200,20 0,20 180,150")),
            ("cubic-cusp", svg("M50,200 C200,50 0,50 250,200")),
            ("cubic-s", svg("M20,150 C120,-50 180,350 280,150")),
            (
                "arc-ellipse",
                Arc::new((150.0, 150.0), (120.0, 60.0), -PI / 4.0, 1.5 * PI, 0.3).to_path(1e-6),
            ),
            (
                "crossing-polyline",
                polyline(&[(0.0, 0.0), (100.0, 100.0), (100.0, 0.0), (0.0, 100.0)]),
            ),
            ("wavy40", wavy(40)),
            ("zero-length", zero_length()),
            ("mixed", mixed()),
            ("circle", Circle::new((120.0, 120.0), 70.0).to_path(1e-6)),
            ("rect", Rect::new(40.0, 80.0, 160.0, 140.0).to_path(1e-9)),
            (
                "rrect",
                RoundedRect::new(30.0, 40.0, 270.0, 200.0, 30.0).to_path(1e-6),
            ),
            ("star5", star(5, 100.0, 40.0, 0.0)),
            ("star10", star(10, 100.0, 60.0, 0.0)),
            ("star24", star(24, 110.0, 30.0, 0.0)),
            ("bulging-star24", bulging_star(24, 60.0, 0.15, 0.1)),
            ("gear24", gear(24, 150.0, 110.0, 90.0)),
            ("donut", donut()),
            ("plate", plate()),
            (
                "circles-overlap",
                {
                    let mut p = Circle::new((110.0, 150.0), 80.0).to_path(1e-6);
                    p.extend(&Circle::new((190.0, 150.0), 80.0).to_path(1e-6));
                    p
                },
            ),
            ("bowtie", bowtie()),
            ("figure-eight", figure_eight()),
            ("pretzel", pretzel()),
            ("pentagram", pentagram()),
            (
                "wedge",
                polygon(&[(20.0, 180.0), (230.0, 160.0), (20.0, 140.0)]),
            ),
            ("hash-3.8", thin_hash(3.8)),
        ];
        for (name, path) in full {
            let i = b.shape(name, path);
            let styles = full_styles(has_open_subpath(&b.shapes[i].path));
            b.add_all(i, &styles, tol);
        }
        // Tiny and huge coordinates. Every length of the style scales with
        // the shape, and so does the tolerance.
        for (name, path, k) in [
            ("star5-tiny", star(5, 100.0, 40.0, 0.0), 1e-3),
            ("star5-huge", star(5, 100.0, 40.0, 0.0), 1e5),
        ] {
            let i = b.shape_scaled(name, scaled(&path, k), k);
            b.add_all(i, &full_styles(false), tol);
        }

        // Shapes with the lite style set.
        let lite: Vec<(&str, BezPath)> = vec![
            (
                "zigzag-fine",
                polyline(
                    &(0..21)
                        .map(|i| (20.0 + 4.0 * f64::from(i), if i % 2 == 0 { 180.0 } else { 20.0 }))
                        .collect::<Vec<_>>(),
                ),
            ),
            (
                "cubic-near-collinear",
                svg("M20,100 C100,100.5 200,99.5 280,100"),
            ),
            ("cubic-collinear", svg("M20,100 C100,100 200,100 280,100")),
            (
                "cubic-collinear-backtrack",
                svg("M20,100 C300,100 0,100 280,100"),
            ),
            ("quad", svg("M20,150 Q150,-50 280,150")),
            ("quad-sharp", svg("M20,150 Q300,100 20,140")),
            (
                "arc-quarter",
                Arc::new((150.0, 150.0), (100.0, 100.0), 0.0, PI / 2.0, 0.0).to_path(1e-6),
            ),
            ("circle-small", Circle::new((150.0, 150.0), 6.0).to_path(1e-6)),
            (
                "ellipse",
                Ellipse::new((150.0, 120.0), (130.0, 50.0), 0.4).to_path(1e-6),
            ),
            ("rect-thin", Rect::new(20.0, 100.0, 280.0, 104.0).to_path(1e-9)),
            (
                "rrect-pill",
                RoundedRect::new(30.0, 100.0, 270.0, 160.0, 30.0).to_path(1e-6),
            ),
            ("star7", star(7, 100.0, 55.0, 0.0)),
            ("bulging-star16", bulging_star(16, 60.0, 0.3, 0.0)),
            (
                "rect-circle-overlap",
                {
                    let mut p = Rect::new(40.0, 60.0, 200.0, 200.0).to_path(1e-9);
                    p.extend(&Circle::new((200.0, 90.0), 60.0).to_path(1e-6));
                    p
                },
            ),
            (
                "nested-same-dir",
                {
                    let mut p = Rect::new(20.0, 20.0, 280.0, 280.0).to_path(1e-9);
                    p.extend(&Rect::new(70.0, 70.0, 230.0, 230.0).to_path(1e-9));
                    p.extend(&Rect::new(120.0, 120.0, 180.0, 180.0).to_path(1e-9));
                    p
                },
            ),
            (
                "bowtie-from-crossing",
                polygon(&[
                    (120.0, 100.0),
                    (220.0, 180.0),
                    (220.0, 20.0),
                    (120.0, 100.0),
                    (20.0, 180.0),
                    (20.0, 20.0),
                ]),
            ),
            (
                "figure-eight-offstart",
                svg(concat!(
                    "M240,100 C210,145 150,145 120,100 C90,55 30,55 0,100 ",
                    "C30,145 90,145 120,100 C150,55 210,55 240,100 Z"
                )),
            ),
            (
                "wedge-thin",
                polygon(&[(20.0, 152.0), (280.0, 150.0), (20.0, 148.0)]),
            ),
            ("hash-7.8", thin_hash(7.8)),
            (
                "whole-hexagon",
                polygon(&[
                    (100.0, 100.0),
                    (108.0, 94.0),
                    (123.0, 130.0),
                    (93.0, 146.0),
                    (78.0, 110.0),
                    (70.0, 116.0),
                ]),
            ),
            (
                "whole-bowtie",
                polygon(&[(0.0, 0.0), (40.0, 30.0), (40.0, 0.0), (0.0, 30.0)]),
            ),
            (
                "whole-bowtie-from-crossing",
                polygon(&[
                    (20.0, 15.0),
                    (40.0, 30.0),
                    (40.0, 0.0),
                    (20.0, 15.0),
                    (0.0, 30.0),
                    (0.0, 0.0),
                ]),
            ),
            (
                "sub-resolution-square",
                {
                    let mut p = polygon(&[
                        (0.0, 0.0),
                        (100.0, 0.0),
                        (100.0, 100.0),
                        (0.0, 100.0),
                        (0.0, 1e-13),
                    ]);
                    p.extend(
                        &Rect::new(30.0, 30.0, 70.0, 70.0)
                            .to_path(1e-9)
                            .reverse_subpaths(),
                    );
                    p.extend(&Rect::new(130.0, 0.0, 160.0, 30.0).to_path(1e-9));
                    p
                },
            ),
            (
                "leaning-hole",
                with_holes(
                    Rect::new(0.0, 0.0, 100.0, 100.0).to_path(1e-9),
                    [polygon(&[(60.0, 80.0), (60.0, 20.0), (62.0, 10.0), (20.0, 80.0)])],
                ),
            ),
            ("merged-dollar", svg(MERGED_DOLLAR)),
            (
                "notch-bar",
                polygon(&[
                    (0.0, 0.0),
                    (93.0, 0.0),
                    (100.0, 25.0),
                    (107.0, 0.0),
                    (200.0, 0.0),
                    (200.0, 30.0),
                    (115.0, 30.0),
                    (100.0, 40.0),
                    (85.0, 30.0),
                    (0.0, 30.0),
                ]),
            ),
            (
                "spike-notch",
                polygon(&[
                    (0.0, 0.0),
                    (200.0, 0.0),
                    (200.0, 50.0),
                    (138.3, 50.0),
                    (130.0, 80.0),
                    (121.7, 50.0),
                    (60.0, 50.0),
                    (60.0, 100.0),
                    (115.0, 100.0),
                    (130.0, 85.0),
                    (145.0, 100.0),
                    (200.0, 100.0),
                    (200.0, 150.0),
                    (0.0, 150.0),
                ]),
            ),
            // The shapes of the benchmark suite, `benches/expand.rs`.
            ("bench-circle", Circle::new((100.0, 100.0), 50.0).to_path(1e-9)),
            (
                "bench-donut",
                with_holes(
                    Circle::new((130.0, 130.0), 100.0).to_path(1e-9),
                    [Circle::new((130.0, 130.0), 45.0).to_path(1e-9)],
                ),
            ),
            // 400 segments: fewer than the 750-tooth bench gear has, to keep
            // the run short, but still far denser than the width.
            ("gear200", gear(200, 350.0, 300.0, 290.0)),
        ];
        for (name, path) in lite {
            let i = b.shape(name, path);
            let styles = lite_styles(has_open_subpath(&b.shapes[i].path));
            b.add_all(i, &styles, tol);
        }
        for (name, path, k) in [
            ("donut-tiny", donut(), 1e-3),
            ("figure-eight-huge", figure_eight(), 1e5),
        ] {
            let i = b.shape_scaled(name, scaled(&path, k), k);
            b.add_all(i, &lite_styles(false), tol);
        }

        // Glyphs bring holes and quads, and Inter's `4` has overlapping
        // contours.
        let font_path = glyphs::font_path();
        match std::fs::read(&font_path) {
            Ok(data) => match skrifa::FontRef::new(&data) {
                Ok(font) => {
                    for ch in "&@$%#?48gBQRSaek".chars() {
                        let Some(path) = glyphs::glyph(&font, ch, 200.0, Point::new(20.0, 220.0))
                        else {
                            warnings.push(format!("font has no outline for {ch:?}"));
                            continue;
                        };
                        let i = b.shape(&format!("glyph-{ch}"), path);
                        b.add_all(i, &glyph_styles(), tol);
                    }
                }
                Err(e) => warnings.push(format!(
                    "cannot parse font {}: {e}; glyph cases skipped",
                    font_path.display()
                )),
            },
            Err(e) => warnings.push(format!(
                "cannot read font {} ({e}); glyph cases skipped (set {})",
                font_path.display(),
                glyphs::FONT_ENV
            )),
        }

        // Degenerate cubics from upstream reports.
        for (k, path) in broken_cubics().into_iter().enumerate() {
            let i = b.shape(&format!("broken-cubic{k}"), path);
            for align in ALIGNS {
                for join in [Join::Miter, Join::Round] {
                    b.add(i, &Style::new(align, join, 30.0), tol);
                }
            }
            b.add(
                i,
                &Style::new(Center, Join::Round, 30.0).caps(Cap::Round, Cap::Square),
                tol,
            );
            b.add(
                i,
                &Style::new(Inside, Join::Miter, 2.0).dash(&dashes()[2]),
                tol,
            );
        }

        // The cases of `tests/golden.rs`, with its phased star.
        let golden_star = b.shape("star5-golden", star(5, 100.0, 40.0, -0.3));
        b.add_all(golden_star, &golden_styles(), tol);
        b.add_to(
            &[
                "circle",
                "rect",
                "donut",
                "bowtie",
                "figure-eight",
                "wedge",
                "polyline",
                "cubic-loop",
                "mixed",
            ],
            &golden_styles(),
            tol,
        );
        b.add_to(
            &["circle", "rect", "star5-golden"],
            &[Style::new(Inside, Join::Miter, 12.0)],
            1e-3,
        );

        // The styles of `benches/expand.rs`.
        let bench_dash = Dash::new(&[12.0, 6.0], 0.0, Cap::Round);
        let bench = [
            Style::new(Inside, Join::Miter, 8.0),
            Style::new(Center, Join::Miter, 8.0),
            Style::new(Inside, Join::Miter, 8.0).dash(&bench_dash),
        ];
        b.add_to(&["bench-circle", "bench-donut", "star5", "wavy40"], &bench, tol);

        // A very short dash period on long paths.
        let short: Vec<Style> = ALIGNS
            .iter()
            .flat_map(|&a| [1.0, 4.0].map(|w| Style::new(a, Join::Miter, w).dash(&short_period())))
            .collect();
        b.add_to(
            &["wavy40", "circle", "star5", "polyline", "glyph-&"],
            &short,
            tol,
        );

        Self::changelog_cases(&mut b, tol);
        Self::random_cases(&mut b, tol);

        // Finer tolerances.
        b.add_to(
            &[
                "star5",
                "circle",
                "donut",
                "bowtie",
                "figure-eight",
                "cubic-loop",
                "wavy40",
                "gear24",
                "rrect",
                "plate",
                "polyline",
                "zigzag",
                "cubic-cusp",
                "arc-ellipse",
                "bulging-star24",
                "hash-3.8",
                "mixed",
                "pentagram",
                "pretzel",
                "glyph-&",
                "glyph-g",
                "glyph-4",
                "glyph-B",
            ],
            &fine_styles(),
            0.05,
        );
        let finest: Vec<Style> = vec![
            Style::new(Inside, Join::Miter, 12.0),
            Style::new(Outside, Join::Round, 10.0),
            Style::new(Center, Join::Bevel, 14.0),
            Style::new(Inside, Join::Round, 60.0),
            Style::new(Inside, Join::Miter, 4.0).dash(&dashes()[0]),
            Style::new(Outside, Join::Miter, 11.0).dash(&dashes()[3]),
            Style::new(Left, Join::Miter, 9.0).miter(60.0),
            Style::new(Center, Join::Miter, 0.05),
            Style::new(Inside, Join::Round, 80.0),
            Style::new(Center, Join::Round, 2.0),
        ];
        b.add_to(
            &[
                "star5",
                "circle",
                "donut",
                "bowtie",
                "figure-eight",
                "cubic-loop",
                "polyline",
                "gear24",
                "rect",
                "glyph-&",
            ],
            &finest,
            1e-3,
        );

        Corpus {
            shapes: b.shapes,
            cases: b.cases,
            warnings,
        }
    }

    /// The shapes and styles that the 0.2.0 fixes in CHANGELOG.md were found
    /// on.
    fn changelog_cases(b: &mut Builder, tol: f64) {
        use Align::*;

        // Dash phase across self-intersections. The band goes on the right,
        // so its left edge is the centerline, and the offsets land on
        // pattern boundaries.
        let patterns: [&[f64]; 5] = [
            &[15.0, 15.0],
            &[9.0, 7.0],
            &[4.0, 2.0, 1.0, 2.0],
            &[6.0, 3.0, 2.0],
            &[5.0, 0.0, 5.0, 10.0],
        ];
        let phase_shapes = [
            "crossing-polyline",
            "cubic-loop",
            "bowtie",
            "bowtie-from-crossing",
            "figure-eight-offstart",
            "whole-bowtie",
            "whole-bowtie-from-crossing",
            "whole-hexagon",
        ];
        let mut phase = Vec::new();
        for offset in [0.0, 4.0, -11.0, 6.0] {
            for pattern in patterns {
                phase.push(Style::new(Right, Join::Miter, 2.0).dash(&Dash::new(
                    pattern,
                    offset,
                    Cap::Butt,
                )));
            }
            phase.push(Style::new(Right, Join::Miter, 2.0).dash(&Dash::new(
                &[0.0, 7.0],
                offset,
                Cap::Round,
            )));
        }
        b.add_to(&phase_shapes, &phase, tol);
        b.add_to(
            &phase_shapes,
            &[Style::new(Right, Join::Miter, 2.0).dash(&Dash::new(
                &[4.0, 2.0, 1.0, 2.0],
                5.0,
                Cap::Butt,
            ))],
            1e-3,
        );
        // Stray dots and discs where the pattern restarts on whole-number
        // geometry, and zero-length entries that land exactly on segment
        // ends.
        b.add_to(
            &["whole-hexagon"],
            &[
                Style::new(Right, Join::Miter, 2.0).dash(&Dash::new(&[0.0, 5.0], 14.0, Cap::Round)),
                Style::new(Right, Join::Miter, 2.0).dash(&Dash::new(&[1.0, 6.0], 2.0, Cap::Round)),
            ],
            1e-3,
        );
        let open_zero = b.shape(
            "zero-entry-polyline",
            polyline(&[
                (100.0, 100.0),
                (124.0, 90.0),
                (100.0, 80.0),
                (116.0, 50.0),
                (104.0, 45.0),
                (104.0, 70.0),
                (119.0, 78.0),
                (107.0, 69.0),
                (117.0, 69.0),
            ]),
        );
        let closed_zero = b.shape(
            "zero-entry-polygon",
            polygon(&[
                (100.0, 100.0),
                (136.0, 115.0),
                (146.0, 115.0),
                (110.0, 100.0),
                (115.0, 112.0),
                (105.0, 112.0),
            ]),
        );
        for (shape, pattern, offset) in [
            (open_zero, [0.0, 7.0], 12.0),
            (closed_zero, [6.0, 0.0], 3.0),
        ] {
            for align in [Inside, Outside] {
                for cap in [Cap::Round, Cap::Butt] {
                    let s = Style::new(align, Join::Miter, 2.0).dash(&Dash::new(&pattern, offset, cap));
                    b.add(shape, &s, tol);
                    b.add(shape, &s, 1e-3);
                }
            }
        }

        // A dash running to the end of an open line keeps the end cap.
        let short_line = b.shape("line-short", polyline(&[(1.1, 0.0), (7.3, 0.0)]));
        let one_dash = Dash::new(&[100.0, 1.0], 0.0, Cap::Butt);
        for t in [tol, 0.05, 1e-3] {
            for s in [
                Style::new(Center, Join::Miter, 2.0).caps(Cap::Butt, Cap::Round),
                Style::new(Center, Join::Miter, 2.0)
                    .caps(Cap::Butt, Cap::Round)
                    .dash(&one_dash),
                Style::new(Center, Join::Miter, 2.0)
                    .caps(Cap::Square, Cap::Round)
                    .dash(&Dash::new(&[100.0, 1.0], 0.0, Cap::Round)),
                Style::new(Inside, Join::Miter, 2.0)
                    .caps(Cap::Round, Cap::Square)
                    .dash(&one_dash),
            ] {
                b.add(short_line, &s, t);
            }
        }

        // Bevels and miters at widths near the corner spacing.
        let mut corners = Vec::new();
        for (join, angle) in [
            (Join::Bevel, DEFAULT_MITER_ANGLE),
            (Join::Miter, DEFAULT_MITER_ANGLE),
            (Join::Miter, 150.0),
        ] {
            for w in [7.5, 16.0, 20.0, 32.0, 48.0] {
                corners.push(Style::new(Inside, join, w).miter(angle));
            }
            for w in [40.0, 48.0] {
                corners.push(Style::new(Outside, join, w).miter(angle));
            }
            corners.push(Style::new(Center, join, 32.0).miter(angle));
        }
        b.add_to(&["star5", "star7", "gear24"], &corners, tol);
        let dense_miters: Vec<Style> = [48.0, 100.0, 250.0]
            .map(|w| Style::new(Outside, Join::Miter, w))
            .to_vec();
        b.add_to(&["gear24"], &dense_miters, tol);
        let one_sided_joins = [
            Style::new(Outside, Join::Miter, 20.0),
            Style::new(Inside, Join::Miter, 20.0),
        ];
        b.add_to(&["notch-bar", "spike-notch"], &one_sided_joins, tol);
        b.add_to(&["notch-bar", "spike-notch"], &one_sided_joins, 1e-3);

        // Spiky gears whose erosion is close to collapse.
        let mut spiky: Vec<Style> = [60.0, 65.0, 70.0, 75.0, 80.0]
            .map(|w| Style::new(Inside, Join::Round, w))
            .to_vec();
        spiky.push(Style::new(Center, Join::Round, 120.0));
        b.add_to(&["gear24"], &spiky, tol);
        b.add_to(&["gear24"], &spiky, 0.05);
        b.add_to(&["gear24"], &[Style::new(Inside, Join::Round, 80.0)], 1e-3);
        let fine_gear: Vec<Style> = [4.0, 8.0, 12.0, 20.0, 30.0]
            .into_iter()
            .flat_map(|w| [Join::Round, Join::Miter].map(|j| Style::new(Inside, j, w)))
            .collect();
        b.add_to(&["gear200"], &fine_gear, tol);
        let gear750 = b.shape("gear750", gear(750, 400.0, 300.0, 290.0));
        for s in [
            Style::new(Inside, Join::Round, 4.0),
            Style::new(Inside, Join::Round, 8.0),
            Style::new(Inside, Join::Miter, 8.0),
            Style::new(Inside, Join::Round, 30.0),
        ] {
            b.add(gear750, &s, tol);
        }

        // Fill probes and crossing refinement on bulging stars.
        let bulging: Vec<Style> = [Inside, Outside]
            .into_iter()
            .flat_map(|a| [4.0, 6.0, 10.0].map(|w| Style::new(a, Join::Round, w)))
            .collect();
        b.add_to(&["bulging-star24", "bulging-star16"], &bulging, tol);
        b.add_to(&["bulging-star24", "bulging-star16"], &bulging, 0.05);

        // The loop of a self-crossing curve: its fill side, its dash mask
        // at width 10, and one dash that runs once round the loop as a
        // single curve. That dash's chord is zero, which `group_len` in
        // `src/dash.rs` has to allow for.
        let long_dash = Dash::new(&[1000.0, 1.0], 0.0, Cap::Butt);
        let mut loops = Vec::new();
        for w in [1.0, 3.0, 10.0] {
            for align in [Inside, Outside, Left, Right] {
                loops.push(Style::new(align, Join::Miter, w));
                loops.push(Style::new(align, Join::Miter, w).dash(&long_dash));
            }
        }
        for d in &dashes()[..4] {
            loops.push(Style::new(Inside, Join::Miter, 10.0).dash(d));
        }
        b.add_to(&["cubic-loop"], &loops, tol);
        b.add_to(&["cubic-loop"], &loops[..8], 1e-3);

        // Bars thinner than twice the width, and overlapping contours
        // stroked centered.
        b.add_to(&["hash-3.8"], &[Style::new(Inside, Join::Miter, 2.0)], tol);
        b.add_to(&["hash-7.8"], &[Style::new(Inside, Join::Miter, 4.0)], tol);
        b.add_to(&["glyph-4"], &[Style::new(Center, Join::Miter, 2.0)], 0.0625);
        let merged = [
            Style::new(Inside, Join::Round, 8.0),
            Style::new(Outside, Join::Round, 8.0),
            Style::new(Center, Join::Round, 8.0),
        ];
        b.add_to(&["merged-dollar"], &merged, tol);
        b.add_to(&["merged-dollar"], &merged, 0.0625);
        b.add_to(
            &["sub-resolution-square", "leaning-hole"],
            &[
                Style::new(Inside, Join::Round, 8.0),
                Style::new(Center, Join::Round, 8.0),
                Style::new(Inside, Join::Round, 4.0),
            ],
            tol,
        );
    }

    /// Seeded random paths, each stroked with three random styles.
    fn random_cases(b: &mut Builder, tol: f64) {
        let mut rng = XorShift::new(0x6b75_72626f);
        for k in 0..100 {
            let path = random_path(&mut rng, k % 6);
            let open = has_open_subpath(&path);
            let i = b.shape(&format!("rand{k:03}"), path);
            for _ in 0..3 {
                let style = random_style(&mut rng, open);
                b.add(i, &style, tol);
            }
        }
    }
}
