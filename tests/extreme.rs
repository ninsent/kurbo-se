// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Regression tests for the extreme-weight and self-intersection bug
//! reports (sandbox session, 2026-08-20):
//! 1. extreme weights saturate — the band fills the shape rather than
//!    folding over itself — and never cross the fill boundary,
//! 2. self-overlapping open paths must not punch unpainted pockets,
//! 3. self-intersecting closed shapes resolve inside/outside per lobe
//!    against the fill, like Figma.
//!
//! The set-theoretic contract itself lives in `set_semantics.rs`; these are
//! the concrete shapes and widths from the bug reports.

use kurbo::{BezPath, Circle, Point, Rect, Shape};
use kurbo_se::{StrokeAlignment, StrokeStyle, stroke_aligned};

fn grid_check(
    out: &BezPath,
    src: &BezPath,
    bbox: kurbo::Rect,
    n: usize,
    mut expect: impl FnMut(Point, bool, bool) -> Result<(), &'static str>,
) {
    for iy in 0..n {
        for ix in 0..n {
            let p = Point::new(
                bbox.x0 + (ix as f64 + 0.5) / n as f64 * bbox.width(),
                bbox.y0 + (iy as f64 + 0.5) / n as f64 * bbox.height(),
            );
            let in_out = out.winding(p) != 0;
            let in_src = src.winding(p) != 0;
            if let Err(msg) = expect(p, in_out, in_src) {
                panic!("{msg} at {p:?}");
            }
        }
    }
}

/// Bug 1a: star at weight 60 — the stroke must stay inside the star and
/// cover it solidly (every fill point is within 60 of the boundary).
#[test]
fn star_extreme_weight_inside() {
    let mut star = BezPath::new();
    for i in 0..10 {
        let r = if i % 2 == 0 { 100.0 } else { 40.0 };
        let a = std::f64::consts::PI * (i as f64) / 5.0;
        let pt = (150.0 + r * a.cos(), 150.0 + r * a.sin());
        if i == 0 {
            star.move_to(pt);
        } else {
            star.line_to(pt);
        }
    }
    star.close_path();
    let style = StrokeStyle::new(60.0).with_alignment(StrokeAlignment::Inside);
    let out = stroke_aligned(&star, &style, 1e-3);
    assert!(out.is_finite());
    let bbox = star.bounding_box().inflate(10.0, 10.0);
    grid_check(&out, &star, bbox, 72, |_, in_out, in_src| {
        if in_out && !in_src {
            return Err("stroke leaked outside the star");
        }
        Ok(())
    });
    // Every point of the star is within 60 of its outline (the inner radius
    // is 40), so the stroke saturates: the shape is filled completely.
    let bbox2 = star.bounding_box();
    for iy in 0..64 {
        for ix in 0..64 {
            let p = Point::new(
                bbox2.x0 + (ix as f64 + 0.5) / 64.0 * bbox2.width(),
                bbox2.y0 + (iy as f64 + 0.5) / 64.0 * bbox2.height(),
            );
            if star.winding(p) != 0 {
                assert!(out.winding(p) != 0, "saturated star has a hole at {p:?}");
            }
        }
    }
}

/// Bug 1b: donut at weight 100 — the hole must stay unpainted and the ring
/// must be fully covered (Figma parity via the pair clamp).
#[test]
fn donut_extreme_weight_inside() {
    let mut donut = Circle::new((130.0, 130.0), 100.0).to_path(1e-6);
    donut.extend(
        Circle::new((130.0, 130.0), 45.0)
            .to_path(1e-6)
            .reverse_subpaths()
            .iter(),
    );
    let style = StrokeStyle::new(100.0).with_alignment(StrokeAlignment::Inside);
    let out = stroke_aligned(&donut, &style, 1e-3);
    assert!(out.is_finite());
    // Hole center and hole interior stay clean.
    for r in [0.0, 20.0, 40.0] {
        let p = Point::new(130.0 + r, 130.0);
        assert_eq!(out.winding(p), 0, "hole painted at radius {r}");
    }
    // Ring fully covered (between r=45 and r=100).
    for r in [50.0, 70.0, 95.0] {
        let p = Point::new(130.0 + r, 130.0);
        assert!(out.winding(p) != 0, "ring not covered at radius {r}");
    }
    // Nothing outside the outer circle.
    for r in [105.0, 130.0] {
        let p = Point::new(130.0 + r, 130.0);
        assert_eq!(out.winding(p), 0, "stroke leaked outside at radius {r}");
    }
}

/// Bug 1c: rect 80×60 at weight 35 (2w exceeds the short side) — the whole
/// rect is within 35 of an edge, so the inside stroke covers it without
/// cancellation holes.
#[test]
fn rect_overlapping_inside_band() {
    let rect = Rect::new(40.0, 80.0, 120.0, 140.0);
    let src = rect.to_path(1e-9);
    let style = StrokeStyle::new(35.0).with_alignment(StrokeAlignment::Inside);
    let out = stroke_aligned(rect, &style, 1e-3);
    // 2w exceeds the short side, so the band saturates: the rect is filled
    // solid, including the centre line where the two sides would overlap.
    for p in [
        (80.0, 85.0),
        (80.0, 135.0),
        (45.0, 110.0),
        (115.0, 110.0),
        (80.0, 110.0),
        (80.0, 108.0),
        (80.0, 112.0),
    ] {
        assert!(out.winding(p.into()) != 0, "unpainted hole at {p:?}");
    }
    let bbox = src.bounding_box().inflate(8.0, 8.0);
    grid_check(&out, &src, bbox, 64, |_, in_out, in_src| {
        if in_out && !in_src {
            return Err("stroke leaked outside the rect");
        }
        Ok(())
    });
}

/// Bug 1d: sharp wedge at weight 60 — no leak beyond the source triangle.
#[test]
fn wedge_extreme_weight_containment() {
    let mut wedge = BezPath::new();
    wedge.move_to((20.0, 180.0));
    wedge.line_to((230.0, 160.0));
    wedge.line_to((20.0, 140.0));
    wedge.close_path();
    let style = StrokeStyle::new(60.0).with_alignment(StrokeAlignment::Inside);
    let out = stroke_aligned(&wedge, &style, 1e-3);
    assert!(out.is_finite());
    let bbox = wedge.bounding_box().inflate(20.0, 20.0);
    grid_check(&out, &wedge, bbox, 72, |_, in_out, in_src| {
        if in_out && !in_src {
            return Err("wedge stroke leaked outside");
        }
        // The wedge is thinner than 2*60 everywhere, so it saturates.
        if in_src && !in_out {
            return Err("saturated wedge has a hole");
        }
        Ok(())
    });
}

/// Bug 3: bowtie — inside hugs the fill of *each lobe*; outside never
/// enters the fill. (Figma reference screenshots.)
#[test]
fn bowtie_lobes_follow_fill() {
    let mut bowtie = BezPath::new();
    bowtie.move_to((20.0, 20.0));
    bowtie.line_to((220.0, 180.0));
    bowtie.line_to((220.0, 20.0));
    bowtie.line_to((20.0, 180.0));
    bowtie.close_path();

    let inside = stroke_aligned(
        &bowtie,
        &StrokeStyle::new(12.0).with_alignment(StrokeAlignment::Inside),
        1e-3,
    );
    let outside = stroke_aligned(
        &bowtie,
        &StrokeStyle::new(12.0).with_alignment(StrokeAlignment::Outside),
        1e-3,
    );
    let bbox = bowtie.bounding_box().inflate(16.0, 16.0);
    // Inside ⊆ fill, outside ∩ fill = ∅ — even though the lobes wind in
    // opposite directions.
    grid_check(&inside, &bowtie, bbox, 80, |_, in_out, in_src| {
        if in_out && !in_src {
            return Err("inside stroke left the fill");
        }
        Ok(())
    });
    grid_check(&outside, &bowtie, bbox, 80, |_, in_out, in_src| {
        if in_out && in_src {
            return Err("outside stroke entered the fill");
        }
        Ok(())
    });
    // And the inside stroke actually hugs BOTH lobes (probe near each lobe's
    // boundary, a few px inside the fill).
    for p in [(40.0, 40.0), (200.0, 40.0), (40.0, 160.0), (200.0, 160.0)] {
        let p: Point = p.into();
        if bowtie.winding(p) != 0 {
            assert!(inside.winding(p) != 0, "lobe not hugged at {p:?}");
        }
    }
}

/// Bug 2: open curve with a loop — the loop lobe and the strands band
/// independently; overlaps add instead of cancelling (no unpainted bowties).
#[test]
fn open_loop_curve_no_cancellation() {
    let mut p = BezPath::new();
    p.move_to((20.0, 150.0));
    p.curve_to((200.0, 20.0), (0.0, 20.0), (180.0, 150.0));
    let style = StrokeStyle::new(30.0).with_alignment(StrokeAlignment::Outside);
    let out = stroke_aligned(&p, &style, 1e-3);
    assert!(out.is_finite());
    assert!(!out.is_empty());
    // The band region along the left-of-travel side near the start and end
    // must be painted; sample by offsetting from curve points.
    // Start heads up-right (toward the first control point); left of travel
    // is up-left.
    let probes = [
        Point::new(15.0, 135.0),  // left of the start segment
        Point::new(185.0, 135.0), // left of the end (travel down-right, left = up-right)
    ];
    for p in probes {
        assert!(out.winding(p) != 0, "band missing at {p:?}");
    }
}

/// Degenerate-width regression (sandbox session, 2026-08-20): at the exact
/// width where an offset degenerates, the pipeline used to shred it into
/// thousands of fuzz segments (donut at half its ring thickness) or spend
/// over 100 ms cutting a collapsed offset cluster (circle at w = r). The
/// output must stay small and correctly saturated.
#[test]
fn degenerate_widths_stay_bounded() {
    // Donut at exactly half the ring thickness: inward offsets coincide.
    let mut donut = Circle::new((130.0, 130.0), 100.0).to_path(1e-6);
    donut.extend(
        Circle::new((130.0, 130.0), 45.0)
            .to_path(1e-6)
            .reverse_subpaths()
            .iter(),
    );
    let style = StrokeStyle::new(27.5).with_alignment(StrokeAlignment::Inside);
    let out = stroke_aligned(&donut, &style, 0.01);
    assert!(out.is_finite());
    assert!(
        out.segments().count() < 200,
        "coincident offsets shredded into {} segments",
        out.segments().count()
    );
    // The ring saturates: mid-ring points are painted, the hole stays clean.
    assert!(
        out.winding((130.0 + 72.5, 130.0).into()) != 0,
        "ring not covered"
    );
    assert_eq!(out.winding((130.0, 130.0).into()), 0, "hole painted");

    // Circle at w = r: the inside offset collapses to a point.
    let circle = Circle::new((120.0, 120.0), 80.0).to_path(1e-6);
    let style = StrokeStyle::new(80.0).with_alignment(StrokeAlignment::Inside);
    let out = stroke_aligned(&circle, &style, 0.01);
    assert!(out.is_finite());
    assert!(
        out.segments().count() < 100,
        "collapsed offset leaked {} segments",
        out.segments().count()
    );
    // Saturated: the whole disc is painted.
    assert!(
        out.winding((120.0, 120.0).into()) != 0,
        "center not painted"
    );
    assert!(
        out.winding((120.0 + 79.0, 120.0).into()) != 0,
        "rim not painted"
    );
}

/// The sandbox "Multi-subpath mixed" gallery shape: an open polyline above
/// a rect and a reversed circle.
fn mixed_shape() -> BezPath {
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
    p
}

/// Every fill-interior cell must stay unpainted by an outside stroke.
fn assert_outside_avoids_fill(out: &BezPath, src: &BezPath, bbox: Rect, n: usize, label: &str) {
    for iy in 0..n {
        for ix in 0..n {
            let p = Point::new(
                bbox.x0 + (ix as f64 + 0.5) / n as f64 * bbox.width(),
                bbox.y0 + (iy as f64 + 0.5) / n as f64 * bbox.height(),
            );
            if src.winding(p) != 0 {
                assert_eq!(
                    out.winding(p),
                    0,
                    "{label}: outside stroke inside fill at {p:?}"
                );
            }
        }
    }
}

/// Outside stroke on a mixed open+closed path (2026-08-20 report): the open
/// polyline must not take part in the closed contours' region construction.
/// Previously its proximity pruned chunks out of the rect/circle dilations
/// at weights above ~24, and the stitch bridged the holes with chords
/// across the fill.
#[test]
fn mixed_open_closed_outside_extreme() {
    let p = mixed_shape();
    for join in [kurbo::Join::Round, kurbo::Join::Miter, kurbo::Join::Bevel] {
        for w in [11.0, 21.0, 25.5, 30.0, 44.0, 47.0, 60.0] {
            let style = StrokeStyle::new(w)
                .with_alignment(StrokeAlignment::Outside)
                .with_join(join);
            let out = stroke_aligned(&p, &style, 1e-3);
            assert!(out.is_finite());
            // Outside stroke never enters any fill — dense grids over both
            // closed shapes (the miter-wedge bug painted a vertical strip
            // through the circle's left half).
            assert_outside_avoids_fill(
                &out,
                &p,
                Rect::new(40.0, 80.0, 120.0, 140.0),
                24,
                &format!("rect {join:?} w={w}"),
            );
            assert_outside_avoids_fill(
                &out,
                &p,
                Rect::new(155.0, 75.0, 225.0, 145.0),
                24,
                &format!("circle {join:?} w={w}"),
            );
            // The ring must hug the rect's top-left corner outward — the
            // corner nearest the roof polyline, where the open-path pruning
            // used to carve it off. Diagonal reach depends on the join:
            // bevel chords stop at w/√2, round at w, miter at w·√2.
            let factor = if join == kurbo::Join::Bevel {
                0.55
            } else {
                0.9
            };
            let d = w / core::f64::consts::SQRT_2 * factor;
            let probe = Point::new(40.0 - d, 80.0 - d);
            assert!(
                out.winding(probe) != 0,
                "rect corner ring missing {join:?} w={w} ({probe:?})"
            );
            // Once the two dilations overlap (gap is 35), the merged band
            // must cover the midpoint between the shapes.
            if w > 20.0 {
                assert!(
                    out.winding((137.5, 110.0).into()) != 0,
                    "merged band has a gap {join:?} w={w}"
                );
            }
        }
    }
}

/// Saturation is monotone in the weight (2026-08-21 report): once the band
/// covers the shape, every larger weight must keep covering it — no holes
/// reappearing, no leaks outside.
///
/// The star regressed here at w ≈ 79–95 while 60 and 100 were fine: the
/// distance prune exempted join geometry outright, so at extreme widths the
/// enormous join fans at the star's reflex corners survived inside the fill
/// and stitched into a bogus erosion loop. The exemption is now bounded by
/// the join's real reach (`cos(φ/2)` for a bevel chord).
#[test]
fn extreme_inside_saturation_is_monotone() {
    let mut star = BezPath::new();
    for i in 0..10 {
        let r = if i % 2 == 0 { 100.0 } else { 40.0 };
        let a = std::f64::consts::PI * (i as f64) / 5.0 - std::f64::consts::FRAC_PI_2;
        let pt = (150.0 + r * a.cos(), 150.0 + r * a.sin());
        if i == 0 {
            star.move_to(pt);
        } else {
            star.line_to(pt);
        }
    }
    star.close_path();

    let mut wedge = BezPath::new();
    wedge.move_to((20.0, 180.0));
    wedge.line_to((230.0, 160.0));
    wedge.line_to((20.0, 140.0));
    wedge.close_path();

    let rect = Rect::new(40.0, 80.0, 160.0, 140.0).to_path(1e-9);
    let circle = Circle::new((150.0, 150.0), 100.0).to_path(1e-6);

    // Past these weights each shape is fully within reach of its own
    // boundary, so the inside stroke must paint it solid.
    for (name, src, saturated_at) in [
        ("star", &star, 40.0),
        ("wedge", &wedge, 25.0),
        ("rect", &rect, 35.0),
        ("circle", &circle, 105.0),
    ] {
        for join in [kurbo::Join::Miter, kurbo::Join::Round, kurbo::Join::Bevel] {
            for w in [
                saturated_at,
                saturated_at * 1.5,
                79.0_f64.max(saturated_at),
                85.0_f64.max(saturated_at),
                95.0_f64.max(saturated_at),
                saturated_at * 4.0,
                saturated_at * 20.0,
            ] {
                let style = StrokeStyle::new(w)
                    .with_alignment(StrokeAlignment::Inside)
                    .with_join(join);
                let out = stroke_aligned(src, &style, 1e-3);
                assert!(out.is_finite(), "{name}/{join:?}/w{w}: non-finite");
                let bbox = src.bounding_box().inflate(8.0, 8.0);
                grid_check(&out, src, bbox, 96, |_, in_out, in_src| {
                    if in_out && !in_src {
                        return Err("saturated stroke leaked outside the shape");
                    }
                    if in_src && !in_out {
                        return Err("saturated stroke left a hole");
                    }
                    Ok(())
                });
            }
        }
    }
}

/// Dashed one-sided strokes are masked by the fill (2026-08-21 report).
///
/// A dash is banded directly — the region construction cannot be used once
/// the contour is cut into dashes — so a band that does not fit the local
/// geometry used to escape: a dash starting at a star tip (where the wedge
/// is far thinner than the weight) sat mostly outside the shape, and dashes
/// near the bowtie's crossing reached into the neighbouring lobe's fill.
#[test]
fn dashed_one_sided_stays_masked() {
    let mut star = BezPath::new();
    for i in 0..10 {
        let r = if i % 2 == 0 { 100.0 } else { 40.0 };
        let a = std::f64::consts::PI * (i as f64) / 5.0 - std::f64::consts::FRAC_PI_2;
        let pt = (150.0 + r * a.cos(), 150.0 + r * a.sin());
        if i == 0 {
            star.move_to(pt);
        } else {
            star.line_to(pt);
        }
    }
    star.close_path();

    let mut bowtie = BezPath::new();
    bowtie.move_to((20.0, 20.0));
    bowtie.line_to((220.0, 180.0));
    bowtie.line_to((220.0, 20.0));
    bowtie.line_to((20.0, 180.0));
    bowtie.close_path();

    let circle = Circle::new((150.0, 150.0), 100.0).to_path(1e-6);

    for (name, src) in [("star", &star), ("bowtie", &bowtie), ("circle", &circle)] {
        for cap in [
            kurbo_se::Cap::Round,
            kurbo_se::Cap::Butt,
            kurbo_se::Cap::Square,
        ] {
            for (alignment, inside) in [
                (StrokeAlignment::Inside, true),
                (StrokeAlignment::Outside, false),
            ] {
                // Two phases: the reported one, plus a shift that lands dash
                // ends elsewhere on the corners.
                for offset in [-39.0, 0.0, 7.5] {
                    let style = StrokeStyle::new(10.0).with_alignment(alignment).with_dash(
                        kurbo_se::DashStyle::from_pattern([20.0, 29.0])
                            .with_offset(offset)
                            .with_cap(cap),
                    );
                    let out = stroke_aligned(src, &style, 1e-3);
                    assert!(out.is_finite(), "{name}/{alignment:?}/{cap:?} non-finite");
                    let label = format!("{name}/{alignment:?}/{cap:?}/off{offset}");
                    let bbox = src.bounding_box().inflate(20.0, 20.0);
                    grid_check(&out, src, bbox, 96, |_, in_out, in_src| {
                        if inside && in_out && !in_src {
                            return Err("dashed inside stroke left the fill");
                        }
                        if !inside && in_out && in_src {
                            return Err("dashed outside stroke entered the fill");
                        }
                        Ok(())
                    });
                    // The mask must trim, not erase: dashes still paint.
                    assert!(
                        out.area().abs() > 500.0,
                        "{label}: dashes vanished (area {})",
                        out.area().abs()
                    );
                }
            }
        }
    }
}

/// Figure-eight is now handled by lobe splitting: inside stays inside the
/// fill of both lobes (previously a documented limitation).
#[test]
fn figure_eight_now_correct() {
    let mut p = BezPath::new();
    p.move_to((120.0, 100.0));
    p.curve_to((150.0, 55.0), (210.0, 55.0), (240.0, 100.0));
    p.curve_to((210.0, 145.0), (150.0, 145.0), (120.0, 100.0));
    p.curve_to((90.0, 55.0), (30.0, 55.0), (0.0, 100.0));
    p.curve_to((30.0, 145.0), (90.0, 145.0), (120.0, 100.0));
    p.close_path();
    let style = StrokeStyle::new(8.0).with_alignment(StrokeAlignment::Inside);
    let out = stroke_aligned(&p, &style, 1e-3);
    let bbox = p.bounding_box().inflate(10.0, 10.0);
    grid_check(&out, &p, bbox, 80, |_, in_out, in_src| {
        if in_out && !in_src {
            return Err("figure-eight inside stroke escaped the fill");
        }
        Ok(())
    });
    // Both lobes hugged (probe just inside each lobe's extreme point).
    for probe in [(234.0, 100.0), (6.0, 100.0)] {
        let p2: Point = probe.into();
        assert!(out.winding(p2) != 0, "lobe not hugged at {p2:?}");
    }
}
