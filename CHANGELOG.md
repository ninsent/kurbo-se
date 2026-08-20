# Changelog

This changelog follows <https://keepachangelog.com/en/>.

## Unreleased

### Added

- Package metadata: author, repository and homepage
  (<https://github.com/ninsent/kurbo-se>), plus `LICENSE-APACHE`,
  `LICENSE-MIT` and `AUTHORS` files.

### Changed

- **Aligned strokes on closed contours are now defined set-theoretically**,
  matching Figma: `Inside(w) = { p ∈ F : dist(p, path) ≤ w }` and
  `Outside(w) = { p ∉ F : dist(p, path) ≤ w }`. An inside stroke never
  leaves the fill and an outside stroke never enters it at any width, and a
  width beyond the local thickness **saturates** — the shape fills solid
  instead of the band folding over itself and punching hollow pockets.
  Implemented by pruning the raw offset (cut at every intersection, discard
  pieces nearer than `w` or on the wrong side of the fill, restitch), which
  replaces the earlier effective-width clamp.
- Self-intersecting contours are split at their crossings, so each lobe of a
  bowtie or figure-eight resolves its side against the fill independently
  and overlapping bands add instead of cancelling.
- Faster predicates: the source is flattened once into a `y`-bucketed edge
  index, so distance and winding queries no longer run a per-curve solve;
  line-involving intersections use kurbo's analytic `PathSeg::intersect_line`.
- One-sided expansion fast path: when the raw offsets have no mutual or
  self intersections (every smooth contour at moderate width), each offset
  loop is classified whole and the cut/prune/stitch machinery is skipped.
  Adjacent segments are domain-trimmed before intersection, so smooth
  joints reject at the bounding-box level instead of recursing to flatness.
  Inside/outside strokes on circles and donuts drop 2–5×, and one-sided
  cost no longer grows as tolerance shrinks.

### Fixed

- Inside/outside strokes on paths mixing open and closed subpaths: the open
  subpaths' geometry took part in the region predicates (distance pruning
  and implicit-closure winding), so at larger widths a nearby open polyline
  carved chunks out of a closed contour's ring and the stitcher bridged the
  holes with chords across the fill (sandbox "Multi-subpath mixed", outside
  weights ≳ 24). The region construction now sees only the closed contours
  that participate in it; open subpaths are banded independently and add by
  winding where they overlap.
- The Boolean between overlapping one-sided bands of different contours is
  now join-aware. Cross-contour validity of a boundary piece used to be a
  distance test against the other sources, which is only correct for round
  joins: it under-pruned inside miter wedges (another contour's arc stubs
  survived inside a corner and stitched into strips across the fill) and
  over-pruned inside bevel notches (the union boundary lost its transition
  pieces and split into disjoint loops, exposing the fill). It is now a
  winding test against the actual raw offset loops, with a coincidence
  guard for loops closer than the resolution. The distance test remains as
  the self-fold guard, restricted to the piece's own contour, and
  outer-side join geometry (bevel chords, miter legs, round fans) is
  tagged at emission and exempt from it.
- Degenerate one-sided widths no longer blow up:
  - a **collapsed offset** (circle stroked inside at exactly `w = r`) was
    shattered by cusp handling into thousands of segments and cut pairwise
    (>120 ms per call); a resolution guard now drops loops smaller than the
    pruning fuzz before cutting (~1 ms, correct saturation);
  - two **coincident offsets** (donut at exactly half its ring thickness)
    stitched ~1.4k fuzz segments into the output; a net-area gate now
    collapses region components thinner than the boundary band everywhere
    into clean saturation.
- Sandbox: the dev server's wasm now builds with `opt-level = 2` and
  `npm run build` uses `wasm-pack --release` (both previously unoptimized
  `--dev`, the dominant cause of sandbox lag).

- `no_std` (`libm`) build: the `y`-bucket lookups in the source index called
  the std-only `f64::floor`/`f64::ceil` inherent methods; they now go through
  the crate's float shims. Caught by the CI `no-std` job.

## 0.1.0 (unpublished)

Initial release, targeting kurbo 0.13.

### Added

- `StrokeStyle` / `StrokeAlignment` / `StrokeSide` / `DashStyle`: Figma's
  stroke panel as a style type — alignment (inside/center/outside), raw side
  override, joins with a **miter angle in degrees**, independent start/end
  caps, dash pattern/offset/**dash cap**.
- `stroke_aligned` / `stroke_aligned_with` (+ `AlignedStrokeCtx` for
  allocation reuse): expand an aligned stroke into a nonzero-rule fill
  outline. One-sided bands keep the source path as their exact shared
  boundary; sharp corners are clipped so inside strokes never leak outside
  the fill (and vice versa).
- Hole-aware per-subpath side resolution (winding probe): holes stroke into
  the ring; reversing winding direction never changes the result.
- Dashing on the centerline with seam-merge on closed contours (smooth
  `dash_offset` animation), pattern sanitization (empty/negative/zero-sum
  patterns render solid instead of panicking or hanging), and zero-length
  dashes rendered as dots (round + oriented square).
- `miter_angle_to_limit` / `miter_limit_to_angle` conversions.
- `From<&StrokeStyle> for kurbo::Stroke` escape hatch (centered
  interpretation, documented lossiness).
- `analyze_subpaths` introspection for editors/debug tooling.
- `no_std` support (`libm` feature), `serde` feature, zero `unsafe`.
- Interactive wasm sandbox (`sandbox/`), headless vello example
  (`examples/vello-demo`), criterion benchmarks.
