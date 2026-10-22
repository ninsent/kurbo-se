// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Glyph outlines from a TrueType font, for the text cases.

use std::path::PathBuf;

use kurbo::{BezPath, Point};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, MetadataProvider};

/// Environment variable that overrides the font file.
pub const FONT_ENV: &str = "KURBO_SE_REGRESS_FONT";

/// The font to draw glyphs from. It is `$KURBO_SE_REGRESS_FONT` when set,
/// else the sandbox's bundled Inter. That one is found relative to this
/// package, as it sits in the repository at `tools/regress/`.
pub fn font_path() -> PathBuf {
    match std::env::var_os(FONT_ENV) {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../sandbox/wasm/fonts/Inter-Regular.ttf"),
    }
}

/// The outline of `ch` drawn at `size` units per em with its origin at
/// `origin`, in y-down coordinates. `None` when the font has no outline
/// for it.
pub fn glyph(font: &FontRef<'_>, ch: char, size: f32, origin: Point) -> Option<BezPath> {
    let id = font.charmap().map(ch)?;
    let outline = font.outline_glyphs().get(id)?;
    let mut pen = GlyphPen {
        path: BezPath::new(),
        origin,
    };
    let settings = DrawSettings::unhinted(Size::new(size), LocationRef::default());
    outline.draw(settings, &mut pen).ok()?;
    Some(pen.path)
}

/// Collects a glyph outline into a path. Font outlines are y-up around the
/// glyph origin, so y flips. That reverses every contour alike, so holes
/// keep their opposite winding, as in the sandbox.
struct GlyphPen {
    path: BezPath,
    origin: Point,
}

impl GlyphPen {
    fn pt(&self, x: f32, y: f32) -> Point {
        Point::new(self.origin.x + f64::from(x), self.origin.y - f64::from(y))
    }
}

impl OutlinePen for GlyphPen {
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
