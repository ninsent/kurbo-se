# Changelog

This changelog follows <https://keepachangelog.com/en/>.

## 0.1.0 (unreleased)

Initial release, targeting kurbo 0.13.

### Added

- `StrokeStyle` / `StrokeAlignment` / `StrokeSide` / `DashStyle`: Figma's
  stroke panel as a style type. Alignment (inside/center/outside), raw side
  override, joins with a miter angle in degrees, independent start/end
  caps, dash pattern/offset/cap.
- `stroke_aligned` / `stroke_aligned_with`, plus `AlignedStrokeCtx` for
  allocation reuse: expand an aligned stroke into a nonzero-rule fill
  outline. One-sided bands keep the source path as their exact shared
  boundary. Sharp corners are clipped so inside strokes never leak outside
  the fill, and outside strokes never leak in.
- Hole-aware per-subpath side resolution via a winding probe. Holes stroke
  into the ring; reversing winding direction never changes the result.
- Dashing on the centerline, with seam-merge on closed contours (smooth
  `dash_offset` animation), pattern sanitization (empty, negative, or
  zero-sum patterns render solid instead of panicking or hanging), and
  zero-length dashes rendered as dots (round, or oriented square).
- `miter_angle_to_limit` / `miter_limit_to_angle` conversions.
- `From<&StrokeStyle> for kurbo::Stroke` escape hatch. Centered
  interpretation; the lossiness is documented on the impl.
- `analyze_subpaths` introspection for editors and debug tooling.
- `no_std` support (`libm` feature), a `serde` feature, no `unsafe`.
- Interactive wasm sandbox (`sandbox/`), headless vello example
  (`examples/vello-demo`), criterion benchmarks.
- Golden characterization tests (`tests/golden.rs` plus a repo-only
  fixture) that pin the exact outline of a 10-shape × 8-style matrix, as a
  net for behavior-preserving refactors.
- Package metadata: author, repository and homepage
  (<https://github.com/ninsent/kurbo-se>), plus `LICENSE-APACHE`,
  `LICENSE-MIT` and `AUTHORS` files.

The sections below record the pre-release development history of the
above. None of it was ever published.

### Changed (during development)

- Aligned strokes on closed contours became set-defined, matching Figma:
  `Inside(w) = { p ∈ F : dist(p, path) ≤ w }` and
  `Outside(w) = { p ∉ F : dist(p, path) ≤ w }`. An inside stroke never
  leaves the fill and an outside stroke never enters it, at any width. A
  width beyond the local thickness saturates: the shape fills solid
  instead of the band folding over itself and punching hollow pockets.
  Implemented by pruning the raw offset — cut at every intersection,
  discard pieces nearer than `w` or on the wrong side of the fill,
  restitch — replacing the earlier effective-width clamp.
- Self-intersecting contours are split at their crossings. Each lobe of a
  bowtie or figure-eight resolves its side against the fill independently,
  and overlapping bands add instead of cancelling.
- Faster predicates: the source is flattened once into a `y`-bucketed edge
  index, so distance and winding queries no longer run a per-curve solve.
  Line-involving intersections use kurbo's analytic
  `PathSeg::intersect_line`.
- One-sided expansion fast path: when the raw offsets have no mutual or
  self intersections — every smooth contour at moderate width — each
  offset loop is classified whole, and the cut/prune/stitch machinery is
  skipped. Adjacent segments are domain-trimmed before intersection, so
  smooth joints reject at the bounding-box level instead of recursing to
  flatness. Inside/outside strokes on circles and donuts got 2–5× faster,
  and one-sided cost no longer grows as tolerance shrinks.

### Fixed (during development)

- Centered strokes on closed contours grew holes at extreme weights. Past
  `w/2 >` the local thickness, the direct band's inner boundary inverts
  and its winding cancels interior coverage: a circle stroked wider than
  its diameter had a hole in the middle, and a donut's hole contour did
  the same from `w/2 >` the hole radius. Solid centered bands on closed
  simple contours are now built set-theoretically like the one-sided ones,
  `Center(w) = D_{w/2} \ E_{w/2}`, so they saturate instead. Centered
  strokes on open subpaths, dashed centered strokes, and self-intersecting
  contours (kurbo winding-additive parity, documented) keep the direct
  band. Solid centered closed contours now cost the region construction:
  about 50 µs for the 62-segment circle against 14 µs before.
- Dashing stopped partway along paths containing degenerate segments. A
  fully coincident `QuadTo` has a `NaN` arc length upstream
  (`QuadBez::arclen`), which poisoned the dash phase; everything past that
  element rendered as one uninterrupted band. Segments whose squared
  length vanishes are now dropped before dashing and before
  self-intersection splitting. That includes control deltas too small to
  square without underflowing, which are not point-coincident and used to
  make the whole output non-finite; runs of them also read as the path
  revisiting a vertex and split dashes for no reason. Arc-length reads are
  additionally guarded, so an unmeasurable overflow-scale segment cannot
  stall the phase. Reported upstream as
  [`upstream/04-quad-arclen-nan.md`](upstream/04-quad-arclen-nan.md).
- Solid strokes on paths with underflow-scale segments went non-finite. A
  delta of about 1e-300 passes an exact point-inequality check, but its
  squared length underflows to zero, so normalizing the tangent divided by
  zero. Only the dash and split stages had the guard. Degenerate segments
  are now dropped once at input canonicalization, which protects every
  construction. The synthesized closing chord, created after
  canonicalization, gets its own underflow-aware gate, and an
  all-subnormal subpath now renders like a coincident-point one: a dot at
  most. Measured cost: none.
- Square dash caps on curved contours shattered each dash into a wedge
  plus a sliver. The cap overshoots the fill on a convex curve, so the
  mask runs; a fill-boundary piece that straddled the end of the band's
  shared edge was discarded whole as coincident, stranding that edge as
  its own loop. The fill boundary is now also cut at the ends of the
  shared edge, so every piece is either wholly coincident with it or
  wholly free.
- Extreme inside weights on a star punched a hole in the middle of an
  otherwise saturated shape: weights around 79–95 failed while 60 and 100
  were fine. The distance prune exempted outer-side join geometry
  outright, so at large widths the join fans at the star's reflex corners
  survived inside the fill and stitched into a bogus erosion loop. The
  exemption is now bounded — a bevel chord may come within `cos(φ/2)·w` of
  its corner, round joins and miters not at all — and applies only to the
  dilation, where corner-cutting is the requested style and nothing folds
  inward. For the erosion the distance test is the saturation guard and
  stays strict. Dropping a bevel chord there costs nothing, because the
  stitcher bridges the gap with the same chord.
- Dashed inside/outside strokes are now masked by the fill. A dash is
  banded directly, since the region construction cannot be used once the
  contour is cut into dashes, so a band that did not fit the local
  geometry escaped: a dash starting at a star's tip sat mostly outside the
  shape, and dashes beside the bowtie's crossing reached into the
  neighbouring lobe's fill. Each dash and synthesized dot is now
  intersected with the fill, or subtracted from it for outside alignment.
  The mask is skipped where the excursion is below `tolerance`, so
  display-tolerance dashing keeps its speed. A dashed circle costs about
  55 µs against 31 µs before.
- Inside/outside strokes on paths mixing open and closed subpaths: the
  open subpaths' geometry took part in the region predicates (distance
  pruning and implicit-closure winding), so at larger widths a nearby open
  polyline carved chunks out of a closed contour's ring, and the stitcher
  bridged the holes with chords across the fill (sandbox "Multi-subpath
  mixed", outside weights above about 24). The region construction now
  sees only the closed contours that participate in it. Open subpaths are
  banded independently and add by winding where they overlap.
- The Boolean between overlapping one-sided bands of different contours is
  now join-aware. Cross-contour validity of a boundary piece used to be a
  distance test against the other sources, which is only correct for round
  joins: it under-pruned inside miter wedges (another contour's arc stubs
  survived inside a corner and stitched into strips across the fill) and
  over-pruned inside bevel notches (the union boundary lost its transition
  pieces and split into disjoint loops, exposing the fill). It is now a
  winding test against the actual raw offset loops, with a coincidence
  guard for loops closer than the resolution. The distance test remains as
  the self-fold guard, restricted to the piece's own contour. Outer-side
  join geometry — bevel chords, miter legs, round fans — is tagged at
  emission and exempt from it.
- Degenerate one-sided widths no longer blow up:
  - A collapsed offset (a circle stroked inside at exactly `w = r`) was
    shattered by cusp handling into thousands of segments and cut
    pairwise, over 120 ms per call. A resolution guard now drops loops
    smaller than the pruning fuzz before cutting: about 1 ms, correct
    saturation.
  - Two coincident offsets (a donut at exactly half its ring thickness)
    stitched about 1.4k fuzz segments into the output. A net-area gate now
    collapses region components thinner than the boundary band everywhere
    into clean saturation.
- Sandbox redesign: refreshed palette and panel styling; stats moved to a
  bottom bar so they no longer overlap the drawing; Figma-style navigation
  (two-finger scroll pans, pinch or ⌘/ctrl+scroll zooms toward the cursor,
  a zoom toolbar with fit, double-click to fit, a clamped zoom range); a
  shape Fill toggle with colour pickers for both the fill and the stroke
  result, all URL-shareable; and a pruned gallery. The kurbo #344 cubic
  and the collinear-cubics entries are gone, and the polyline spiral is
  now a smooth Hermite-fitted Archimedean spiral.
- Sandbox: the dev server's wasm now builds with `opt-level = 2`, and
  `npm run build` uses `wasm-pack --release`. Both were unoptimized
  `--dev` builds before, the dominant cause of sandbox lag.
- `no_std` (`libm`) build: the `y`-bucket lookups in the source index
  called the std-only `f64::floor`/`f64::ceil` inherent methods. They now
  go through the crate's float shims. Caught by the CI `no-std` job.

### Sandbox

- The dash controls take a full Figma-style pattern (`2, 4, 6, 8`) instead
  of a single dash/gap pair. Even entries are dash lengths, odd entries
  gaps, and odd-length lists read doubled — the semantics the crate always
  had in `DashStyle::from_pattern`, now reachable from the panel, from the
  shareable URL (`dashes=2,4,6,8`; the old `dash`/`gap` params still
  parse), and from the native-SVG reference overlay, whose
  `stroke-dasharray` shares the same semantics and so remains a valid
  oracle. Pinned crate-side by `tests/dash_pattern.rs`, including the
  byte-exact odd-doubling equivalence.

### Internal (during development)

- The two flattened edge indexes (source path and raw offset loops) now
  share one `y`-bucketed `EdgeIndex` with filter-parameterized winding and
  proximity predicates. The region-construction call sites share a common
  setup helper. Small geometry helpers (`append_seg`, `overlaps`,
  `start_point`) are single-sourced. The vestigial per-piece effective
  width, a leftover of the pre-region width clamp, is gone. No output or
  measured performance change (golden suite and benches).
- Documented limitation: a dash pattern whose single dash swallows an
  entire closed contour (dash ≥ perimeter, zero gap) expands as an
  unmasked ring and can cross the fill boundary at extreme widths. Use a
  solid stroke instead.
- The golden fixture records the OS it was generated on, and the test
  skips elsewhere: a different libm can flip a subdivision decision and
  change the segment count, which the per-coordinate slack cannot absorb.
  The CI matrix still enforces the fixture on a matching runner.
