// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Top-level orchestration: input gating, per-subpath side resolution,
//! solid/dashed dispatch, and the allocation-reusing context.

use alloc::vec::Vec;

use kurbo::{BezPath, Cap, PathEl, Shape};

use crate::dash;
use crate::expand::{Band, BandParams};
use crate::normalize;
use crate::orient;
use crate::style::{StrokeAlignment, StrokeSide, StrokeStyle};

/// Reusable buffers for repeated stroke expansion (mirrors kurbo's
/// [`StrokeCtx`](kurbo::StrokeCtx) pattern, which was made public exactly for
/// this purpose).
///
/// Reusing one context across calls keeps the hot path allocation-free once
/// the buffers have grown to a working size — the intended usage for
/// animated `dash_offset` (re-expansion per frame is the supported model; see
/// the crate docs for measured costs).
#[derive(Default, Debug)]
pub struct AlignedStrokeCtx {
    input: BezPath,
    left: BezPath,
    right: BezPath,
    scratch: BezPath,
    sink: BezPath,
    dash_group: Vec<PathEl>,
    output: BezPath,
}

impl AlignedStrokeCtx {
    /// A fresh context.
    pub fn new() -> Self {
        Self::default()
    }

    /// The most recently produced fill outline.
    pub fn output(&self) -> &BezPath {
        &self.output
    }
}

/// Expand an aligned stroke into a fill outline.
///
/// The result must be interpreted with the **nonzero winding rule**; see the
/// crate docs for the geometric conventions and alignment semantics.
///
/// `tolerance` controls the accuracy of curve offsets, joins, and caps, and
/// is also passed to `path`'s [`Shape::path_elements`] conversion. For
/// display purposes `0.25` works well; use smaller values if the result will
/// be scaled up.
///
/// Never panics on degenerate input: zero width, empty paths, and non-finite
/// coordinates yield an empty path (non-finite *parameters* additionally
/// `debug_assert`).
pub fn stroke_aligned(path: impl Shape, style: &StrokeStyle, tolerance: f64) -> BezPath {
    let mut ctx = AlignedStrokeCtx::default();
    stroke_aligned_with(path, style, tolerance, &mut ctx);
    core::mem::take(&mut ctx.output)
}

/// Allocation-reusing variant of [`stroke_aligned`].
///
/// The returned reference borrows from `ctx`; the result also stays available
/// via [`AlignedStrokeCtx::output`] until the next call.
pub fn stroke_aligned_with<'a>(
    path: impl Shape,
    style: &StrokeStyle,
    tolerance: f64,
    ctx: &'a mut AlignedStrokeCtx,
) -> &'a BezPath {
    ctx.output.truncate(0);

    // Parameter gates: width == 0 is a valid empty result; negative
    // or non-finite width is a caller bug (debug_assert) but still empty.
    if style.width <= 0.0 || !style.width.is_finite() {
        debug_assert!(
            style.width == 0.0,
            "stroke width must be finite and >= 0, got {}",
            style.width
        );
        return &ctx.output;
    }
    let tolerance = if tolerance.is_finite() {
        tolerance.max(1e-9)
    } else {
        debug_assert!(false, "tolerance must be finite, got {tolerance}");
        1e-3
    };

    normalize::collect_canonical(path.path_elements(tolerance), &mut ctx.input);
    // Input gate: non-finite geometry would propagate NaN into the output.
    if !ctx.input.is_finite() || ctx.input.elements().is_empty() {
        return &ctx.output;
    }

    let probe_scale = ctx.input.control_box().size().max_side();
    let miter_limit = style.miter_limit();
    let width = style.width;
    let sanitized_dash = style.dash.as_ref().and_then(dash::sanitize);
    // Cut positions only need to be accurate to within rendering tolerance;
    // asking for far more drives the subdivision search to full depth.
    let split_accuracy = tolerance.clamp(1e-4, 0.25);
    // Centered bands don't flip sides across self-intersections (overlaps
    // are winding-additive, kurbo semantics) — skip the splitting machinery.
    let needs_split = !matches!(
        (style.side, style.alignment),
        (Some(StrokeSide::Center), _) | (None, StrokeAlignment::Center)
    );

    let AlignedStrokeCtx {
        input,
        left,
        right,
        scratch,
        sink,
        dash_group,
        output,
    } = ctx;

    // ---- pass 1: decompose into pieces ----------------------------------
    // A piece is a whole simple subpath, or — when a subpath self-intersects
    // — one lobe/strand of it (bug-for-bug Figma parity: each lobe resolves
    // its side against the fill, and independent contours add winding
    // instead of cancelling; see `split`).
    struct Piece {
        els: BezPath,
        closed: bool,
        /// True endpoints (open, unsplit or boundary pieces): for cap
        /// assignment under dashing.
        true_start: Option<kurbo::Point>,
        true_end: Option<kurbo::Point>,
        /// Dash units with phase offsets; `None` = dash `els` directly.
        arcs: Option<Vec<crate::split::SplitArc>>,
        /// Crossing vertices of the source subpath (join suppression).
        crossings: alloc::rc::Rc<[kurbo::Point]>,
        side: StrokeSide,
        fill_side: Option<StrokeSide>,
        eff_width: f64,
    }
    let no_crossings: alloc::rc::Rc<[kurbo::Point]> = alloc::rc::Rc::from(&[][..]);
    let mut pieces: Vec<Piece> = Vec::new();

    for subpath in normalize::subpaths(input.elements()) {
        let closed = normalize::is_closed(subpath);

        if normalize::is_zero_length(subpath) {
            // Zero-length subpath: a dot only for Center alignment with Round
            // caps on both ends (there is no tangent, hence no side and no
            // orientation for squares). Documented in the crate docs.
            let center = matches!(
                (style.side, style.alignment),
                (Some(StrokeSide::Center), _) | (None, StrokeAlignment::Center)
            );
            if center && style.start_cap == Cap::Round && style.end_cap == Cap::Round {
                let pt = normalize::start_point(subpath);
                output.extend(kurbo::Circle::new(pt, 0.5 * width).path_elements(tolerance));
            }
            continue;
        }

        let true_start = (!closed).then(|| normalize::start_point(subpath));
        let true_end = (!closed).then(|| normalize::end_point(subpath));
        let split = if needs_split {
            crate::split::split_self_intersections(subpath, closed, split_accuracy)
        } else {
            None
        };
        match split {
            Some(mut res) => {
                // Distribute arcs to their pieces for dashing.
                let mut arcs_by_piece: Vec<Vec<crate::split::SplitArc>> =
                    (0..res.pieces.len()).map(|_| Vec::new()).collect();
                for arc in res.arcs.drain(..) {
                    if arc.piece_ix < arcs_by_piece.len() {
                        arcs_by_piece[arc.piece_ix].push(arc);
                    }
                }
                let crossings: alloc::rc::Rc<[kurbo::Point]> =
                    alloc::rc::Rc::from(res.crossing_points.as_slice());
                for (piece, arcs) in res.pieces.into_iter().zip(arcs_by_piece) {
                    pieces.push(Piece {
                        closed: piece.closed,
                        els: piece.els,
                        true_start,
                        true_end,
                        arcs: Some(arcs),
                        crossings: crossings.clone(),
                        side: StrokeSide::Center,
                        fill_side: None,
                        eff_width: width,
                    });
                }
            }
            None => pieces.push(Piece {
                els: BezPath::from_vec(subpath.to_vec()),
                closed,
                true_start,
                true_end,
                arcs: None,
                crossings: no_crossings.clone(),
                side: StrokeSide::Center,
                fill_side: None,
                eff_width: width,
            }),
        }
    }

    // ---- pass 2: resolve sides and clamp effective widths ---------------
    for piece in &mut pieces {
        let resolved = orient::resolve(
            style.alignment,
            style.side,
            piece.els.elements(),
            piece.closed,
            input,
            probe_scale,
        );
        piece.side = resolved.side;
        piece.fill_side = resolved.fill_side;
    }
    // ---- pass 3: expand --------------------------------------------------
    // Closed contours with a one-sided band are built set-theoretically:
    // `Inside(w) = F \ E`, `Outside(w) = D \ F` (Figma's model), where E and
    // D are the erosion and dilation of the fill by w. So the emission is the
    // source contours plus the eroded/dilated boundary wound the other way.
    // Past the local thickness the erosion vanishes and the stroke saturates
    // — it fills the shape — instead of folding over itself. See `region`.
    //
    // The winding arithmetic needs the fill expressed with winding +1, so the
    // contours are normalised: outer-like boundaries clockwise, holes
    // counter-clockwise (the filled set is unchanged).
    let region_ix: Vec<usize> = pieces
        .iter()
        .enumerate()
        .filter(|(_, p)| p.closed && p.side != StrokeSide::Center && p.eff_width > 0.0)
        .map(|(i, _)| i)
        .collect();
    // The set formulation needs every one-sided closed band to face the fill
    // the same way (always true for Inside/Outside; a raw `side` override on
    // a compound path can disagree, and then each contour keeps the direct
    // band construction).
    let uniform_fill_side = region_ix
        .iter()
        .filter_map(|&i| pieces[i].fill_side.map(|f| f == pieces[i].side))
        .collect::<Vec<_>>();
    let region_mode = sanitized_dash.is_none()
        && !region_ix.is_empty()
        && uniform_fill_side.len() == region_ix.len()
        && uniform_fill_side.iter().all(|v| *v == uniform_fill_side[0]);

    let mut region_done = alloc::vec![false; pieces.len()];
    if region_mode {
        let into_fill = uniform_fill_side[0];
        let width_eff = pieces[region_ix[0]].eff_width;
        let normalized: Vec<BezPath> = region_ix
            .iter()
            .map(|&i| normalize_orientation(&pieces[i].els, pieces[i].fill_side.unwrap()))
            .collect();
        let specs: Vec<crate::region::ContourSpec<'_>> = normalized
            .iter()
            .map(|els| crate::region::ContourSpec {
                els: els.elements(),
                // Normalisation put the fill to the right of travel for every
                // contour, outer boundaries and holes alike.
                fill_side: StrokeSide::Right,
            })
            .collect();
        // Flatten the source once for the distance predicate, finely enough
        // not to eat into the pruning slack.
        let index = crate::region::SourceIndex::new(input, (width_eff * 1e-3).max(tolerance * 0.1));
        let base = BandParams {
            d_left: 0.0,
            d_right: 0.0,
            join: style.join,
            miter_limit,
            start_cap: style.start_cap,
            end_cap: style.end_cap,
            tolerance,
            join_thresh: 2.0 * tolerance / width_eff,
            crossings: &[],
            crossing_eps: 0.0,
            raw_offset: true,
        };
        let kind = if into_fill {
            crate::region::RegionKind::Erosion
        } else {
            crate::region::RegionKind::Dilation
        };
        let loops = crate::region::region_loops(
            &specs,
            kind,
            &index,
            width_eff,
            &base,
            split_accuracy,
            left,
            right,
            scratch,
            sink,
        );
        if into_fill {
            // F minus the erosion.
            for els in &normalized {
                output.extend(els.iter());
            }
            for l in &loops {
                output.extend(l.reverse_subpaths().iter());
            }
        } else if !loops.is_empty() {
            // The dilation minus F. Without a dilation boundary there is no
            // region at all — emitting bare reversed contours would paint the
            // shape itself.
            for l in &loops {
                output.extend(l.iter());
            }
            for els in &normalized {
                output.extend(els.reverse_subpaths().iter());
            }
        }
        for &i in &region_ix {
            region_done[i] = true;
        }
    }

    for (ix, piece) in pieces.iter().enumerate() {
        if piece.eff_width <= 0.0 || region_done[ix] {
            continue;
        }
        let (d_left, d_right) = orient::side_distances(piece.side, piece.eff_width);
        let params = BandParams {
            d_left,
            d_right,
            join: style.join,
            miter_limit,
            start_cap: style.start_cap,
            end_cap: style.end_cap,
            tolerance,
            join_thresh: 2.0 * tolerance / piece.eff_width,
            crossings: &piece.crossings,
            crossing_eps: split_accuracy * 8.0,
            raw_offset: false,
        };

        match (&sanitized_dash, &piece.arcs) {
            (Some(d), None) => dash::expand_dashed_subpath(
                piece.els.elements(),
                piece.closed,
                d,
                params,
                0.0,
                piece.true_start,
                piece.true_end,
                dash_group,
                left,
                right,
                scratch,
                output,
            ),
            (Some(d), Some(arcs)) => {
                // Dash split pieces arc by arc so the pattern phase stays
                // continuous along the original path; dash caps appear at
                // crossings (the original endpoints keep their real caps).
                for arc in arcs {
                    dash::expand_dashed_subpath(
                        arc.els.elements(),
                        false,
                        d,
                        params,
                        arc.s0,
                        piece.true_start,
                        piece.true_end,
                        dash_group,
                        left,
                        right,
                        scratch,
                        output,
                    );
                }
            }
            (None, _) => {
                let mut band = Band::new(params, left, right, scratch, output);
                band.run(piece.els.iter());
            }
        }
    }

    &ctx.output
}

/// Re-wind a closed contour so that the filled side lies to the right of
/// travel — i.e. clockwise for outer-like boundaries, counter-clockwise for
/// holes (Y-down). The filled set is unchanged; only the winding numbers are
/// normalised, which is what the region arithmetic needs.
fn normalize_orientation(els: &BezPath, fill_side: StrokeSide) -> BezPath {
    if fill_side == StrokeSide::Left {
        els.reverse_subpaths()
    } else {
        els.clone()
    }
}

/// How one subpath was interpreted by [`stroke_aligned`].
///
/// Returned by [`analyze_subpaths`]; useful for debugging tools and editors
/// that want to display the resolved side, orientation, or degeneracy of
/// each subpath without re-deriving the crate's conventions.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SubpathInfo {
    /// Whether the subpath is closed (contains a `ClosePath`).
    pub closed: bool,
    /// Whether the subpath is degenerate (no band geometry; at most a dot).
    pub zero_length: bool,
    /// Signed area (positive = clockwise in Y-down); `0.0` for open subpaths.
    pub area: f64,
    /// The geometric side the band was placed on.
    pub side: StrokeSide,
    /// The subpath's start point.
    pub start: kurbo::Point,
}

/// Report how each subpath of `path` resolves under `style`.
///
/// Runs the same canonicalization and side-resolution logic as
/// [`stroke_aligned`] (winding probe included) without expanding anything.
pub fn analyze_subpaths(
    path: impl Shape,
    style: &StrokeStyle,
    tolerance: f64,
) -> alloc::vec::Vec<SubpathInfo> {
    let tolerance = if tolerance.is_finite() {
        tolerance.max(1e-9)
    } else {
        1e-3
    };
    let mut input = BezPath::new();
    normalize::collect_canonical(path.path_elements(tolerance), &mut input);
    let mut infos = alloc::vec::Vec::new();
    if !input.is_finite() {
        return infos;
    }
    let probe_scale = input.control_box().size().max_side();
    for subpath in normalize::subpaths(input.elements()) {
        let closed = normalize::is_closed(subpath);
        let zero_length = normalize::is_zero_length(subpath);
        let area = if closed { subpath.area() } else { 0.0 };
        let side = orient::resolve_side(
            style.alignment,
            style.side,
            subpath,
            closed,
            &input,
            probe_scale,
        );
        infos.push(SubpathInfo {
            closed,
            zero_length,
            area,
            side,
            start: normalize::start_point(subpath),
        });
    }
    infos
}

/// Blanket convenience so any [`Shape`] can be stroked directly:
/// `shape.stroke_aligned(&style, tolerance)`.
pub trait StrokeAlignExt: Shape {
    /// Expand this shape's aligned stroke into a fill outline
    /// (see [`stroke_aligned`]).
    fn stroke_aligned(&self, style: &StrokeStyle, tolerance: f64) -> BezPath
    where
        Self: Sized,
    {
        stroke_aligned(self, style, tolerance)
    }
}

impl<T: Shape> StrokeAlignExt for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{DashStyle, StrokeAlignment};
    use core::f64::consts::PI;
    use kurbo::{Circle, Join, Rect, Stroke, StrokeOpts, stroke};

    const TOL: f64 = 1e-4;

    fn circle() -> Circle {
        Circle::new((100.0, 100.0), 50.0)
    }

    /// Analytic ring areas for a circle: the strongest cheap oracle we have
    /// (no corners ⇒ no winding>1 pockets ⇒ raw signed area is meaningful).
    #[test]
    fn circle_ring_areas_analytic() {
        let c = circle();
        let (r, w) = (50.0, 10.0);
        for (alignment, expected) in [
            (StrokeAlignment::Inside, PI * (r * r - (r - w) * (r - w))),
            (StrokeAlignment::Outside, PI * ((r + w) * (r + w) - r * r)),
            (
                StrokeAlignment::Center,
                PI * ((r + w / 2.0) * (r + w / 2.0) - (r - w / 2.0) * (r - w / 2.0)),
            ),
        ] {
            let style = StrokeStyle::new(w).with_alignment(alignment);
            let out = stroke_aligned(c, &style, TOL);
            let area = out.area().abs();
            assert!(
                (area - expected).abs() < 2.0,
                "{alignment:?}: area {area:.3} vs analytic {expected:.3}"
            );
        }
    }

    /// Inside stroke never leaves the source circle; outside never enters.
    #[test]
    fn circle_containment_by_bbox() {
        let c = circle();
        let style = StrokeStyle::new(10.0).with_alignment(StrokeAlignment::Inside);
        let out = stroke_aligned(c, &style, TOL);
        let bb = out.bounding_box();
        let cb = c.bounding_box();
        assert!(
            cb.contains_rect(bb),
            "inside stroke bbox {bb:?} escapes source {cb:?}"
        );

        let style = StrokeStyle::new(10.0).with_alignment(StrokeAlignment::Outside);
        let out = stroke_aligned(c, &style, TOL);
        let bb = out.bounding_box();
        // The outside ring's inner boundary is the circle; its bbox is the
        // outset bbox.
        let expected = cb.inflate(10.0, 10.0);
        assert!((bb.x0 - expected.x0).abs() < 0.01 && (bb.x1 - expected.x1).abs() < 0.01);
    }

    /// `DoD` #5: reversing winding must not change Inside/Outside output.
    #[test]
    fn reversal_invariance() {
        let path = circle().to_path(1e-9);
        let reversed = path.reverse_subpaths();
        for alignment in [StrokeAlignment::Inside, StrokeAlignment::Outside] {
            let style = StrokeStyle::new(10.0).with_alignment(alignment);
            let a = stroke_aligned(&path, &style, TOL);
            let b = stroke_aligned(&reversed, &style, TOL);
            assert!(
                (a.area().abs() - b.area().abs()).abs() < 1e-6,
                "{alignment:?}: area changed under reversal"
            );
            let (ba, bb) = (a.bounding_box(), b.bounding_box());
            assert!(
                (ba.x0 - bb.x0).abs() < 1e-6
                    && (ba.y0 - bb.y0).abs() < 1e-6
                    && (ba.x1 - bb.x1).abs() < 1e-6
                    && (ba.y1 - bb.y1).abs() < 1e-6
            );
        }
    }

    /// Center + solid agrees with `kurbo::stroke` by area on curves and lines.
    #[test]
    fn center_parity_with_kurbo() {
        let c = circle().to_path(1e-9);
        let style = StrokeStyle::new(8.0)
            .with_join(Join::Round)
            .with_caps(Cap::Round);
        let ours = stroke_aligned(&c, &style, TOL);
        let kurbo_stroke = Stroke::from(&style);
        let theirs = stroke(c.iter(), &kurbo_stroke, &StrokeOpts::default(), TOL);
        assert!(
            (ours.area().abs() - theirs.area().abs()).abs() < 1.0,
            "ours {} vs kurbo {}",
            ours.area().abs(),
            theirs.area().abs()
        );
    }

    /// Rect inside stroke stays inside; hole of a donut strokes into the ring.
    #[test]
    fn rect_and_donut_inside() {
        let rect = Rect::new(0.0, 0.0, 100.0, 60.0);
        let style = StrokeStyle::new(8.0).with_alignment(StrokeAlignment::Inside);
        let out = stroke_aligned(rect, &style, 0.1);
        assert!(!out.is_empty());
        assert!(rect.bounding_box().contains_rect(out.bounding_box()));

        // Donut: outer 100×60, hole 40×20 in the middle, opposite winding.
        let mut donut = rect.to_path(1e-9);
        let hole = Rect::new(30.0, 20.0, 70.0, 40.0)
            .to_path(1e-9)
            .reverse_subpaths();
        donut.extend(hole.iter());
        let out = stroke_aligned(&donut, &style, 0.1);
        // The hole's inside stroke goes outward from the hole: the outline
        // must extend beyond the hole rect but stay within the outer rect.
        let bb = out.bounding_box();
        assert!(rect.bounding_box().contains_rect(bb));
        // Points just outside the hole (in the ring, within 8px) are covered.
        for probe in [(25.0, 30.0), (75.0, 30.0), (50.0, 15.0), (50.0, 45.0)] {
            assert!(
                out.winding(probe.into()) != 0,
                "ring point {probe:?} not covered by the hole's inside stroke"
            );
        }
        // The hole interior is untouched.
        assert_eq!(out.winding((50.0, 30.0).into()), 0);
    }

    /// Degenerate inputs: no panics, empty or finite output.
    #[test]
    fn degenerate_inputs_no_panic() {
        let style = StrokeStyle::new(4.0);
        // Zero width.
        let z = StrokeStyle::new(0.0);
        assert!(stroke_aligned(circle(), &z, TOL).is_empty());
        // Empty path.
        assert!(stroke_aligned(BezPath::new(), &style, TOL).is_empty());
        // Non-finite coordinates.
        let mut bad = BezPath::new();
        bad.move_to((0.0, 0.0));
        bad.line_to((f64::NAN, 5.0));
        assert!(stroke_aligned(&bad, &style, TOL).is_empty());
        // Single point subpath, butt caps: nothing.
        let mut dot = BezPath::new();
        dot.move_to((5.0, 5.0));
        dot.line_to((5.0, 5.0));
        assert!(stroke_aligned(&dot, &style, TOL).is_empty());
        // Same, center + round caps: a dot.
        let dotted = StrokeStyle::new(4.0).with_caps(Cap::Round);
        let out = stroke_aligned(&dot, &dotted, TOL);
        assert!(
            (out.area().abs() - PI * 4.0).abs() < 0.01,
            "area {}",
            out.area()
        );
        // But not for one-sided alignment (no tangent ⇒ no side).
        let sided = dotted.clone().with_alignment(StrokeAlignment::Inside);
        assert!(stroke_aligned(&dot, &sided, TOL).is_empty());
    }

    /// Dashed inside stroke on a rect: stays inside, dashes are separate.
    #[test]
    fn dashed_inside_rect() {
        let rect = Rect::new(0.0, 0.0, 100.0, 60.0);
        let style = StrokeStyle::new(6.0)
            .with_alignment(StrokeAlignment::Inside)
            .with_dash(DashStyle::new(12.0, 8.0));
        let out = stroke_aligned(rect, &style, 0.1);
        assert!(!out.is_empty());
        assert!(rect.bounding_box().contains_rect(out.bounding_box()));
        // Unusable patterns fall back to solid.
        let solid_style = StrokeStyle::new(6.0).with_alignment(StrokeAlignment::Inside);
        let bad = solid_style
            .clone()
            .with_dash(DashStyle::from_pattern([0.0, 0.0]));
        let a = stroke_aligned(rect, &bad, 0.1);
        let b = stroke_aligned(rect, &solid_style, 0.1);
        assert_eq!(a.elements(), b.elements());
    }

    /// Zero-length dashes with round dash caps become dots.
    #[test]
    fn zero_dash_dots() {
        let mut line = BezPath::new();
        line.move_to((0.0, 0.0));
        line.line_to((30.0, 0.0));
        let style = StrokeStyle::new(4.0)
            .with_dash(DashStyle::from_pattern([0.0, 10.0]).with_cap(Cap::Round));
        let out = stroke_aligned(&line, &style, TOL);
        // Dots at s = 0, 10, 20, 30 ⇒ 4 circles of radius 2.
        let area = out.area().abs();
        assert!(
            (area - 4.0 * PI * 4.0).abs() < 0.05,
            "expected 4 dots, area {area}"
        );
    }
}
