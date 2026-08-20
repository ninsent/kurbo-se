//! WASM surface for the kurbo-se sandbox: SVG path data in, SVG path data +
//! debug JSON out. Deliberately renderer-free (plain SVG on the frontend).

use kurbo_se::kurbo::{
    self, BezPath, Cap, Circle, CubicBez, Join, ParamCurve, PathEl, PathSeg, Rect, Shape, Vec2,
};
use kurbo_se::{
    DashStyle, StrokeAlignment, StrokeSide, StrokeStyle, analyze_subpaths, miter_angle_to_limit,
    stroke_aligned,
};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct StyleDto {
    width: f64,
    alignment: String,
    side: Option<String>,
    join: String,
    miter_angle: f64,
    start_cap: String,
    end_cap: String,
    dash: Option<DashDto>,
}

impl Default for StyleDto {
    fn default() -> Self {
        StyleDto {
            width: 8.0,
            alignment: "center".into(),
            side: None,
            join: "miter".into(),
            miter_angle: 28.96,
            start_cap: "none".into(),
            end_cap: "none".into(),
            dash: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct DashDto {
    pattern: Vec<f64>,
    offset: f64,
    cap: String,
}

impl Default for DashDto {
    fn default() -> Self {
        DashDto {
            pattern: vec![12.0, 6.0],
            offset: 0.0,
            cap: "none".into(),
        }
    }
}

fn parse_cap(s: &str) -> Cap {
    match s {
        "round" => Cap::Round,
        "square" => Cap::Square,
        _ => Cap::Butt, // "none"
    }
}

fn to_style(dto: &StyleDto) -> StrokeStyle {
    let mut style = StrokeStyle::new(dto.width)
        .with_alignment(match dto.alignment.as_str() {
            "inside" => StrokeAlignment::Inside,
            "outside" => StrokeAlignment::Outside,
            _ => StrokeAlignment::Center,
        })
        .with_join(match dto.join.as_str() {
            "bevel" => Join::Bevel,
            "round" => Join::Round,
            _ => Join::Miter,
        })
        .with_miter_angle(dto.miter_angle)
        .with_start_cap(parse_cap(&dto.start_cap))
        .with_end_cap(parse_cap(&dto.end_cap));
    if let Some(side) = dto.side.as_deref() {
        style = style.with_side(match side {
            "left" => StrokeSide::Left,
            "right" => StrokeSide::Right,
            _ => StrokeSide::Center,
        });
    }
    if let Some(d) = &dto.dash {
        style = style.with_dash(
            DashStyle::from_pattern(d.pattern.iter().copied())
                .with_offset(d.offset)
                .with_cap(parse_cap(&d.cap)),
        );
    }
    style
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DebugOut {
    output_d: String,
    subpaths: Vec<SubpathDto>,
    joins: Vec<JoinDto>,
    input_segs: usize,
    output_segs: usize,
    miter_limit: f64,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SubpathDto {
    closed: bool,
    zero_length: bool,
    area: f64,
    side: String,
    start: (f64, f64),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JoinDto {
    x: f64,
    y: f64,
    /// Interior corner angle, degrees.
    angle: f64,
    /// Whether a miter join falls back to bevel here.
    fell_back: bool,
}

fn parse_inputs(path_d: &str, style_json: &str) -> Result<(BezPath, StrokeStyle), String> {
    let path = BezPath::from_svg(path_d).map_err(|e| format!("path: {e}"))?;
    let dto: StyleDto =
        serde_json::from_str(style_json).map_err(|e| format!("style: {e}"))?;
    Ok((path, to_style(&dto)))
}

/// Expand a stroke; returns SVG path data (empty string on error).
#[wasm_bindgen]
pub fn expand_stroke(path_d: &str, style_json: &str, tolerance: f64) -> String {
    match parse_inputs(path_d, style_json) {
        Ok((path, style)) => stroke_aligned(&path, &style, tolerance).to_svg(),
        Err(_) => String::new(),
    }
}

/// Expand a stroke with debug info; returns a JSON object (see `DebugOut`).
#[wasm_bindgen]
pub fn expand_stroke_debug(path_d: &str, style_json: &str, tolerance: f64) -> String {
    let (path, style) = match parse_inputs(path_d, style_json) {
        Ok(v) => v,
        Err(e) => {
            return serde_json::to_string(&DebugOut {
                output_d: String::new(),
                subpaths: vec![],
                joins: vec![],
                input_segs: 0,
                output_segs: 0,
                miter_limit: 0.0,
                error: Some(e),
            })
            .unwrap();
        }
    };
    let out = stroke_aligned(&path, &style, tolerance);
    let subpaths = analyze_subpaths(&path, &style, tolerance)
        .into_iter()
        .map(|i| SubpathDto {
            closed: i.closed,
            zero_length: i.zero_length,
            area: i.area,
            side: match i.side {
                StrokeSide::Left => "left",
                StrokeSide::Right => "right",
                StrokeSide::Center => "center",
            }
            .into(),
            start: (i.start.x, i.start.y),
        })
        .collect();
    let joins = corner_joins(&path, style.miter_angle);
    let debug = DebugOut {
        output_d: out.to_svg(),
        subpaths,
        joins,
        input_segs: path.segments().count(),
        output_segs: out.segments().count(),
        miter_limit: miter_angle_to_limit(style.miter_angle),
        error: None,
    };
    serde_json::to_string(&debug).unwrap()
}

/// Corner markers: interior angle at each vertex of the source path, and
/// whether a miter join would fall back to bevel there (angle ≤ threshold).
fn corner_joins(path: &BezPath, miter_angle: f64) -> Vec<JoinDto> {
    fn seg_tangents(seg: &PathSeg) -> (Vec2, Vec2) {
        // Chord-based fallbacks, same spirit as kurbo's robust tangents.
        const EPS: f64 = 1e-12;
        match seg {
            PathSeg::Line(l) => (l.p1 - l.p0, l.p1 - l.p0),
            PathSeg::Quad(q) => {
                let d0 = if (q.p1 - q.p0).hypot2() > EPS {
                    q.p1 - q.p0
                } else {
                    q.p2 - q.p0
                };
                let d1 = if (q.p2 - q.p1).hypot2() > EPS {
                    q.p2 - q.p1
                } else {
                    q.p2 - q.p0
                };
                (d0, d1)
            }
            PathSeg::Cubic(c) => {
                let d0 = if (c.p1 - c.p0).hypot2() > EPS {
                    c.p1 - c.p0
                } else if (c.p2 - c.p0).hypot2() > EPS {
                    c.p2 - c.p0
                } else {
                    c.p3 - c.p0
                };
                let d1 = if (c.p3 - c.p2).hypot2() > EPS {
                    c.p3 - c.p2
                } else if (c.p3 - c.p1).hypot2() > EPS {
                    c.p3 - c.p1
                } else {
                    c.p3 - c.p0
                };
                (d0, d1)
            }
        }
    }

    let mut joins = Vec::new();
    let els = path.elements();
    let mut i = 0;
    while i < els.len() {
        let mut j = i + 1;
        while j < els.len() && !matches!(els[j], PathEl::MoveTo(_)) {
            j += 1;
        }
        let sub = &els[i..j];
        let mut segs: Vec<PathSeg> = kurbo::segments(sub.iter().copied()).collect();
        // Zero-length segments carry no direction; drop them so corners
        // across them are still detected.
        segs.retain(|s| {
            let (t0, t1) = seg_tangents(s);
            t0.hypot2() > 1e-24 || t1.hypot2() > 1e-24
        });
        let closed = matches!(sub.last(), Some(PathEl::ClosePath));
        if segs.len() >= 2 || (closed && !segs.is_empty()) {
            let n = segs.len();
            let corners = if closed { n } else { n - 1 };
            for k in 0..corners {
                let (_, tan_in) = seg_tangents(&segs[k]);
                let (tan_out, _) = seg_tangents(&segs[(k + 1) % n]);
                let cross = tan_in.cross(tan_out);
                let dot = tan_in.dot(tan_out);
                let turn = cross.abs().atan2(dot);
                if turn < 1e-3 {
                    continue; // straight-ish, no visible join
                }
                let interior = 180.0 - turn.to_degrees();
                let pt = segs[k].end();
                joins.push(JoinDto {
                    x: pt.x,
                    y: pt.y,
                    angle: interior,
                    fell_back: interior <= miter_angle,
                });
            }
        }
        i = j;
    }
    joins
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GalleryEntry {
    name: String,
    d: String,
    bbox: (f64, f64, f64, f64),
}

fn entry(name: &str, path: BezPath) -> GalleryEntry {
    let bb = path.bounding_box();
    GalleryEntry {
        name: name.into(),
        d: path.to_svg(),
        bbox: (bb.x0, bb.y0, bb.x1, bb.y1),
    }
}

/// The mission gallery, built from kurbo primitives (guaranteed-valid data).
#[wasm_bindgen]
pub fn gallery() -> String {
    let mut items = Vec::new();

    let mut p = BezPath::new();
    p.move_to((20.0, 160.0));
    p.line_to((70.0, 40.0));
    p.line_to((120.0, 160.0));
    p.line_to((170.0, 40.0));
    p.line_to((220.0, 160.0));
    items.push(entry("Open polyline", p));

    let mut p = BezPath::new();
    p.move_to((20.0, 150.0));
    p.curve_to((200.0, 20.0), (0.0, 20.0), (180.0, 150.0));
    items.push(entry("Open curve (loop/cusp)", p));

    items.push(entry(
        "Circle",
        Circle::new((120.0, 120.0), 80.0).to_path(1e-6),
    ));

    items.push(entry(
        "Rectangle",
        Rect::new(40.0, 60.0, 240.0, 180.0).to_path(1e-9),
    ));

    let mut p = BezPath::new();
    for i in 0..10 {
        let r = if i % 2 == 0 { 100.0 } else { 40.0 };
        let a = core::f64::consts::PI * (i as f64) / 5.0 - core::f64::consts::FRAC_PI_2;
        let pt = (150.0 + r * a.cos(), 150.0 + r * a.sin());
        if i == 0 {
            p.move_to(pt);
        } else {
            p.line_to(pt);
        }
    }
    p.close_path();
    items.push(entry("Star (concave corners)", p));

    let mut p = Circle::new((130.0, 130.0), 100.0).to_path(1e-6);
    p.extend(
        Circle::new((130.0, 130.0), 45.0)
            .to_path(1e-6)
            .reverse_subpaths()
            .iter(),
    );
    items.push(entry("Donut (hole)", p));

    let mut p = BezPath::new();
    p.move_to((20.0, 20.0));
    p.line_to((220.0, 180.0));
    p.line_to((220.0, 20.0));
    p.line_to((20.0, 180.0));
    p.close_path();
    items.push(entry("Self-intersecting (bowtie)", p));

    let mut p = BezPath::new();
    p.move_to((120.0, 100.0));
    p.curve_to((150.0, 55.0), (210.0, 55.0), (240.0, 100.0));
    p.curve_to((210.0, 145.0), (150.0, 145.0), (120.0, 100.0));
    p.curve_to((90.0, 55.0), (30.0, 55.0), (0.0, 100.0));
    p.curve_to((30.0, 145.0), (90.0, 145.0), (120.0, 100.0));
    p.close_path();
    items.push(entry("Figure-eight (known limit)", p));

    let mut p = BezPath::new();
    p.move_to((20.0, 100.0));
    p.line_to((20.0, 100.0));
    p.line_to((100.0, 40.0));
    p.line_to((100.0, 40.0));
    p.quad_to((100.0, 40.0), (100.0, 40.0));
    p.line_to((180.0, 100.0));
    p.curve_to((180.0, 100.0), (180.0, 100.0), (180.0, 100.0));
    p.line_to((100.0, 160.0));
    items.push(entry("Zero-length segments", p));

    let mut p = BezPath::new();
    p.move_to((20.0, 60.0));
    p.curve_to((80.0, 60.0), (140.0, 60.0), (200.0, 60.0));
    p.move_to((20.0, 140.0));
    p.curve_to((240.0, 140.0), (-40.0, 140.0), (180.0, 140.0));
    items.push(entry("Collinear cubics (+cusps)", p));

    let mut p = BezPath::new();
    p.move_to((150.0, 150.0));
    for i in 1..=72 {
        let a = i as f64 * 0.35;
        let r = 4.0 + i as f64 * 1.9;
        p.line_to((150.0 + r * a.cos(), 150.0 + r * a.sin()));
    }
    items.push(entry("Spiral (polyline)", p));

    items.push(entry(
        "kurbo #344 adversarial cubic",
        CubicBez::new(
            (51.0, 0.0),
            (-0.0859375, 161.640625),
            (0.0, 164.0),
            (0.0, 164.0),
        )
        .into_path(1e-6),
    ));

    let mut p = BezPath::new();
    p.move_to((20.0, 40.0));
    p.line_to((120.0, 20.0));
    p.line_to((220.0, 40.0));
    p.extend(Rect::new(40.0, 80.0, 120.0, 140.0).to_path(1e-9).iter());
    p.extend(
        Circle::new((190.0, 110.0), 35.0)
            .to_path(1e-6)
            .reverse_subpaths()
            .iter(),
    );
    items.push(entry("Multi-subpath mixed", p));

    let mut p = BezPath::new();
    p.move_to((20.0, 180.0));
    p.line_to((230.0, 160.0));
    p.line_to((20.0, 140.0));
    p.close_path();
    items.push(entry("Sharp wedge (inner clip)", p));

    serde_json::to_string(&items).unwrap()
}
