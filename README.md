# kurbo-se — stroke extensions for kurbo

The feature set of Figma's stroke panel on top of [kurbo]'s public API:
stroke alignment (inside / center / outside), independent start and end
caps, dash caps, and dashed aligned strokes. The output is a fill outline.

kurbo-se is not a fork. It depends on unmodified kurbo from crates.io and
re-exports it as `kurbo_se::kurbo`, so its `BezPath` is the same type your
renderer already consumes. With [vello], the output feeds `Scene::fill`
directly.

By [Nursultan Akim](https://github.com/ninsent) ·
[github.com/ninsent/kurbo-se](https://github.com/ninsent/kurbo-se)

```rust
use kurbo_se::{StrokeStyle, StrokeAlignment, stroke_aligned};
use kurbo_se::kurbo::{Rect, Shape};

let shape = Rect::new(0.0, 0.0, 100.0, 60.0);
let style = StrokeStyle::new(8.0).with_alignment(StrokeAlignment::Inside);
let outline = stroke_aligned(shape, &style, 0.1);
// Fill `outline` with the nonzero winding rule.
```

| | | |
|---|---|---|
| ![inside-aligned star](https://raw.githubusercontent.com/ninsent/kurbo-se/main/docs/sandbox-star-inside.png) | ![dashed outside donut](https://raw.githubusercontent.com/ninsent/kurbo-se/main/docs/sandbox-donut-dashed.png) | ![saturated star](https://raw.githubusercontent.com/ninsent/kurbo-se/main/docs/sandbox-star-saturated.png) |
| inside-aligned, miter | donut, outside + dashed, round dash caps | past the local thickness the inside stroke saturates |
| ![bowtie outside](https://raw.githubusercontent.com/ninsent/kurbo-se/main/docs/sandbox-bowtie-outside.png) | ![figure-eight outside](https://raw.githubusercontent.com/ninsent/kurbo-se/main/docs/sandbox-figure-eight-outside.png) | ![sharp wedge](https://raw.githubusercontent.com/ninsent/kurbo-se/main/docs/sandbox-wedge-clip.png) |
| self-intersecting: outside never enters the fill | crossing lobes merge into one outside band | a thin wedge fills solid |

## Feature scope (Figma's stroke panel)

| Parameter | Values | Notes |
|---|---|---|
| `width` | `f64 ≥ 0` | `0` yields an empty path; negative or non-finite yields empty and `debug_assert`s |
| `alignment` | `Center` \| `Inside` \| `Outside` | resolved per subpath, hole-aware (see below) |
| `side` | `Option<Left \| Center \| Right>` | raw geometric override; bypasses alignment |
| `join` | `kurbo::Join` (Miter/Bevel/Round) | reused, not redefined |
| `miter_angle` | `0..=180` degrees | corner angle at or below which miters bevel; `limit = 1/sin(angle/2)`; conversions are exported |
| `start_cap`/`end_cap` | `kurbo::Cap` (Butt/Round/Square) | independent per end, open subpaths |
| `dash.pattern` | arbitrary `&[f64]` | even indices are dashes, odd are gaps; odd-length lists read doubled (SVG); unusable patterns render solid instead of panicking |
| `dash.offset` | any finite `f64` | normalized like `stroke-dashoffset`; animating it is a supported use case |
| `dash.cap` | `kurbo::Cap` | for dash-created edges; true endpoints keep the start/end caps; zero-length dashes render as dots (Round/Square) |
| `tolerance` | `f64` | applied everywhere, including join and cap arcs; `0.25` suits display |

Width profiles are out of scope.

## Semantics

- Output contract: a fill outline for the nonzero winding rule. The outline
  self-overlaps by design (inner joins, cusps, 180° turns). Nonzero cancels
  the overlaps; even-odd shows artifacts.
- Conventions: Y-down coordinates. The unit normal `t̂.turn_90()` points
  right of travel. Positive signed area means clockwise, as in kurbo.
- Closed contours are set-defined, matching Figma:
  `Inside(w) = { p ∈ F : dist(p, path) ≤ w }` and
  `Outside(w) = { p ∉ F : dist(p, path) ≤ w }`, where `F` is the filled
  region of the whole path. An inside stroke never leaves the fill and an
  outside stroke never enters it, at any width. A width beyond the local
  thickness saturates: the shape fills completely instead of the band
  folding over itself.
- Holes and compounds: a donut's hole strokes into the ring, not into the
  void. Winding direction of the input never changes what `Inside` means.
- Centered strokes on closed simple contours are set-defined too:
  `Center(w) = { p : dist(p, path) ≤ w/2 }`, the fill dilated by `w/2`
  minus the fill eroded by `w/2`. Past the local thickness the band
  saturates instead of the inverted inner boundary punching a hole. A
  circle stroked wider than its diameter stays a solid disc.
- Self-intersecting contours (bowtie, figure-eight) are split at their
  crossings. Each lobe resolves against the fill on its own.
- Open subpaths: inside/outside is geometrically undefined there. kurbo-se
  maps `Inside` to the right of travel and `Outside` to the left, following
  the SVG Strokes draft, and `side` sets it explicitly. Open subpaths are
  banded independently of the closed contours' region construction. A
  nearby open subpath never carves into a closed contour's band; overlaps
  add under nonzero winding.
- Dashing order: the side is resolved from the undashed subpath. Dashes are
  cut from the original centerline, so their lengths do not change with
  alignment. Each dash is then expanded on its own and masked by the fill,
  so a dashed inside stroke stays inside and a dashed outside stroke stays
  outside even where the band is wider than the local geometry. Closed
  contours seam-merge: a dash across the seam stays one piece, and
  animating `dash.offset` never pops.
- Exact shared boundary: for one-sided strokes the boundary at distance 0
  is the source path, element for element.

## Performance

Measured on an Apple-silicon laptop at tolerance 0.25 (criterion, release):

| Case | time |
|---|---|
| 10-point star (lines), inside, solid | 7.0 µs |
| circle (62 segs), center, solid | 50 µs (kurbo's native stroker: 12.9 µs) |
| circle (62 segs), inside, solid | 32 µs |
| circle, inside, dashed + round dash caps | 55 µs |
| donut (124 segs, hole), inside, solid | 99 µs |
| 40-cubic wavy path, inside, solid | 106 µs |
| 40-cubic wavy path, inside, dashed | 143 µs |

Run-to-run variation on a laptop is a few percent, so treat these as
magnitudes rather than exact figures.

Solid centered strokes on closed contours use the same set construction as
one-sided ones; that is what keeps an over-wide circle solid. Centered
strokes on open subpaths and all dashed centered strokes keep the direct
band at kurbo-stroker cost. One-sided strokes pay for the region
construction, but when the raw offsets have no intersections — every
smooth contour at moderate width — a fast path classifies each offset loop
whole and skips the cut/prune/stitch machinery. Polylines, the common UI
case, stay in single-digit microseconds. Degenerate widths (an offset
collapsing to a point at `w = r`, two offsets coinciding at half a ring's
thickness) are guarded and stay near a millisecond. Dashed one-sided
strokes also mask each dash against the fill; the mask short-circuits
wherever the band leaves the fill by less than `tolerance`, so the usual
cost is a per-dash scan, not a Boolean.

Re-expanding per frame (an animated `dash.offset`, for example) is the
supported model; there is no caching layer to manage. Use
[`stroke_aligned_with`] with a reused [`AlignedStrokeCtx`] so the large
expansion buffers keep their capacity across calls, like kurbo's own
`StrokeCtx`. The region and dash-mask stages still make small per-call
allocations, which the numbers above include.

## Renderer integration

kurbo-se expands strokes on the CPU. So does vello for every dashed stroke
(see *GPU-friendly Stroke Expansion*, §9.3). `examples/vello-demo` renders
inside/center/outside stars plus an animated dashed ring headlessly:

![vello demo](https://raw.githubusercontent.com/ninsent/kurbo-se/main/docs/vello-demo.png)

Measured there: over 120 frames at 900×620, scene build including all
stroke expansion averages 0.034 ms; the full GPU frame averages 1.6 ms.
For plain centered solid strokes at moderate widths you can skip kurbo-se
at render time: `kurbo::Stroke::from(&style)` converts. The conversion is
lossy — alignment and dash caps have no kurbo equivalent — and kurbo's
stroker folds over itself past the local thickness, which the centered set
construction here avoids. The impl documents the details.

## Relationship to kurbo

kurbo-se reuses kurbo's public machinery: `offset_cubic` (the
error-controlled, cusp-aware offsetter), `dash`, `Arc`, `Join`/`Cap`, and
the subpath plumbing. It reimplements what is private: join and cap
emission, generalized from kurbo's symmetric stroker to independent
per-side distances, plus robust endpoint tangents and collinear-cubic
handling. The geometry conventions match kurbo's exactly. kurbo is
dual-licensed Apache-2.0/MIT by the Kurbo Authors; this crate follows both
the license and, gratefully, the design.

## Known limitations

- Boundary band: classification within roughly one tolerance of
  `dist = w` is approximate. The offset curve, the flattened distance
  index, and the cut positions each carry tolerance-scale error. Region
  membership is exact everywhere else. A region component thinner than
  that band everywhere (the sliver annulus of a donut stroked at exactly
  half its ring thickness) collapses to clean saturation rather than
  rendering as sub-tolerance fuzz.
- Self-intersecting contours, centered: centered strokes keep kurbo's
  winding-additive band on self-intersecting contours, because splitting
  them would change moderate-width output away from kurbo/SVG parity. At
  extreme widths their inverted inner boundaries can still cancel.
- Interleaved self-intersections: a contour whose crossings nest
  irregularly (a pretzel) decomposes only partially into lobes. The result
  stays finite and contained, but the lobe split is not guaranteed minimal.
- Overlapping lobes of one contour: where a self-intersecting contour
  overlaps itself and the fill winds twice, the erosion inside the
  double-wound part may still paint.
- Dashed strokes at extreme widths: dashes are expanded as open bands and
  masked by the fill. They stay contained but do not saturate the way
  solid closed contours do. Where the shape is thinner than the weight a
  dash covers it, but neighbouring dashes do not merge into a solid fill.
- Gap-free "dash" patterns: a pattern whose single dash swallows an entire
  closed contour (dash ≥ perimeter, zero gap) expands as an unmasked ring
  and can cross the fill boundary at extreme widths. That pattern means
  "not dashed"; drop `dash` instead.
- Dash masking is tolerance-driven: a dash band that leaves the fill by
  less than `tolerance` is left alone. On any convex curve every cap pokes
  out by a fraction of the local sagitta. A finer tolerance tightens the
  mask.
- Dash phase restarts at each subpath (kurbo `stroke_with` semantics);
  browsers continue it across subpaths.
- Coordinate magnitude: this pipeline squares coordinates throughout, so a
  path whose coordinates do not survive squaring — beyond roughly `1e154` —
  is treated like non-finite input and yields an empty outline. Real
  coordinate systems are nowhere near that.
- Dashing cost is the path length divided by the pattern period, so a very
  long path with a very short pattern produces correspondingly many dashes.
  That work is real, not a defect, but it is worth knowing before dashing a
  path that spans millions of units with a ten-unit pattern.
- Raw `Shape::area` on stroke outlines over-counts self-overlap pockets.
  Measure regions by winding, not by summed signed area.

## Sandbox

`sandbox/` hosts an interactive wasm playground (Vite + plain SVG) with a
Figma-style panel, a gallery of pathological shapes, debug layers
(even-odd x-ray, direction and orientation badges, miter-fallback markers)
and a native-SVG overlay as a center-stroke oracle. See
[sandbox/README.md](sandbox/README.md).

## Features

- `std` (default): float functions from the standard library.
- `libm`: `no_std` support; an allocator is still required.
- `serde`: Serialize/Deserialize for the style types.

MSRV: Rust 1.85. No `unsafe`.

## Credits

Written by Nursultan Akim — <contact@nursultan.me> ·
<https://github.com/ninsent/kurbo-se>

Built on [kurbo] by Raph Levien and the Kurbo Authors. kurbo's offset
engine, dashing, and path plumbing do the heavy lifting here, and this
crate follows kurbo's geometric conventions exactly.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option — the same dual licence as kurbo.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this crate by you, as defined in the Apache-2.0
licence, shall be dual licensed as above, without any additional terms or
conditions.

[kurbo]: https://github.com/linebender/kurbo
[vello]: https://github.com/linebender/vello
[`stroke_aligned_with`]: https://docs.rs/kurbo-se/latest/kurbo_se/fn.stroke_aligned_with.html
[`AlignedStrokeCtx`]: https://docs.rs/kurbo-se/latest/kurbo_se/struct.AlignedStrokeCtx.html
