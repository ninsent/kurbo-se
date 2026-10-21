//! Wasm surface for the kurbo-se sandbox: SVG path data in, SVG path data
//! and debug JSON out. Renderer-free on purpose; the frontend draws plain
//! SVG.

use std::cell::RefCell;
use std::sync::Arc;

use kurbo_se::kurbo::{
    self, BezPath, Cap, Circle, Join, ParamCurve, PathEl, PathSeg, Rect, Shape, Vec2,
};
use kurbo_se::{
    DashStyle, StrokeAlignment, StrokeSide, StrokeStyle, analyze_subpaths, miter_angle_to_limit,
    stroke_aligned,
};
use parley::fontique::{Blob, Collection, CollectionOptions, SourceCache};
use parley::{FontContext, FontFamily, Layout, LayoutContext, PositionedLayoutItem, StyleProperty};
use serde::{Deserialize, Serialize};
use skrifa::instance::{LocationRef, NormalizedCoord, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};
use wasm_bindgen::prelude::*;

mod union;

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
    /// Interior corner angle in degrees.
    angle: f64,
    /// Whether a miter join falls back to bevel at this corner.
    fell_back: bool,
}

fn parse_inputs(path_d: &str, style_json: &str) -> Result<(BezPath, StrokeStyle), String> {
    let path = BezPath::from_svg(path_d).map_err(|e| format!("path: {e}"))?;
    let dto: StyleDto = serde_json::from_str(style_json).map_err(|e| format!("style: {e}"))?;
    Ok((path, to_style(&dto)))
}

/// Expand a stroke and return SVG path data. Empty string on error.
#[wasm_bindgen]
pub fn expand_stroke(path_d: &str, style_json: &str, tolerance: f64) -> String {
    match parse_inputs(path_d, style_json) {
        Ok((path, style)) => stroke_aligned(&path, &style, tolerance).to_svg(),
        Err(_) => String::new(),
    }
}

/// Expand a stroke with debug info, as a JSON object. See `DebugOut`.
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

/// Corner markers: the interior angle at each vertex of the source path,
/// and whether a miter join falls back to bevel there, which happens when
/// the angle is at or below the threshold.
fn corner_joins(path: &BezPath, miter_angle: f64) -> Vec<JoinDto> {
    fn seg_tangents(seg: &PathSeg) -> (Vec2, Vec2) {
        // Chord-based fallbacks, in the spirit of kurbo's robust tangents.
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
        // Zero-length segments carry no direction. Drop them so a corner
        // across one is still detected.
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

/// The gallery, built from kurbo primitives so the data is always valid.
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

    // Archimedean spiral, r = b·θ, as exact Hermite-fitted cubics.
    let mut p = BezPath::new();
    let b = 5.2_f64;
    let th0 = 1.1_f64;
    let th1 = 4.25 * core::f64::consts::TAU;
    let steps = 46_usize;
    let pos = |th: f64| kurbo::Point::new(150.0 + b * th * th.cos(), 150.0 + b * th * th.sin());
    let deriv = |th: f64| {
        Vec2::new(
            b * (th.cos() - th * th.sin()),
            b * (th.sin() + th * th.cos()),
        )
    };
    let h = (th1 - th0) / steps as f64;
    p.move_to(pos(th0));
    for i in 0..steps {
        let a = th0 + h * i as f64;
        let z = a + h;
        p.curve_to(
            pos(a) + deriv(a) * (h / 3.0),
            pos(z) - deriv(z) * (h / 3.0),
            pos(z),
        );
    }
    items.push(entry("Spiral (smooth)", p));

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

// ---------- text source ----------

/// Most characters the text source lays out, counted as Unicode scalar
/// values. The panel's input stops at the same number of UTF-16 units,
/// so it never goes past this limit. This is the backstop for URL params
/// and direct calls.
const TEXT_MAX_CHARS: usize = 32;

/// Crossing-search accuracy for the glyph union, as a fraction of the font
/// size.
const UNION_ACCURACY_EM: f64 = 1e-6;

/// Inter Regular, under the SIL Open Font License 1.1 in `fonts/OFL.txt`.
/// Wasm has no system fonts, so this is the only font parley can pick.
static INTER: &[u8] = include_bytes!("../fonts/Inter-Regular.ttf");

/// Parley's font database and layout scratch space, built once.
struct TextCx {
    font_cx: FontContext,
    layout_cx: LayoutContext<()>,
    family: String,
}

impl TextCx {
    fn new() -> Self {
        let mut font_cx = FontContext {
            collection: Collection::new(CollectionOptions {
                shared: false,
                system_fonts: false,
            }),
            source_cache: SourceCache::default(),
        };
        let families = font_cx
            .collection
            .register_fonts(Blob::new(Arc::new(INTER)), None);
        let family = families
            .first()
            .and_then(|(id, _)| font_cx.collection.family_name(*id))
            .unwrap_or("Inter")
            .to_owned();
        TextCx {
            font_cx,
            layout_cx: LayoutContext::new(),
            family,
        }
    }

    /// Lays `text` out on one line at `size` px and collects every glyph's
    /// outline into one path. The path is in y-down layout space, with the
    /// first line's top at y = 0.
    fn outline(&mut self, text: &str, size: f32) -> BezPath {
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, false);
        builder.push_default(StyleProperty::FontSize(size));
        builder.push_default(FontFamily::named(&self.family));
        let mut layout: Layout<()> = builder.build(text);
        layout.break_all_lines(None);

        let mut path = BezPath::new();
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                    continue;
                };
                let run = glyph_run.run();
                let font = run.font();
                let Ok(font_ref) = FontRef::from_index(font.data.as_ref(), font.index) else {
                    continue;
                };
                let outlines = font_ref.outline_glyphs();
                let coords: Vec<NormalizedCoord> = run
                    .normalized_coords()
                    .iter()
                    .map(|c| NormalizedCoord::from_bits(c.to_bits()))
                    .collect();
                for glyph in glyph_run.positioned_glyphs() {
                    let Some(outline) = outlines.get(GlyphId::new(glyph.id)) else {
                        continue;
                    };
                    let settings = DrawSettings::unhinted(
                        Size::new(run.font_size()),
                        LocationRef::new(&coords),
                    );
                    let mut pen = GlyphPen {
                        path: &mut path,
                        origin: (f64::from(glyph.x), f64::from(glyph.y)),
                    };
                    // A glyph that fails to draw is left out, like a
                    // missing one.
                    let _ = outline.draw(settings, &mut pen);
                }
            }
        }
        path
    }
}

thread_local! {
    static TEXT_CX: RefCell<TextCx> = RefCell::new(TextCx::new());
}

/// Appends one glyph's outline to a path. Font outlines are y-up around
/// the glyph origin, so y is flipped. The flip reverses every contour,
/// outer and hole alike, so holes still wind opposite to outer contours.
/// The nonzero fill and the crate's hole detection rely on that.
struct GlyphPen<'a> {
    path: &'a mut BezPath,
    origin: (f64, f64),
}

impl GlyphPen<'_> {
    fn pt(&self, x: f32, y: f32) -> kurbo::Point {
        kurbo::Point::new(self.origin.0 + f64::from(x), self.origin.1 - f64::from(y))
    }
}

impl OutlinePen for GlyphPen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.pt(x, y);
        self.path.move_to(p);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.pt(x, y);
        self.path.line_to(p);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (c, p) = (self.pt(cx0, cy0), self.pt(x, y));
        self.path.quad_to(c, p);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (c0, c1, p) = (self.pt(cx0, cy0), self.pt(cx1, cy1), self.pt(x, y));
        self.path.curve_to(c0, c1, p);
    }

    fn close(&mut self) {
        self.path.close_path();
    }
}

/// The text source's character limit. The panel's input uses the same
/// number.
#[wasm_bindgen]
pub fn text_max_chars() -> usize {
    TEXT_MAX_CHARS
}

/// Glyph outlines of `text`, laid out by parley at `size` px, as a gallery
/// entry JSON object `{name, d, bbox}`. With `union`, overlapping contours
/// merge into one outline that keeps the curves; see `union.rs`. Without
/// it, the contours are the font's own, overlaps and all. Only the first
/// `TEXT_MAX_CHARS` characters are used. Empty or all-whitespace text
/// gives an empty `d` and a zero bbox.
#[wasm_bindgen]
pub fn text_outline(text: &str, size: f64, union: bool) -> String {
    let text: String = text.chars().take(TEXT_MAX_CHARS).collect();
    let size = if size.is_finite() {
        size.clamp(1.0, 1000.0)
    } else {
        120.0
    };
    let mut path = TEXT_CX.with_borrow_mut(|cx| cx.outline(&text, size as f32));
    if union {
        path = union::union_nonzero(&path, size * UNION_ACCURACY_EM);
    }
    let out = if path.is_empty() {
        GalleryEntry {
            name: "Text".into(),
            d: String::new(),
            bbox: (0.0, 0.0, 0.0, 0.0),
        }
    } else {
        entry("Text", path)
    };
    serde_json::to_string(&out).unwrap()
}
