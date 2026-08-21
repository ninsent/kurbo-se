// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stroke style types and the miter angle ↔ miter limit conversion.

use kurbo::{Cap, Dashes, Join, Stroke};

use crate::math;

/// Figma's default miter angle in degrees (≙ SVG's default miter limit of 4).
pub(crate) const DEFAULT_MITER_ANGLE: f64 = 28.96;

/// Where the stroke band sits relative to the path, in user-facing terms.
///
/// For closed subpaths, `Inside` and `Outside` refer to the filled region
/// of the whole path under nonzero winding. Holes stroke into the solid
/// ring, and reversing a subpath's winding direction does not change the
/// result.
///
/// For open subpaths the notion is geometrically undefined. kurbo-se maps
/// `Inside` to [`StrokeSide::Right`] and `Outside` to [`StrokeSide::Left`],
/// relative to the travel direction in Y-down coordinates. Use
/// [`StrokeStyle::side`] to control the side explicitly.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StrokeAlignment {
    /// Half the width on each side of the path (ordinary stroking).
    #[default]
    Center,
    /// The band hugs the inside of the filled region.
    Inside,
    /// The band hugs the outside of the filled region.
    Outside,
}

/// Geometric side selection relative to the path's travel direction.
///
/// This bypasses [`StrokeAlignment`]'s orientation detection entirely: the
/// same side is used for every subpath, open or closed.
///
/// In Y-down coordinates, `Right` is the clockwise (`turn_90`) side of the
/// tangent. A positive-area (clockwise) contour has its interior on the right.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StrokeSide {
    /// Half the width on each side.
    #[default]
    Center,
    /// The whole band on the left of the travel direction.
    Left,
    /// The whole band on the right of the travel direction.
    Right,
}

/// Dash configuration for [`StrokeStyle`].
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DashStyle {
    /// Alternating dash/gap lengths, Figma-style. Entries at even indices
    /// are dash lengths; entries at odd indices are gaps. Any length is
    /// allowed. An odd-length pattern behaves as if read twice, per SVG:
    /// `2, 7, 4` equals `2, 7, 4, 2, 7, 4`, so dashes and gaps never swap
    /// roles as the pattern repeats.
    ///
    /// Entries must be finite and `>= 0` with a positive sum. Patterns that
    /// violate this render solid; kurbo's raw `dash` would panic or hang on
    /// them.
    pub pattern: Dashes,
    /// Phase shift into the pattern, in path-length units. Any finite value;
    /// normalized like SVG's `stroke-dashoffset` (negative values wrap).
    pub offset: f64,
    /// Cap applied to the edges created by dashing.
    ///
    /// The true endpoints of an open path keep [`StrokeStyle::start_cap`]
    /// and [`StrokeStyle::end_cap`]; every dash-generated edge uses this
    /// cap. Zero-length dashes render as dots: a circle for `Round`, an
    /// oriented square for `Square`. `Butt` renders nothing for them.
    ///
    /// Known limitation: a pattern whose single dash swallows an entire
    /// closed contour (dash length ≥ perimeter, zero gap) expands as an
    /// unmasked ring. At widths beyond the local thickness it can cross the
    /// fill boundary where a genuinely dashed or solid stroke would not.
    /// Such a pattern means "not dashed"; drop `dash` instead to get the
    /// solid construction.
    pub cap: Cap,
}

impl DashStyle {
    /// A `dash`/`gap` pattern with zero offset and butt dash caps.
    pub fn new(dash: f64, gap: f64) -> Self {
        let mut pattern = Dashes::new();
        pattern.push(dash);
        pattern.push(gap);
        DashStyle {
            pattern,
            offset: 0.0,
            cap: Cap::Butt,
        }
    }

    /// A dash style from an arbitrary pattern of alternating dash/gap
    /// lengths (see [`DashStyle::pattern`] for the index roles).
    ///
    /// ```
    /// let d = kurbo_se::DashStyle::from_pattern([2.0, 4.0, 6.0, 8.0]);
    /// assert_eq!(d.pattern.as_slice(), &[2.0, 4.0, 6.0, 8.0]);
    /// ```
    pub fn from_pattern(pattern: impl IntoIterator<Item = f64>) -> Self {
        DashStyle {
            pattern: pattern.into_iter().collect(),
            offset: 0.0,
            cap: Cap::Butt,
        }
    }

    /// Builder method for setting the dash offset.
    #[must_use]
    pub fn with_offset(mut self, offset: f64) -> Self {
        self.offset = offset;
        self
    }

    /// Builder method for setting the dash cap.
    #[must_use]
    pub fn with_cap(mut self, cap: Cap) -> Self {
        self.cap = cap;
        self
    }
}

/// The visual style of an aligned stroke, mirroring Figma's stroke panel.
///
/// The defaults from [`StrokeStyle::new`] follow Figma's panel: `Miter`
/// join at a 28.96° miter angle, `Butt` caps, centered, solid. They
/// intentionally differ from [`kurbo::Stroke`]'s defaults, which use
/// `Round` joins and caps.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StrokeStyle {
    /// Total width of the stroke band.
    ///
    /// `0` yields an empty outline. Negative or non-finite widths are treated
    /// as empty as well (with a `debug_assert`), never absolute-valued.
    pub width: f64,
    /// Where the band sits relative to the path. Ignored when [`side`] is set.
    ///
    /// [`side`]: StrokeStyle::side
    pub alignment: StrokeAlignment,
    /// Explicit geometric side, bypassing alignment resolution when `Some`.
    pub side: Option<StrokeSide>,
    /// How segments are connected within a subpath.
    pub join: Join,
    /// Corner angle in degrees (`0..=180`) at or below which a miter join
    /// falls back to bevel. Converted internally with
    /// [`miter_angle_to_limit`]. Only meaningful when [`join`] is
    /// [`Join::Miter`]. The value is retained when the join is switched
    /// away and back, as in Figma.
    ///
    /// [`join`]: StrokeStyle::join
    pub miter_angle: f64,
    /// Cap for the start of each open subpath.
    pub start_cap: Cap,
    /// Cap for the end of each open subpath.
    pub end_cap: Cap,
    /// Dash configuration; `None` means solid.
    pub dash: Option<DashStyle>,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        Self::new(1.0)
    }
}

impl StrokeStyle {
    /// A centered, solid, miter-joined (28.96°), butt-capped style of the
    /// given width.
    pub const fn new(width: f64) -> Self {
        StrokeStyle {
            width,
            alignment: StrokeAlignment::Center,
            side: None,
            join: Join::Miter,
            miter_angle: DEFAULT_MITER_ANGLE,
            start_cap: Cap::Butt,
            end_cap: Cap::Butt,
            dash: None,
        }
    }

    /// Builder method for setting the alignment.
    #[must_use]
    pub const fn with_alignment(mut self, alignment: StrokeAlignment) -> Self {
        self.alignment = alignment;
        self
    }

    /// Builder method for forcing a geometric side (bypasses alignment).
    #[must_use]
    pub const fn with_side(mut self, side: StrokeSide) -> Self {
        self.side = Some(side);
        self
    }

    /// Builder method for setting the join style.
    #[must_use]
    pub const fn with_join(mut self, join: Join) -> Self {
        self.join = join;
        self
    }

    /// Builder method for setting the miter angle in degrees (`0..=180`).
    #[must_use]
    pub const fn with_miter_angle(mut self, degrees: f64) -> Self {
        self.miter_angle = degrees;
        self
    }

    /// Builder method for setting the start cap.
    #[must_use]
    pub const fn with_start_cap(mut self, cap: Cap) -> Self {
        self.start_cap = cap;
        self
    }

    /// Builder method for setting the end cap.
    #[must_use]
    pub const fn with_end_cap(mut self, cap: Cap) -> Self {
        self.end_cap = cap;
        self
    }

    /// Builder method for setting both start and end caps.
    #[must_use]
    pub const fn with_caps(mut self, cap: Cap) -> Self {
        self.start_cap = cap;
        self.end_cap = cap;
        self
    }

    /// Builder method for setting the dash configuration.
    #[must_use]
    pub fn with_dash(mut self, dash: DashStyle) -> Self {
        self.dash = Some(dash);
        self
    }

    /// The internal miter limit corresponding to [`miter_angle`].
    ///
    /// [`miter_angle`]: StrokeStyle::miter_angle
    pub fn miter_limit(&self) -> f64 {
        miter_angle_to_limit(self.miter_angle)
    }
}

/// Largest miter limit produced by [`miter_angle_to_limit`]; corresponds to
/// an angle threshold so small that every representable corner miters.
const MITER_LIMIT_CLAMP: f64 = 1e9;

/// Convert a corner angle in degrees to the equivalent miter limit.
///
/// The relation is exact: `limit = 1 / sin(angle / 2)`. Corners whose
/// interior angle is at or below `angle_deg` render as bevel. Sharper
/// corners keep the miter spike.
///
/// The input is clamped to `[0, 180]`. An angle of 0 would give an
/// infinite limit and is capped at `1e9`, where every representable corner
/// miters. An angle of 180 gives `1.0`, where everything bevels.
/// Non-finite input debug-asserts and falls back to the default angle
/// (28.96°, limit ≈ 4).
///
/// ```
/// let limit = kurbo_se::miter_angle_to_limit(28.96);
/// assert!((limit - 4.0).abs() < 4e-3);
/// ```
pub fn miter_angle_to_limit(angle_deg: f64) -> f64 {
    if !angle_deg.is_finite() {
        debug_assert!(false, "miter angle must be finite, got {angle_deg}");
        return miter_angle_to_limit(DEFAULT_MITER_ANGLE);
    }
    let half = angle_deg.clamp(0.0, 180.0).to_radians() * 0.5;
    let s = math::sin(half);
    if s <= 1.0 / MITER_LIMIT_CLAMP {
        MITER_LIMIT_CLAMP
    } else {
        1.0 / s
    }
}

/// Convert a miter limit to the equivalent corner angle in degrees.
///
/// Inverse of [`miter_angle_to_limit`]: `angle = 2·asin(1/limit)`. Limits
/// below `1` are treated as `1`, which yields 180° (everything bevels). An
/// infinite limit yields `0`. Any other non-finite input debug-asserts and
/// returns the default angle.
///
/// ```
/// let angle = kurbo_se::miter_limit_to_angle(16.0);
/// assert!((angle - 7.17).abs() < 2e-2);
/// ```
pub fn miter_limit_to_angle(limit: f64) -> f64 {
    if limit == f64::INFINITY {
        return 0.0;
    }
    if !limit.is_finite() {
        debug_assert!(false, "miter limit must be finite, got {limit}");
        return DEFAULT_MITER_ANGLE;
    }
    let l = limit.max(1.0);
    (2.0 * math::asin(1.0 / l)).to_degrees()
}

/// Centered interpretation of a [`StrokeStyle`] for kurbo-native stroking.
///
/// The escape hatch for callers that only need `Center` alignment and want
/// a renderer's built-in stroker. The conversion is lossy:
///
/// - `alignment` and `side` are ignored; kurbo strokes are always centered.
/// - `dash.cap` is dropped; kurbo caps dashes with the start/end caps.
/// - Everything else maps 1:1, with `miter_angle` converted through
///   [`miter_angle_to_limit`].
impl From<&StrokeStyle> for Stroke {
    fn from(s: &StrokeStyle) -> Stroke {
        let mut stroke = Stroke::new(s.width)
            .with_join(s.join)
            .with_miter_limit(miter_angle_to_limit(s.miter_angle))
            .with_start_cap(s.start_cap)
            .with_end_cap(s.end_cap);
        if let Some(dash) = &s.dash {
            stroke = stroke.with_dashes(dash.offset, dash.pattern.iter().copied());
        }
        stroke
    }
}

impl From<StrokeStyle> for Stroke {
    fn from(s: StrokeStyle) -> Stroke {
        (&s).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_rel(actual: f64, expected: f64, rel: f64) {
        let err = (actual - expected).abs() / expected.abs().max(1e-12);
        assert!(
            err <= rel,
            "expected {expected}, got {actual} (rel err {err:.2e})"
        );
    }

    /// The four known conversion pairs from the spec, both directions.
    #[test]
    fn miter_known_pairs() {
        for (angle, limit) in [(7.17, 16.0), (28.96, 4.0), (11.478, 10.0), (96.0, 1.345)] {
            assert_rel(miter_angle_to_limit(angle), limit, 1.5e-3);
            assert_rel(miter_limit_to_angle(limit), angle, 1.5e-3);
        }
    }

    #[test]
    fn miter_clamps() {
        assert_eq!(miter_angle_to_limit(180.0), 1.0);
        assert_eq!(miter_angle_to_limit(360.0), 1.0); // clamped to 180
        assert_eq!(miter_angle_to_limit(0.0), 1e9);
        assert_eq!(miter_angle_to_limit(-45.0), 1e9); // clamped to 0
        assert_eq!(miter_limit_to_angle(1.0), 180.0);
        assert_eq!(miter_limit_to_angle(0.5), 180.0); // clamped to 1
        assert_eq!(miter_limit_to_angle(f64::INFINITY), 0.0);
    }

    #[test]
    fn miter_round_trip() {
        for angle in [1.0, 7.17, 28.96, 45.0, 90.0, 135.0, 179.0] {
            assert_rel(
                miter_limit_to_angle(miter_angle_to_limit(angle)),
                angle,
                1e-9,
            );
        }
        for limit in [1.0, 1.345, 2.0, 4.0, 10.0, 16.0, 1000.0] {
            assert_rel(
                miter_angle_to_limit(miter_limit_to_angle(limit)),
                limit,
                1e-9,
            );
        }
    }

    #[test]
    fn stroke_conversion_maps_fields() {
        let style = StrokeStyle::new(6.0)
            .with_alignment(StrokeAlignment::Inside) // ignored by the conversion
            .with_join(Join::Round)
            .with_miter_angle(96.0)
            .with_start_cap(Cap::Round)
            .with_end_cap(Cap::Square)
            .with_dash(
                DashStyle::new(4.0, 2.0)
                    .with_offset(-3.0)
                    .with_cap(Cap::Round),
            );
        let k: Stroke = (&style).into();
        assert_eq!(k.width, 6.0);
        assert_eq!(k.join, Join::Round);
        assert_rel(k.miter_limit, 1.345, 1.5e-3);
        assert_eq!(k.start_cap, Cap::Round);
        assert_eq!(k.end_cap, Cap::Square);
        assert_eq!(k.dash_pattern.as_slice(), &[4.0, 2.0]);
        assert_eq!(k.dash_offset, -3.0);
    }

    #[test]
    fn defaults_follow_figma_panel() {
        let s = StrokeStyle::new(1.0);
        assert_eq!(s.alignment, StrokeAlignment::Center);
        assert_eq!(s.side, None);
        assert_eq!(s.join, Join::Miter);
        assert_rel(miter_angle_to_limit(s.miter_angle), 4.0, 4e-3);
        assert_eq!(s.start_cap, Cap::Butt);
        assert_eq!(s.end_cap, Cap::Butt);
        assert!(s.dash.is_none());
    }
}
